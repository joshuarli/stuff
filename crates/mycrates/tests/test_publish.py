import io
import json
import os
import shutil
import unittest

from mycrates.errors import InputError, IntegrityError
from mycrates.model import IndexEntry
from mycrates.names import shard_for_name
from mycrates.publish import Publisher
from mycrates.site import Site, sync_immutable_file
from mycrates.store import NativeRecord, Store

from .support import ScratchTestCase, cli, init_state, sha256

BASE = "http://localhost:8080"


def put_native(store, name, version, data, **fields):
    checksum = store.ingest_blob(io.BytesIO(data))
    store.put_native_record(
        NativeRecord(IndexEntry(name=name, vers=version, deps=[], cksum=checksum, features={}, yanked=False, v=1, **fields))
    )
    return checksum


class GenerationTest(ScratchTestCase):
    def setUp(self):
        super().setUp()
        self.store = Store(self.tmp / "state")
        self.store.ensure_dirs()
        self.site = self.tmp / "site"
        self.publisher = Publisher(self.store, self.site, BASE)

    def test_identical_inputs_give_identical_generations_and_a_repeat_is_a_noop(self):
        put_native(self.store, "foo", "1.0.0", b"sample-crate-tarball-bytes")
        first = self.publisher.publish()
        other = Store(self.tmp / "other-state")
        other.ensure_dirs()
        put_native(other, "foo", "1.0.0", b"sample-crate-tarball-bytes")
        second = Publisher(other, self.tmp / "other-site", BASE).publish()
        self.assertEqual(first.generation_id, second.generation_id)
        self.assertFalse(first.is_noop)
        again = self.publisher.publish()
        self.assertTrue(again.is_noop)
        self.assertEqual(os.readlink(self.site / "current"), f"generations/{first.generation_id}")

    def test_the_public_base_url_and_auth_flag_change_the_generation(self):
        put_native(self.store, "foo", "1.0.0", b"x")
        base = self.publisher.publish().generation_id
        other_url = Publisher(self.store, self.tmp / "s2", "https://example.test").publish().generation_id
        gated = Publisher(self.store, self.tmp / "s3", BASE, auth_required=True).publish().generation_id
        self.assertEqual(len({base, other_url, gated}), 3)

    def test_config_documents_follow_the_base_url_with_or_without_a_prefix(self):
        for base, expected in [(BASE, BASE), ("https://deps.example.net/mycrates/", "https://deps.example.net/mycrates")]:
            site = self.tmp / f"site-{abs(hash(base))}"
            result = Publisher(self.store, site, base, auth_required=True).publish()
            config = json.loads((site / "generations" / result.generation_id / "registry/config.json").read_text())
            self.assertEqual(config["dl"], f"{expected}/blobs/{{sha256-checksum}}.crate")
            self.assertEqual(config["api"], base)
            self.assertTrue(config["auth-required"])
            mirror = json.loads((site / "generations" / result.generation_id / "crates-io/config.json").read_text())
            self.assertEqual(mirror, {"dl": config["dl"]})

    def test_publishing_two_sources_into_one_state_keeps_both(self):
        put_native(self.store, "first", "1.0.0", b"one")
        self.publisher.publish()
        put_native(self.store, "second", "1.0.0", b"two")
        result = self.publisher.publish()
        registry = self.site / "generations" / result.generation_id / "registry"
        for name in ("first", "second"):
            self.assertTrue((registry / shard_for_name(name)).exists())

    def test_a_missing_blob_stops_publication_before_current_exists(self):
        self.store.put_native_record(
            NativeRecord(IndexEntry(name="bar", vers="0.1.0", deps=[], cksum="e3b0c442" * 8, features={}, v=1))
        )
        with self.assertRaisesRegex(IntegrityError, "missing blob"):
            self.publisher.publish()
        self.assertFalse((self.site / "current").exists())

    def test_a_tampered_state_blob_stops_publication(self):
        checksum = put_native(self.store, "tamper-pkg", "1.0.0", b"original-content")
        self.store.blob_path(checksum).write_bytes(b"modified-bytes")
        with self.assertRaisesRegex(IntegrityError, "tampered blob"):
            self.publisher.publish()
        self.assertFalse((self.site / "current").exists())

    def test_a_damaged_generation_is_never_rewritten_but_can_be_repaired_by_removal(self):
        checksum = put_native(self.store, "repair-pkg", "1.0.0", b"repair-payload")
        first = self.publisher.publish()
        generation = self.site / "generations" / first.generation_id
        index_file = generation / "registry" / shard_for_name("repair-pkg")
        index_file.unlink()
        with self.assertRaisesRegex(IntegrityError, "refusing to replace an immutable generation"):
            self.publisher.publish()
        self.assertFalse(index_file.exists())
        shutil.rmtree(generation)
        repaired = self.publisher.publish()
        self.assertFalse(repaired.is_noop)
        self.assertTrue(index_file.exists())

        os.unlink(self.site / "current")
        os.symlink("generations/nonexistent_gen_repair", self.site / "current")
        healed = self.publisher.publish()
        self.assertFalse(healed.is_noop)
        self.assertTrue((self.site / os.readlink(self.site / "current")).exists())
        self.assertEqual(sha256((self.site / "blobs" / f"{checksum}.crate").read_bytes()), checksum)

    def test_served_blobs_are_never_overwritten(self):
        checksum = put_native(self.store, "blobby", "1.0.0", b"payload")
        served = self.site / "blobs" / f"{checksum}.crate"
        served.parent.mkdir(parents=True)
        served.write_bytes(b"different-length-content")
        with self.assertRaisesRegex(IntegrityError, "served blob mismatch"):
            self.publisher.publish()
        self.assertEqual(served.read_bytes(), b"different-length-content")
        self.assertFalse((self.site / "current").exists())

    def test_blobs_are_hard_linked_so_the_site_costs_no_extra_space(self):
        checksum = put_native(self.store, "linked", "1.0.0", b"payload")
        self.publisher.publish()
        self.assertTrue(os.path.samefile(self.store.blob_path(checksum), self.site / "blobs" / f"{checksum}.crate"))

    def test_sync_falls_back_to_a_verified_copy_when_linking_is_impossible(self):
        source = self.tmp / "src.crate"
        source.write_bytes(b"payload")
        checksum = sha256(b"payload")
        destination = self.tmp / "other" / "dst.crate"
        destination.parent.mkdir()
        real_link = os.link
        os.link = lambda *a, **k: (_ for _ in ()).throw(OSError("cross-device"))
        try:
            sync_immutable_file(source, destination, checksum)
        finally:
            os.link = real_link
        self.assertEqual(destination.read_bytes(), b"payload")
        self.assertFalse(os.path.samefile(source, destination))

    def test_interrupted_publications_leave_nothing_that_blocks_the_next_one(self):
        put_native(self.store, "foo", "1.0.0", b"x")
        self.site.mkdir()
        (self.site / ".staging.interrupted.123").mkdir()
        (self.site / ".staging.interrupted.123" / "partial").write_text("half")
        (self.site / ".current.tmp.interrupted.456").write_text("temp")
        os.symlink("generations/ghost", self.site / ".current.tmp.1.2.3")
        result = self.publisher.publish()
        leftovers = [p.name for p in self.site.iterdir() if p.name.startswith(".")]
        self.assertEqual(leftovers, [])
        self.assertEqual(os.readlink(self.site / "current"), f"generations/{result.generation_id}")

    def test_recovering_a_temporary_link_unlinks_it_without_touching_its_target(self):
        keep = self.site / "generations" / "keepme"
        keep.mkdir(parents=True)
        (keep / "config.json").write_text("keep")
        os.symlink("generations/keepme", self.site / ".current.tmp.9.9.9")
        Site(self.site).recover_staging()
        self.assertFalse(os.path.lexists(self.site / ".current.tmp.9.9.9"))
        self.assertEqual((keep / "config.json").read_text(), "keep")

    def test_symlinks_inside_the_site_are_refused(self):
        put_native(self.store, "foo", "1.0.0", b"x")
        self.site.mkdir()
        os.symlink(self.tmp, self.site / "blobs")
        with self.assertRaisesRegex(IntegrityError, "unexpected symlink"):
            self.publisher.publish()

    def test_a_large_blob_publishes(self):
        data = bytes(i % 251 for i in range(2_000_000))
        checksum = put_native(self.store, "big", "1.0.0", data)
        self.publisher.publish()
        self.assertEqual((self.site / "blobs" / f"{checksum}.crate").stat().st_size, len(data))

    def test_yanking_and_rolling_back_move_the_catalog_while_old_generations_stay_immutable(self):
        put_native(self.store, "widget", "1.0.0", b"catalog-acceptance-tarball")
        first = self.publisher.publish()
        first_catalog = (self.site / "generations" / first.generation_id / "catalog.json").read_bytes()
        catalog = json.loads(first_catalog)
        self.assertEqual(catalog["generation"], first.generation_id)
        widget = next(p for p in catalog["packages"] if p["name"] == "widget")
        self.assertEqual((widget["source"], widget["latest"], widget["versions"][0]["yanked"]), ("native", "1.0.0", False))

        self.store.set_native_yank("widget", "1.0.0", True)
        second = self.publisher.publish()
        self.assertNotEqual(second.generation_id, first.generation_id)
        yanked = json.loads((self.site / "generations" / second.generation_id / "catalog.json").read_text())
        self.assertTrue(yanked["packages"][0]["versions"][0]["yanked"])
        self.assertEqual((self.site / "generations" / first.generation_id / "catalog.json").read_bytes(), first_catalog)

        site = Site(self.site)
        staging = site.stage()
        site.activate(staging, first.generation_id, lambda directory: (directory / "catalog.json").is_file())
        restored = json.loads((self.site / "current" / "catalog.json").read_text())
        self.assertFalse(restored["packages"][0]["versions"][0]["yanked"])

    def test_activation_refuses_foreign_staging_directories(self):
        site = Site(self.tmp / "site")
        site.ensure_layout()
        with self.assertRaises(InputError):
            site.activate(self.tmp / "elsewhere" / ".staging.x", "gen-x", lambda d: True)
        with self.assertRaises(InputError):
            site.activate(self.tmp / "site" / "not-staging", "gen-x", lambda d: True)
        with self.assertRaises(InputError):
            site.activate(site.stage(), "../escape", lambda d: True)

    def test_a_generation_that_fails_verification_after_install_is_quarantined(self):
        site = Site(self.tmp / "site")
        site.ensure_layout()
        staging = site.stage()
        with self.assertRaisesRegex(IntegrityError, "failed verification"):
            site.activate(staging, "gen-bad", lambda d: False)
        self.assertFalse((self.tmp / "site" / "generations" / "gen-bad").exists())
        self.assertIsNone(site.current_generation())
        self.assertTrue(any(p.name.startswith(".quarantine.gen-bad") for p in (self.tmp / "site").iterdir()))

    def test_activating_over_an_existing_generation_that_fails_verification_changes_nothing(self):
        site = Site(self.tmp / "site")
        site.ensure_layout()
        site.activate(site.stage(), "gen-b", lambda directory: True)
        tampered = self.tmp / "site" / "generations" / "gen-b" / "index"
        tampered.write_text("tampered")
        staging = site.stage()
        (staging / "index").write_text("v1")
        with self.assertRaisesRegex(IntegrityError, "immutable generation"):
            site.activate(staging, "gen-b", lambda directory: (directory / "index").exists() and (directory / "index").read_text() == "v1")
        self.assertEqual(tampered.read_text(), "tampered")
        self.assertFalse(staging.exists())
        self.assertEqual(site.current_generation(), "gen-b")

    def test_current_must_be_a_plain_relative_link_into_generations(self):
        site = Site(self.tmp / "site")
        site.ensure_layout()
        for target in ["/etc", "generations/a/b", "elsewhere/gen", "generations/..", "generations/a.b"]:
            if os.path.lexists(self.tmp / "site" / "current"):
                os.unlink(self.tmp / "site" / "current")
            os.symlink(target, self.tmp / "site" / "current")
            with self.subTest(target=target), self.assertRaises(IntegrityError):
                site.current_generation()


class UninitializedStateTest(ScratchTestCase):
    def test_publish_refuses_a_state_directory_that_was_never_initialized(self):
        code, _, err = cli("publish", "--state-dir", self.tmp / "mistyped", "--site-dir", self.tmp / "site")
        self.assertEqual(code, 2)
        self.assertIn("mycrates init", err)
        self.assertFalse((self.tmp / "mistyped" / "config.json").exists())
        self.assertFalse((self.tmp / "site" / "current").exists())

    def test_publish_after_init_renders_an_empty_registry(self):
        state, site = init_state(self.tmp)
        self.assertEqual(cli("publish", "--state-dir", state, "--site-dir", site)[0], 0)
        catalog = json.loads((site / "current" / "catalog.json").read_text())
        self.assertEqual(catalog["packages"], [])


if __name__ == "__main__":
    unittest.main()
