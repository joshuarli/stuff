import pytest
import io
import json
import os
import shutil
import subprocess

from mycrates.errors import InputError, IntegrityError, NetworkError
from mycrates.lockfile import classify, parse_lockfile
from mycrates.pins import PinEntry, Pins, write_pins
from mycrates.publish import Publisher
from mycrates.store import Store, UpstreamRecord
from mycrates.validate import validate_archive, validate_project

from .support import ScratchTestCase, init_state, make_crate, sha256, start_server

CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"


def seed_upstream(store, name, version, deps=(), yanked=False):
    manifest = "[dependencies]\n" + "".join(f'{dep} = "={req}"\n' for dep, req in deps) if deps else ""
    data = make_crate(name, version, "\n" + manifest if manifest else "")
    checksum = store.ingest_blob(io.BytesIO(data))
    line = json.dumps(
        {
            "name": name, "vers": version, "cksum": checksum, "features": {}, "yanked": yanked,
            "deps": [{"name": d, "req": f"={r}", "features": [], "optional": False, "default_features": True, "target": None, "kind": "normal"} for d, r in deps],
        },
        separators=(",", ":"),
    )
    store.put_upstream_record(UpstreamRecord(name, version, checksum, line, 1000))
    return checksum


def environment(home):
    env = {k: v for k, v in os.environ.items() if not k.startswith("CARGO_HTTP") and k != "RUSTC_WRAPPER"}
    env.update(CARGO_HOME=str(home), CARGO_TERM_COLOR="never", HTTPS_PROXY="http://127.0.0.1:1", NO_PROXY="127.0.0.1,localhost")
    return env


@pytest.mark.skipif(not (shutil.which("cargo")), reason="validate shells out to cargo")
class ValidateTest(ScratchTestCase):
    def setup_method(self, method=None):
        super().setup_method(method)
        self.state, self.site = init_state(self.tmp)
        self.store = Store(self.state)
        self.running = start_server(self.state, self.site, public_base_url="local")
        self.addCleanup(self.running.close)
        self.mirror = f"{self.running.url}/current/crates-io/"

    def republish(self):
        with self.store.lock_exclusive():
            Publisher(self.store, self.site, self.running.url).publish()

    def project(self, name, dependencies, extra=""):
        directory = self.tmp / name
        (directory / "src").mkdir(parents=True)
        (directory / "Cargo.toml").write_text(
            f'[package]\nname = "{name}"\nversion = "1.2.3"\nedition = "2021"\n\n[dependencies]\n{dependencies}{extra}'
        )
        (directory / "src" / "lib.rs").write_text("pub fn f() {}\n")
        return directory

    def lock(self, directory):
        home = self.tmp / "cargo-home"
        home.mkdir(exist_ok=True)
        (home / "config.toml").write_text(
            f'[source.crates-io]\nreplace-with = "m"\n\n[source.m]\nregistry = "sparse+{self.mirror}"\n'
        )
        result = subprocess.run(["cargo", "generate-lockfile"], cwd=directory, env=environment(home), capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return (directory / "Cargo.lock").read_text()

    def pins_for(self, name, directory, lock_text):
        registry = [
            PinEntry(p.name, p.version, p.checksum, p.source)
            for p in parse_lockfile(lock_text).packages
            if p.source and p.source.startswith("registry+")
        ]
        path = self.store.pins_dir / f"{name}.json"
        write_pins(path, Pins(name, str(directory / "Cargo.lock"), lock_text, registry, []))
        return path, registry

    def test_a_pinned_project_resolves_and_fetches_offline_from_the_archive_without_touching_its_lock(self):
        checksum = seed_upstream(self.store, "archived-dep", "0.1.0")
        self.republish()
        directory = self.project("validation-probe", 'archived-dep = "0.1.0"\n')
        lock_text = self.lock(directory)
        self.assertIn(checksum, lock_text)
        path, registry = self.pins_for("sample-proj", directory, lock_text)
        result = validate_project(self.state, path, self.mirror, directory)
        self.assertEqual((result.project, result.registry_count, result.is_partial), ("sample-proj", 1, False))
        self.assertEqual((directory / "Cargo.lock").read_text(), lock_text)

    def test_a_workspace_with_duplicate_transitive_versions_and_target_dependencies(self):
        seed_upstream(self.store, "dup-dep", "1.0.0")
        seed_upstream(self.store, "dup-dep", "2.0.0")
        seed_upstream(self.store, "mid-a", "1.0.0", [("dup-dep", "1.0.0")])
        seed_upstream(self.store, "mid-b", "1.0.0", [("dup-dep", "2.0.0")])
        seed_upstream(self.store, "win-dep", "1.0.0")
        seed_upstream(self.store, "trans-leaf", "0.5.0")
        self.republish()
        directory = self.project(
            "workspace-root", 'mid-a = "1.0"\nmid-b = "1.0"\ntrans-leaf = "0.5"\n', "\n[target.'cfg(windows)'.dependencies]\nwin-dep = \"1.0\"\n"
        )
        lock_text = self.lock(directory)
        self.assertEqual(lock_text.count('name = "dup-dep"'), 2)
        path, registry = self.pins_for("real-workspace", directory, lock_text)
        self.assertGreaterEqual(len(registry), 5)
        result = validate_project(self.state, path, self.mirror, directory)
        self.assertEqual(result.registry_count, len(registry))
        self.assertEqual((directory / "Cargo.lock").read_text(), lock_text)

    def test_a_missing_project_directory_is_named_but_the_archive_itself_still_validates(self):
        checksum = seed_upstream(self.store, "some-dep", "1.0.0")
        lock_text = f'version = 3\n\n[[package]]\nname = "some-dep"\nversion = "1.0.0"\nsource = "{CRATES_IO}"\nchecksum = "{checksum}"\n'
        path = self.store.pins_dir / "needs-dir.json"
        write_pins(path, Pins("needs-dir", "Cargo.lock", lock_text, [PinEntry("some-dep", "1.0.0", checksum, CRATES_IO)], []))
        with self.assertRaisesRegex(InputError, "--project-dir"):
            validate_project(self.state, path, self.mirror, None)
        self.assertEqual(validate_archive(self.state, path).registry_count, 1)
        with self.assertRaisesRegex(InputError, "not found"):
            validate_project(self.state, path, self.mirror, self.tmp / "absent")

    def test_an_incomplete_archive_names_the_missing_package(self):
        path = self.store.pins_dir / "incomplete.json"
        write_pins(path, Pins("incomplete", None, None, [PinEntry("missing-crate-xyz", "2.3.4", "a" * 64, CRATES_IO)], []))
        with self.assertRaises(IntegrityError) as caught:
            validate_project(self.state, path, self.mirror, None)
        for fragment in ("incomplete archive", "missing-crate-xyz", "2.3.4"):
            self.assertIn(fragment, str(caught.exception))

    def test_a_locked_fetch_the_mirror_cannot_satisfy_is_a_network_failure(self):
        checksum = seed_upstream(self.store, "some-dep", "1.0.0")
        directory = self.project("root-pkg", 'some-dep = "1.0.0"\n')
        lock_text = (
            f'version = 3\n\n[[package]]\nname = "some-dep"\nversion = "1.0.0"\nsource = "{CRATES_IO}"\nchecksum = "{checksum}"\n\n'
            '[[package]]\nname = "root-pkg"\nversion = "1.2.3"\ndependencies = [\n "some-dep",\n]\n'
        )
        (directory / "Cargo.lock").write_text(lock_text)
        path, _ = self.pins_for("network-probe", directory, lock_text)
        with self.assertRaises(NetworkError):
            validate_project(self.state, path, self.mirror, directory)

    def test_the_archive_must_agree_with_the_pins_and_the_lockfile(self):
        checksum = seed_upstream(self.store, "some-dep", "1.0.0")
        lock_text = f'version = 3\n\n[[package]]\nname = "some-dep"\nversion = "1.0.0"\nsource = "{CRATES_IO}"\nchecksum = "{checksum}"\n'
        pin = PinEntry("some-dep", "1.0.0", checksum, CRATES_IO)
        path = self.store.pins_dir / "p.json"

        def check(pins):
            write_pins(path, pins)
            with self.assertRaises(IntegrityError) as caught:
                validate_archive(self.state, path)
            return str(caught.exception)

        other = seed_upstream(self.store, "other", "1.0.0")
        self.assertIn("archive checksum changed", check(Pins("p", None, lock_text, [PinEntry("some-dep", "1.0.0", other, CRATES_IO)], [])))
        different_lock = lock_text.replace(checksum, other)
        self.assertIn("pin checksum changed", check(Pins("p", None, different_lock, [pin], [])))
        self.assertIn("not in pinned archive", check(Pins("p", None, lock_text + f'\n[[package]]\nname = "extra"\nversion = "1.0.0"\nsource = "{CRATES_IO}"\nchecksum = "{other}"\n', [pin], [])))
        self.assertIn("missing from lockfile", check(Pins("p", None, "version = 3\n", [pin], [])))
        record = self.store.upstream_dir / "some-dep" / "1.0.0.json"
        document = json.loads(record.read_text())
        document["raw_index_line"] = document["raw_index_line"].replace('"yanked":false', '"yanked":true')
        record.write_text(json.dumps(document))
        self.assertIn("yanked upstream", check(Pins("p", None, lock_text, [pin], [])))

    def test_a_project_lock_that_drifted_from_the_imported_one_is_refused(self):
        seed_upstream(self.store, "some-dep", "1.0.0")
        self.republish()
        directory = self.project("drift", 'some-dep = "1.0.0"\n')
        lock_text = self.lock(directory)
        path, _ = self.pins_for("drift", directory, lock_text)
        (directory / "Cargo.lock").write_text(lock_text + "\n# edited\n")
        with self.assertRaisesRegex(IntegrityError, "differs from imported"):
            validate_project(self.state, path, self.mirror, directory)
        (directory / "Cargo.lock").write_text(lock_text)
        os.symlink(self.tmp, directory / "escape")
        with self.assertRaisesRegex(InputError, "symlink"):
            validate_project(self.state, path, self.mirror, directory)

    def test_sources_other_than_crates_io_are_reported_as_partial(self):
        parsed = parse_lockfile(
            'version = 3\n[[package]]\nname = "git-pkg"\nversion = "0.2.0"\nsource = "git+https://github.com/example/repo#1234"\n'
            '[[package]]\nname = "alt"\nversion = "0.3.0"\nsource = "registry+https://other.example/index"\nchecksum = "2"\n'
        )
        self.assertEqual([classify(p) for p in parsed.packages], ["git", "alternate-registry"])


