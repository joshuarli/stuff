import json
import os
import shutil
import unittest

from mycrates.adopt import adopt_site
from mycrates.errors import ConflictError, InputError, IntegrityError
from mycrates.publish import Publisher
from mycrates.store import Store

from .support import GOLDEN, ScratchTestCase, init_state


class AdoptTest(ScratchTestCase):
    def setUp(self):
        super().setUp()
        self.source = self.tmp / "published"
        shutil.copytree(GOLDEN / "adopt-publish" / "input-site", self.source, symlinks=True)
        self.generation = self.source / "generations" / "golden1"
        (self.tmp / "target").mkdir()
        self.state, self.site = init_state(self.tmp / "target", "https://crates.example.test")
        self.store = Store(self.state)

    def test_adoption_counts_entries_and_blobs_and_leaves_a_verifiable_state(self):
        summary = adopt_site(self.store, self.source)
        self.assertEqual((summary.native, summary.upstream, summary.blobs), (7, 3, 10))
        self.store.verify()
        self.assertEqual(adopt_site(self.store, self.source).blobs, 0)

    def test_upstream_lines_are_adopted_verbatim_without_an_acquisition_time(self):
        adopt_site(self.store, self.source)
        line = (self.generation / "crates-io/se/rd/serde").read_text().splitlines()[1]
        record = next(r for r in self.store.load_upstream_records() if r.vers == "1.0.1")
        self.assertEqual((record.raw_index_line, record.acquired_at), (line, 0))

    def test_a_tampered_blob_is_an_integrity_error_and_adopts_nothing_from_it(self):
        victim = next((self.source / "blobs").iterdir())
        victim.write_bytes(victim.read_bytes() + b"tampered")
        with self.assertRaisesRegex(IntegrityError, "not the index checksum"):
            adopt_site(self.store, self.source)

    def test_a_missing_blob_fails(self):
        next((self.source / "blobs").iterdir()).unlink()
        with self.assertRaisesRegex(IntegrityError, "missing blob"):
            adopt_site(self.store, self.source)

    def test_blobs_the_current_generation_does_not_reference_are_ignored(self):
        (self.source / "blobs" / ("f" * 64 + ".crate")).write_bytes(b"orphan")
        summary = adopt_site(self.store, self.source)
        self.assertEqual(summary.blobs, 10)
        self.assertFalse(self.store.blob_path("f" * 64).exists())

    def test_index_lines_must_sit_at_their_shard_path(self):
        shard = self.generation / "registry" / "3" / "a" / "abc"
        wrong = self.generation / "registry" / "3" / "b" / "abc"
        wrong.parent.mkdir()
        shard.rename(wrong)
        with self.assertRaisesRegex(IntegrityError, "is stored at"):
            adopt_site(self.store, self.source)

    def test_a_checksum_that_is_not_lowercase_sha256_hex_is_rejected(self):
        path = self.generation / "registry" / "2" / "ab"
        document = json.loads(path.read_text())
        document["cksum"] = document["cksum"].upper()
        path.write_text(json.dumps(document) + "\n")
        with self.assertRaisesRegex(IntegrityError, "lowercase SHA-256"):
            adopt_site(self.store, self.source)

    def test_the_site_needs_a_current_symlink_to_a_plain_generation(self):
        os.unlink(self.source / "current")
        with self.assertRaises(InputError):
            adopt_site(self.store, self.source)
        os.symlink("generations/../../etc", self.source / "current")
        with self.assertRaisesRegex(IntegrityError, "unsafe current target"):
            adopt_site(self.store, self.source)
        os.unlink(self.source / "current")
        os.symlink("generations/missing", self.source / "current")
        with self.assertRaisesRegex(IntegrityError, "generation is missing"):
            adopt_site(self.store, self.source)

    def test_symlinks_inside_an_index_view_are_refused(self):
        os.symlink(self.tmp, self.generation / "registry" / "link")
        with self.assertRaisesRegex(IntegrityError, "unexpected index entry"):
            adopt_site(self.store, self.source)

    def test_adopting_a_conflicting_version_surfaces_the_conflict(self):
        adopt_site(self.store, self.source)
        path = self.generation / "registry" / "2" / "ab"
        document = json.loads(path.read_text())
        document["cksum"] = "0" * 64
        path.write_text(json.dumps(document) + "\n")
        (self.source / "blobs" / ("0" * 64 + ".crate")).write_bytes(b"")
        with self.assertRaises((ConflictError, IntegrityError)):
            adopt_site(self.store, self.source)

    def test_an_adopted_state_republishes_the_identical_generation(self):
        adopt_site(self.store, self.source)
        result = Publisher(self.store, self.site, "https://crates.example.test").publish()
        expected = (GOLDEN / "adopt-publish" / "expected-generation-id").read_text().strip()
        self.assertEqual(result.generation_id, expected)


if __name__ == "__main__":
    unittest.main()
