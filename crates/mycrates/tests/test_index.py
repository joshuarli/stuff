import json

from mycrates.crate_file import parse_packaged_manifest
from mycrates.errors import InputError, IntegrityError
from mycrates.index import mirror_config_json, native_config_json, publish_to_index
from mycrates.model import IndexEntry, PublishBody
from mycrates.names import shard_for_name, validate_crate_name

from .support import AssertMixin, dependency, publish_metadata

CRATES_IO = "https://github.com/rust-lang/crates.io-index"


def body(deps=(), features=None, **extra):
    return PublishBody.from_obj(publish_metadata("foo", "1.0.0", deps, features, checksum="ab" * 32, **extra))


def packaged(extra="", package_extra=""):
    return parse_packaged_manifest(f'[package]\nname = "foo"\nversion = "1.0.0"\n{package_extra}{extra}')


def line_of(published, manifest=None):
    return publish_to_index(published, manifest).index_line()


class EntrySerializationTest(AssertMixin):
    def test_same_registry_dependencies_serialize_a_null_registry_and_crates_io_is_verbatim(self):
        line = line_of(body([dependency("bar", registry=None), dependency("itoa", registry=CRATES_IO)]))
        deps = json.loads(line)["deps"]
        self.assertIsNone(deps[0]["registry"])
        self.assertEqual(deps[1]["registry"], CRATES_IO)

    def test_a_renamed_dependency_is_indexed_under_its_alias_with_the_real_name_as_package(self):
        line = line_of(body([dependency("serde_json", explicit_name_in_toml="json")]))
        dep = json.loads(line)["deps"][0]
        self.assertEqual((dep["name"], dep["package"]), ("json", "serde_json"))

    def test_v2_feature_syntax_selects_index_format_2(self):
        self.assertEqual(json.loads(line_of(body(features={"default": ["dep:bar"]})))["v"], 2)
        self.assertEqual(json.loads(line_of(body(features={"default": ["bar/std"]})))["v"], 1)
        self.assertEqual(json.loads(line_of(body(features={"x": ["bar?/std"]})))["v"], 2)

    def test_optional_fields_appear_only_when_present_and_in_contract_order(self):
        plain = json.loads(line_of(body()))
        self.assertEqual(list(plain), ["name", "vers", "deps", "cksum", "features", "yanked", "v"])
        full = line_of(body(links="foo", rust_version="1.70"))
        self.assertEqual(list(json.loads(full)), ["name", "vers", "deps", "cksum", "features", "yanked", "links", "rust_version", "v"])
        self.assertNotIn(" ", full)

    def test_unknown_fields_of_an_adopted_entry_survive_in_sorted_order(self):
        text = '{"name":"serde","vers":"1.0.0","deps":[],"cksum":"x","features":{},"yanked":false,"pubtime":12345,"alpha":[1]}'
        entry = IndexEntry.from_obj(json.loads(text))
        self.assertEqual(entry.index_line(), text.replace('"pubtime":12345,"alpha":[1]', '"alpha":[1],"pubtime":12345'))

    def test_strings_escape_only_what_the_contract_names(self):
        entry = IndexEntry.from_obj({"name": "n", "vers": "1.0.0", "deps": [], "cksum": "c", "features": {"f\u0008\u000c\u007fé": ["\"\\\n\t\u0001"]}})
        self.assertEqual(
            entry.index_line(),
            '{"name":"n","vers":"1.0.0","deps":[],"cksum":"c","features":{"f\\u0008\\u000c\u007fé":["\\"\\\\\\n\\t\\u0001"]},"yanked":false}',
        )

    def test_sharding_matches_the_crates_io_sparse_layout(self):
        self.assertEqual(
            [shard_for_name(n) for n in ["a", "ab", "abc", "abcd", "serde_json"]],
            ["1/a", "2/ab", "3/a/abc", "ab/cd/abcd", "se/rd/serde_json"],
        )
        self.assertIsNone(shard_for_name("café"))

    def test_config_documents(self):
        native = json.loads(native_config_json("https://example.com/base/", True))
        self.assertEqual(native, {"dl": "https://example.com/base/blobs/{sha256-checksum}.crate", "api": "https://example.com/base/", "auth-required": True})
        self.assertEqual(json.loads(native_config_json("https://example.com", False))["auth-required"], False)
        self.assertEqual(list(json.loads(mirror_config_json("https://example.com/base"))), ["dl"])

    def test_malformed_metadata_is_rejected_as_input_errors(self):
        for deps in [
            [dependency("bad dep!", "1.0")],
            [dependency("good", "not-a-req!!!")],
            [dependency("good", "")],
            [dependency(None)],
            [dependency("good", explicit_name_in_toml="bad alias")],
            [dependency("good", package="bad pkg")],
        ]:
            with self.subTest(deps=deps), self.assertRaises(InputError):
                publish_to_index(body(deps))
        with self.assertRaises(InputError):
            publish_to_index(PublishBody.from_obj(publish_metadata("1bad", "1.0.0", checksum="x")))
        with self.assertRaises(InputError):
            publish_to_index(PublishBody.from_obj(publish_metadata("ok", "1.0", checksum="x")))

    def test_publish_metadata_types_are_checked(self):
        for broken in [
            {"name": "a", "vers": "1.0.0"},
            {"name": 1, "vers": "1.0.0", "deps": []},
            {"name": "a", "vers": "1.0.0", "deps": {}},
            {"name": "a", "vers": "1.0.0", "deps": [], "features": {"f": "x"}},
            {"name": "a", "vers": "1.0.0", "deps": [{"name": "b", "version_req": 1}]},
        ]:
            with self.subTest(broken=broken), self.assertRaises(InputError):
                PublishBody.from_obj(broken)

    def test_crate_names_follow_cargo_rules(self):
        for name in ["a", "serde_json", "My-Crate2", "x" * 64]:
            validate_crate_name(name)
        for name in ["", "1a", "_a", "a b", "a/b", "x" * 65, "café", "a.b"]:
            with self.subTest(name=name), self.assertRaises(InputError):
                validate_crate_name(name)


class ReconcileTest(AssertMixin):
    def accepts(self, published, manifest):
        publish_to_index(published, manifest)

    def rejects(self, published, manifest):
        with self.assertRaises(IntegrityError):
            publish_to_index(published, manifest)

    def test_public_unrenamed_and_renamed_dependencies_match_their_manifest(self):
        self.accepts(body([dependency("serde", "^1.0", registry=CRATES_IO)]), packaged('[dependencies]\nserde = "1.0"\n'))
        self.accepts(
            body([dependency("serde_json", "^1", explicit_name_in_toml="json", registry=CRATES_IO)]),
            packaged('[dependencies]\njson = { package = "serde_json", version = "1" }\n'),
        )

    def test_same_registry_dependency_without_registry_key_matches(self):
        self.accepts(body([dependency("native")]), packaged('[dependencies]\nnative = { version = "1.0", registry = "mine" }\n'))

    def test_versioned_dev_dependency_and_same_crate_in_two_kinds(self):
        self.accepts(body([dependency("t", "^3", kind="dev", registry=CRATES_IO)]), packaged('[dev-dependencies]\nt = "3"\n'))
        self.accepts(
            body([dependency("cc", "^1", registry=CRATES_IO), dependency("cc", "^1", kind="build", registry=CRATES_IO)]),
            packaged('[dependencies]\ncc = "1"\n[build-dependencies]\ncc = "1"\n'),
        )

    def test_same_alias_in_two_target_sections(self):
        published = body(
            [dependency("w", "^0.3", target="cfg(windows)", registry=CRATES_IO), dependency("w", "^0.3", target="cfg(unix)", registry=CRATES_IO)]
        )
        self.accepts(
            published,
            packaged("[target.'cfg(windows)'.dependencies]\nw = \"0.3\"\n[target.'cfg(unix)'.dependencies]\nw = \"0.3\"\n"),
        )

    def test_whitespace_in_target_cfg_is_insignificant(self):
        self.accepts(
            body([dependency("w", "^0.3", target="cfg(any(unix, windows))", registry=CRATES_IO)]),
            packaged("[target.'cfg(any(unix,windows))'.dependencies]\nw = \"0.3\"\n"),
        )

    def test_features_compare_as_sets(self):
        self.accepts(body(features={"a": ["x", "y"]}), packaged('[features]\na = ["y", "x"]\n'))
        self.rejects(body(features={"a": ["x"]}), packaged('[features]\na = ["y"]\n'))
        self.rejects(body(features={}), packaged("[features]\nextra = []\n"))

    def test_divergences_are_integrity_errors(self):
        manifest = packaged('[dependencies]\nbar = "1.0"\n', 'links = "native"\nrust-version = "1.70"\n')
        for published in [
            body(links="other", rust_version="1.70"),
            body(links="native", rust_version="1.71"),
            body([], links="native", rust_version="1.70"),
            body([dependency("bar", "^1.0", kind="dev")], links="native", rust_version="1.70"),
            body([dependency("bar", "^1.0", optional=True)], links="native", rust_version="1.70"),
            body([dependency("bar", "^1.0", features=["f"])], links="native", rust_version="1.70"),
            body([dependency("bar", "^1.0", default_features=False)], links="native", rust_version="1.70"),
            body([dependency("bar", "^2.0")], links="native", rust_version="1.70"),
            body([dependency("bar", "^1.0", registry="https://other.example/index")], links="native", rust_version="1.70"),
        ]:
            with self.subTest(published=published):
                self.rejects(published, manifest)
        self.accepts(body([dependency("bar", "^1.0")], links="native", rust_version="1.70"), manifest)

    def test_packaged_name_and_version_must_match_the_metadata(self):
        other = parse_packaged_manifest('[package]\nname = "bar"\nversion = "1.0.0"\n')
        self.rejects(body(), other)


