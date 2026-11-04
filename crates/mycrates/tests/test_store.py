import io
import os
import threading
import time
import unittest

from mycrates import wirejson
from mycrates.errors import ConflictError, InputError, IntegrityError
from mycrates.migrate import migrate_state
from mycrates.model import IndexEntry
from mycrates.store import NativeRecord, Store, UpstreamRecord

from .support import ScratchTestCase, sha256


def entry(name="alpha", vers="1.0.0", cksum="c" * 64, yanked=False):
    return IndexEntry(name=name, vers=vers, deps=[], cksum=cksum, features={}, yanked=yanked, v=1)


def upstream(name="serde", vers="1.0.0", checksum="d" * 64):
    return UpstreamRecord(name, vers, checksum, "{}", 0)


class BlobTest(ScratchTestCase):
    def setUp(self):
        super().setUp()
        self.store = Store(self.tmp / "state")
        self.store.ensure_dirs()

    def test_ingest_is_content_addressed_and_idempotent(self):
        first = self.store.ingest_blob(io.BytesIO(b"payload"))
        second = self.store.ingest_blob(io.BytesIO(b"payload"))
        self.assertEqual(first, second)
        self.assertEqual(first, sha256(b"payload"))
        self.assertEqual(self.store.blob_path(first).read_bytes(), b"payload")
        self.assertEqual([p.name for p in self.store.blobs_dir.iterdir()], [f"{first}.crate"])

    def test_a_staged_file_that_changed_since_it_was_hashed_is_refused(self):
        staged = self.tmp / "staged"
        staged.write_bytes(b"one")
        digest = sha256(b"one")
        staged.write_bytes(b"two")
        with self.assertRaisesRegex(IntegrityError, "changed before ingest"):
            self.store.ingest_staged_file(staged, digest)
        self.assertEqual(list(self.store.blobs_dir.iterdir()), [])

    def test_verify_detects_a_corrupted_blob(self):
        checksum = self.store.ingest_blob(io.BytesIO(b"good"))
        self.store.verify()
        self.store.blob_path(checksum).write_bytes(b"bad!")
        with self.assertRaisesRegex(IntegrityError, "corrupted blob"):
            self.store.verify()

    def test_verify_requires_a_blob_for_every_record(self):
        self.store.put_native_record(NativeRecord(entry(cksum="a" * 64)))
        with self.assertRaisesRegex(IntegrityError, "missing blob for native crate alpha 1.0.0"):
            self.store.verify()
        fresh = Store(self.tmp / "other")
        fresh.ensure_dirs()
        fresh.put_upstream_record(upstream())
        with self.assertRaisesRegex(IntegrityError, "missing blob for upstream crate serde 1.0.0"):
            fresh.verify()

    def test_stale_temporary_blobs_are_removed_under_the_lock(self):
        stray = self.store.blobs_dir / ".tmp.123.456.crate"
        stray.write_bytes(b"partial")
        with self.store.lock_exclusive():
            self.assertFalse(stray.exists())

    def test_uploads_are_reclaimed_only_when_old(self):
        fresh, stale = self.store.uploads_dir / "fresh", self.store.uploads_dir / "stale"
        fresh.write_bytes(b"x")
        stale.write_bytes(b"x")
        old = time.time() - 7200
        os.utime(stale, (old, old))
        with self.store.lock_exclusive():
            pass
        self.assertTrue(fresh.exists())
        self.assertFalse(stale.exists())

    def test_the_store_lock_is_exclusive(self):
        acquired = threading.Event()
        release = threading.Event()
        second_entered = threading.Event()

        def holder():
            with self.store.lock_exclusive():
                acquired.set()
                release.wait(10)

        def waiter():
            with Store(self.store.state_dir).lock_exclusive():
                second_entered.set()

        first = threading.Thread(target=holder)
        first.start()
        self.assertTrue(acquired.wait(10))
        second = threading.Thread(target=waiter)
        second.start()
        self.assertFalse(second_entered.wait(0.3))
        release.set()
        first.join(10)
        self.assertTrue(second_entered.wait(10))
        second.join(10)


class RecordTest(ScratchTestCase):
    def setUp(self):
        super().setUp()
        self.store = Store(self.tmp / "state")
        self.store.ensure_dirs()

    def test_native_record_round_trips_and_a_repeat_is_not_new(self):
        record = NativeRecord(entry())
        self.assertTrue(self.store.put_native_record(record))
        self.assertFalse(self.store.put_native_record(record))
        loaded = self.store.load_native_records()
        self.assertEqual([r.entry.to_obj() for r in loaded], [record.entry.to_obj()])

    def test_the_same_version_with_a_different_checksum_conflicts(self):
        self.store.put_native_record(NativeRecord(entry()))
        with self.assertRaisesRegex(ConflictError, "already exists with different checksum"):
            self.store.put_native_record(NativeRecord(entry(cksum="e" * 64)))

    def test_build_metadata_does_not_make_a_version_distinct(self):
        self.store.put_native_record(NativeRecord(entry(vers="1.0.0+a")))
        with self.assertRaisesRegex(ConflictError, "build metadata ignored"):
            self.store.put_native_record(NativeRecord(entry(vers="1.0.0+b")))
        self.store.put_upstream_record(upstream(vers="1.0.0+a"))
        with self.assertRaisesRegex(ConflictError, "build metadata ignored"):
            self.store.put_upstream_record(upstream(vers="1.0.0+b", checksum="e" * 64))

    def test_names_that_differ_only_by_case_or_separator_collide(self):
        self.store.put_native_record(NativeRecord(entry(name="Foo_bar")))
        self.store.put_native_record(NativeRecord(entry(name="Foo_bar", vers="2.0.0")))
        for other in ["foo-bar", "foo_bar", "FOO_BAR"]:
            with self.subTest(other=other), self.assertRaisesRegex(ConflictError, "name collision"):
                self.store.put_native_record(NativeRecord(entry(name=other)))
        self.store.put_upstream_record(upstream(name="Serde_Json"))
        with self.assertRaisesRegex(ConflictError, "name collision"):
            self.store.put_upstream_record(upstream(name="serde-json", vers="2.0.0"))

    def test_yanking_rewrites_the_record_and_unknown_versions_are_inputs_errors(self):
        self.store.put_native_record(NativeRecord(entry()))
        self.store.set_native_yank("alpha", "1.0.0", True)
        self.assertTrue(self.store.load_native_records()[0].entry.yanked)
        self.store.set_native_yank("alpha", "1.0.0", False)
        self.assertFalse(self.store.load_native_records()[0].entry.yanked)
        with self.assertRaisesRegex(InputError, "not found"):
            self.store.set_native_yank("alpha", "9.9.9", True)

    def test_a_record_that_disagrees_with_its_entry_fails_to_load(self):
        self.store.put_native_record(NativeRecord(entry()))
        path = self.store.native_dir / "alpha" / "1.0.0.json"
        document = wirejson.loads(path.read_text())
        document["yanked"] = True
        path.write_text(wirejson.pretty(document))
        with self.assertRaisesRegex(IntegrityError, "disagrees with its index entry on yanked"):
            self.store.load_native_records()

    def test_an_upstream_record_in_the_wrong_directory_is_rejected(self):
        self.store.put_upstream_record(upstream())
        (self.store.upstream_dir / "serde").rename(self.store.upstream_dir / "other")
        with self.assertRaisesRegex(IntegrityError, "wrong directory"):
            self.store.load_upstream_records()


class LegacyMigrationTest(ScratchTestCase):
    NATIVE_TOML = (
        'name = "alpha"\nvers = "0.2.0"\nchecksum = "{c}"\nyanked = true\n\n[entry]\nname = "alpha"\nvers = "0.2.0"\n'
        'cksum = "{c}"\nyanked = true\nv = 2\nlinks = "alpha-sys"\n\n[entry.features]\nstd = ["dep:serde"]\n\n'
        '[[entry.deps]]\nname = "serde"\nreq = "^1"\nfeatures = ["std"]\noptional = true\ndefault_features = false\n'
        'kind = "dev"\ntarget = "cfg(unix)"\npackage = "serde_json"\n'
    )

    def setUp(self):
        super().setUp()
        self.state = self.tmp / "state"
        self.store = Store(self.state)
        self.store.ensure_dirs()
        (self.state / "native" / "alpha").mkdir()
        (self.state / "native" / "alpha" / "0.2.0.toml").write_text(self.NATIVE_TOML.format(c="a" * 64))
        (self.state / "pins" / "proj.toml").write_text(
            'project = "proj"\nunsupported = ["git: x 1.0.0"]\n\n[[registry]]\nname = "itoa"\nversion = "1.0.0"\n'
            f'checksum = "{"b" * 64}"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n'
        )

    def test_legacy_records_are_refused_until_migrated(self):
        with self.assertRaisesRegex(InputError, "migrate-state"):
            self.store.load_native_records()
        with self.assertRaisesRegex(InputError, "migrate-state"):
            self.store.put_native_record(NativeRecord(entry(name="beta")))

    def test_adoption_refuses_before_ingesting_anything_into_an_unmigrated_state(self):
        from mycrates.adopt import adopt_site

        from .support import GOLDEN

        with self.assertRaisesRegex(InputError, "migrate-state"):
            adopt_site(self.store, GOLDEN / "adopt-publish" / "input-site")
        self.assertEqual(list(self.store.blobs_dir.iterdir()), [])

    def test_migration_preserves_every_field_and_is_idempotent(self):
        summary = migrate_state(self.state)
        self.assertEqual((summary.native, summary.pins), (1, 1))
        record = Store(self.state).load_native_records()[0].entry
        self.assertEqual(record.index_line(), (
            '{"name":"alpha","vers":"0.2.0","deps":[{"name":"serde","req":"^1","features":["std"],"optional":true,'
            '"default_features":false,"target":"cfg(unix)","kind":"dev","registry":null,"package":"serde_json"}],'
            f'"cksum":"{"a" * 64}","features":{{"std":["dep:serde"]}},"yanked":true,"links":"alpha-sys","v":2}}'
        ))
        self.assertEqual(list(self.state.glob("**/*.toml")), [])
        pins = wirejson.loads((self.state / "pins" / "proj.json").read_text())
        self.assertEqual(pins["registry"][0]["name"], "itoa")
        self.assertEqual(pins["unsupported"], ["git: x 1.0.0"])
        self.assertEqual(migrate_state(self.state).native, 0)

    def test_a_legacy_record_that_disagrees_with_itself_is_not_migrated(self):
        path = self.state / "native" / "alpha" / "0.2.0.toml"
        path.write_text(path.read_text().replace('yanked = true\n\n[entry]', 'yanked = false\n\n[entry]', 1))
        with self.assertRaisesRegex(IntegrityError, "disagrees"):
            migrate_state(self.state)
        self.assertTrue(path.exists())


if __name__ == "__main__":
    unittest.main()
