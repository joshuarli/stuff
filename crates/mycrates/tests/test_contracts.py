import hashlib
import importlib.util
import json
import os
import sys
import unittest

from mycrates.names import shard_for_name
from mycrates.semver import Version

from .support import GOLDEN, PACKAGE_ROOT, ScratchTestCase

DEPLOY = PACKAGE_ROOT.parents[1] / "deploy"
GENERATION = GOLDEN / "adopt-publish" / "expected-generation"
KINDS = ["runtime", "peer", "build", "dev"]
NATIVE_KEYS = ["name", "vers", "deps", "cksum", "features", "yanked"]
DEPENDENCY_KEYS = ["name", "req", "features", "optional", "default_features", "target", "kind", "registry", "package"]


def parse_catalog(text):
    """The reader contract of crates/stuff-catalog for schema 4 cargo catalogs."""
    document = json.loads(text)
    assert list(document) == ["schema_version", "registry", "generation", "packages"], list(document)
    assert document["schema_version"] == 4 and document["registry"] == "cargo"
    assert isinstance(document["generation"], str) and document["generation"]
    identities = []
    for package in document["packages"]:
        assert list(package) == ["name", "source", "latest", "versions", "dependencies"], list(package)
        assert package["name"] and package["source"] in ("native", "upstream")
        versions = package["versions"]
        assert versions, "package.versions must not be empty"
        names = [v["version"] for v in versions]
        assert all(list(v) == ["version", "yanked"] and isinstance(v["yanked"], bool) for v in versions)
        assert len(set(names)) == len(names), "duplicate version"
        assert package["latest"] in names
        assert list(package["dependencies"]) == KINDS
        for kind in KINDS:
            listed = package["dependencies"][kind]
            assert all(isinstance(n, str) and n and "\0" not in n for n in listed)
            assert listed == sorted(set(listed)), f"{package['name']} {kind} must be sorted and unique"
        identities.append((package["name"], package["source"]))
    assert identities == sorted(identities) and len(set(identities)) == len(identities)
    return document


def read_guard():
    sys.path.insert(0, str(DEPLOY))
    try:
        spec = importlib.util.spec_from_file_location("r2_catalog_guard", DEPLOY / "r2-catalog-guard.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
    finally:
        sys.path.remove(str(DEPLOY))
    return module


def file_sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


class GoldenContractTest(unittest.TestCase):
    def test_the_catalog_satisfies_the_reader_contract(self):
        catalog = parse_catalog((GENERATION / "catalog.json").read_text())
        by_name = {(p["name"], p["source"]): p for p in catalog["packages"]}
        alpha = by_name[("alpha", "native")]
        self.assertEqual(alpha["latest"], "0.2.0")
        self.assertEqual(
            alpha["dependencies"],
            {"runtime": ["serde_json"], "peer": [], "build": ["cc"], "dev": ["tempfile"]},
            "kinds are kept apart and renames resolve to the real crate",
        )
        self.assertEqual(by_name[("gamma", "native")]["latest"], "0.1.0", "a package with only yanked versions still has a latest")
        serde = by_name[("serde", "upstream")]
        self.assertEqual((serde["latest"], serde["versions"][1]["yanked"]), ("1.0.0", True), "a yanked newer version is not latest")
        self.assertEqual(by_name[("Beta_crate", "native")]["latest"], "1.0.0-rc.1", "prereleases win when nothing else exists")
        self.assertEqual(by_name[("serde_json", "upstream")]["dependencies"]["runtime"], ["itoa"])

    def test_the_catalog_text_is_the_fixed_format_every_consumer_assumes(self):
        text = (GENERATION / "catalog.json").read_text()
        self.assertTrue(text.startswith('{\n  "schema_version": 4,\n  "registry": "cargo",\n  "generation": "'))
        self.assertTrue(text.endswith("  ]\n}\n"))
        self.assertIn('        {"version": "0.1.0", "yanked": false},\n', text)
        self.assertIn('"dependencies": {"runtime": [], "peer": [], "build": [], "dev": []}\n    }', text)

    def test_the_generation_id_follows_the_documented_derivation(self):
        generation = (GOLDEN / "adopt-publish" / "expected-generation-id").read_text().strip()

        def sealed(name):
            return (GENERATION / name).read_text().replace(generation, "fingerprint")

        fingerprint = hashlib.sha256(
            "\0".join([sealed("catalog.json"), sealed("index.html"), sealed("llms.txt")]).encode()
        ).hexdigest()
        lines = []
        for view in ("crates-io", "registry"):
            for path in sorted((GENERATION / view).rglob("*")):
                if path.is_file():
                    lines.append((view, path.relative_to(GENERATION / view).as_posix(), file_sha256(path)))
        canonical = f"v:4\ncatalog:{fingerprint}\n" + "".join(f"{v}|{p}|{h}\n" for v, p, h in sorted(lines))
        self.assertEqual(hashlib.sha256(canonical.encode()).hexdigest()[:24], generation)

    def test_every_index_line_follows_the_entry_contract(self):
        checked = 0
        for view in ("registry", "crates-io"):
            for path in sorted((GENERATION / view).rglob("*")):
                if not path.is_file() or path.name == "config.json":
                    continue
                relative = path.relative_to(GENERATION / view).as_posix()
                text = path.read_text()
                self.assertTrue(text.endswith("\n") and "\n\n" not in text)
                versions = []
                for line in text.splitlines():
                    entry = json.loads(line)
                    self.assertEqual(shard_for_name(entry["name"].lower()), relative)
                    self.assertEqual(len(entry["cksum"]), 64)
                    self.assertTrue((GOLDEN / "adopt-publish/input-site/blobs" / f"{entry['cksum']}.crate").exists())
                    versions.append(Version.parse(entry["vers"]).precedence_key())
                    if view == "registry":
                        keys = list(entry)
                        self.assertEqual(keys[: len(NATIVE_KEYS)], NATIVE_KEYS)
                        self.assertEqual(entry["features"], dict(sorted(entry["features"].items())))
                        self.assertIn(entry["v"], (1, 2))
                        for dep in entry["deps"]:
                            self.assertEqual(list(dep), [k for k in DEPENDENCY_KEYS if k in dep])
                            self.assertIn("registry", dep)
                    checked += 1
                self.assertEqual(versions, sorted(versions), f"{relative} is not in version order")
        self.assertEqual(checked, 10)

    def test_config_documents(self):
        native = json.loads((GENERATION / "registry/config.json").read_text())
        self.assertEqual(list(native), ["dl", "api", "auth-required"])
        self.assertEqual(native["dl"], "https://crates.example.test/blobs/{sha256-checksum}.crate")
        mirror = json.loads((GENERATION / "crates-io/config.json").read_text())
        self.assertEqual(list(mirror), ["dl"])
        self.assertEqual(mirror["dl"], native["dl"])

    def test_the_search_script_is_embedded_once_and_the_page_is_self_contained(self):
        html = (GENERATION / "index.html").read_text()
        script = (PACKAGE_ROOT.parent / "stuff-catalog/assets/search.js").read_text()
        self.assertEqual(html.count("<script>"), 1)
        self.assertIn(f"<script>{script}</script>", html)
        self.assertNotIn("</script", script.lower())
        generation = (GOLDEN / "adopt-publish/expected-generation-id").read_text().strip()
        self.assertIn(f'<a href="/generations/{generation}/catalog.json">catalog.json</a>', html)

    def test_llms_txt_pins_the_generation_and_names_the_origin(self):
        text = (GENERATION / "llms.txt").read_text()
        generation = (GOLDEN / "adopt-publish/expected-generation-id").read_text().strip()
        self.assertTrue(text.startswith("# Cargo registry\n> Static snapshot for LLMs. Generation `" + generation + "`: 8 packages, 10 versions."))
        self.assertIn("- Origin: https://crates.example.test\n", text)
        self.assertIn('registry = "sparse+https://crates.example.test/current/registry/"', text)
        self.assertIn("- `serde` — latest `1.0.0` (2 versions): `1.0.0`, `1.0.1` (yanked) [upstream]\n", text)
        self.assertTrue(text.endswith("\n\n"))

    def test_the_promotion_guard_reads_versions_straight_from_the_catalog(self):
        guard = read_guard()
        found = guard.versions((GENERATION / "catalog.json").read_text(), "golden")
        self.assertIn(("native", "alpha", "0.2.0"), found)
        self.assertIn(("upstream", "serde", "1.0.1"), found)
        self.assertEqual(len(found), 10)


class GeneratedArtifactContractTest(ScratchTestCase):
    def test_a_site_built_from_scratch_satisfies_the_same_contracts(self):
        from mycrates.publish import Publisher
        from mycrates.store import Store

        from .support import cli, init_state

        state, site = init_state(self.tmp, "https://crates.example.test")
        cli("adopt", "--from-site", GOLDEN / "adopt-publish/input-site", "--state-dir", state)
        cli("publish", "--state-dir", state, "--site-dir", site)
        generation = os.readlink(site / "current").split("/")[-1]
        catalog = parse_catalog((site / "generations" / generation / "catalog.json").read_text())
        self.assertEqual(catalog["generation"], generation)
        self.assertEqual(os.readlink(site / "current"), f"generations/{generation}")
        for blob in (site / "blobs").iterdir():
            self.assertEqual(blob.name, f"{file_sha256(blob)}.crate")
        self.assertEqual(Publisher(Store(state), site, "https://crates.example.test").publish().is_noop, True)


if __name__ == "__main__":
    unittest.main()
