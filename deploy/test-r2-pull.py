#!/usr/bin/env python3
import contextlib
import hashlib
import importlib.util
import io
import json
import os
import pathlib
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name("r2-pull.py")
spec = importlib.util.spec_from_file_location("r2_pull", SCRIPT)
pull = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pull)

FAKE_CF = """#!/bin/sh
exec cat "$FAKE_OBJECT"
"""


class ManifestVerificationTests(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.work.name)
        (self.root / "manifests").mkdir()
        (self.root / "blobs").mkdir()
        self.big = self.root / "blobs" / "big"
        self.big.write_bytes(b"abcdefgh")
        self.saved_part = pull.PART_SIZE
        pull.PART_SIZE = 4
        self.etag = pull.multipart_etag(self.big, pull.PART_SIZE)

    def tearDown(self):
        pull.PART_SIZE = self.saved_part
        self.work.cleanup()

    def write_manifest(self, objects):
        (self.root / "manifests" / "g1.json").write_text(
            json.dumps({"schemaVersion": 1, "objects": objects})
        )

    def expect_failure(self, needle):
        stderr = io.StringIO()
        with self.assertRaises(SystemExit) as raised, contextlib.redirect_stderr(stderr):
            pull.verify_manifest(self.root, "g1")
        self.assertEqual(raised.exception.code, 1)
        self.assertIn(needle, stderr.getvalue())

    def test_multipart_etag_matches_the_manifest(self):
        self.assertTrue(self.etag.endswith("-2"), self.etag)
        self.write_manifest([{"key": "blobs/big", "size": 8, "etag": self.etag}])
        self.assertEqual(pull.verify_manifest(self.root, "g1"), 1)

    def test_missing_object_is_rejected(self):
        self.write_manifest([{"key": "blobs/missing", "size": 8, "etag": self.etag}])
        self.expect_failure("was not pulled")

    def test_size_mismatch_is_rejected(self):
        self.write_manifest([{"key": "blobs/big", "size": 7, "etag": self.etag}])
        self.expect_failure("bytes, but its manifest records 7")

    def test_etag_mismatch_is_rejected(self):
        self.write_manifest([{"key": "blobs/big", "size": 8, "etag": "0" * 32 + "-2"}])
        self.expect_failure("differs from its manifest ETag")

    def test_generation_without_a_manifest_is_not_verified(self):
        self.write_manifest([])
        os.unlink(self.root / "manifests" / "g1.json")
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            self.assertEqual(pull.verify_manifest(self.root, "g1"), 0)
        self.assertIn("has no manifest", stderr.getvalue())


class DownloadVerificationTests(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.work.name)
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        cf = bin_dir / "cf"
        cf.write_text(FAKE_CF)
        cf.chmod(0o755)
        self.dest = self.root / "dest"
        self.dest.mkdir()
        self.saved = {
            name: os.environ.get(name)
            for name in ("PATH", "CF_LOCAL_STATE", "FAKE_OBJECT")
        }
        os.environ["PATH"] = f"{bin_dir}:{self.saved['PATH']}"
        os.environ["CF_LOCAL_STATE"] = str(self.root / "state")
        self.saved_pacer = pull.PACER
        pull.PACER = pull.Pacer(0)
        self.saved_part = pull.PART_SIZE
        pull.PART_SIZE = 4

    def tearDown(self):
        pull.PACER = self.saved_pacer
        pull.PART_SIZE = self.saved_part
        for name, value in self.saved.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value
        self.work.cleanup()

    def fetch(self, data, etag):
        source = self.root / "source"
        source.write_bytes(data)
        os.environ["FAKE_OBJECT"] = str(source)
        pull.download(
            "bucket", self.dest, {"key": "blobs/object", "size": len(data), "etag": etag}
        )
        return (self.dest / "blobs" / "object").read_bytes()

    def test_multipart_object_is_verified_with_the_multipart_etag(self):
        data = b"abcdefgh"
        source = self.root / "probe"
        source.write_bytes(data)
        etag = pull.multipart_etag(source, 4)
        self.assertEqual(self.fetch(data, etag), data)

    def test_single_part_object_still_uses_md5(self):
        data = b"single"
        etag = hashlib.md5(data).hexdigest()
        self.assertEqual(self.fetch(data, etag), data)

    def test_corrupt_download_is_rejected(self):
        data = b"abcdefgh"
        stderr = io.StringIO()
        with self.assertRaises(SystemExit) as raised, contextlib.redirect_stderr(stderr):
            self.fetch(data, "0" * 32 + "-2")
        self.assertEqual(raised.exception.code, 1)
        self.assertIn("could not pull blobs/object", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
