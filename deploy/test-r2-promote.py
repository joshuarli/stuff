#!/usr/bin/env python3
import hashlib
import importlib.util
import os
import pathlib
import sys
import unittest

SCRIPT = pathlib.Path(__file__).with_name("r2-promote.py")
sys.path.insert(0, str(SCRIPT.parent))
import fake_s3
spec = importlib.util.spec_from_file_location("r2_promote", SCRIPT)
r2 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(r2)

ENDPOINT_KEYS = {
    "R2_S3_ENDPOINT": "http://127.0.0.1",
    "R2_S3_ACCESS_KEY_ID": "test-access",
    "R2_S3_SECRET_ACCESS_KEY": "test-secret",
    "R2_S3_REGION": "auto",
}


class PromoteTests(unittest.TestCase):
    def setUp(self):
        self.saved = dict(os.environ)
        self.server = fake_s3.serve()
        body = b"artifact"
        self.server.objects = {
            ("bucket", "blobs/a"): (body, ""),
        }
        self.server.manifest_key = "manifests/g1.json"
        self.server.manifest = (
            '{"schemaVersion":1,"objects":[{"key":"blobs/a","size":8,'
            '"etag":"' + hashlib.md5(body).hexdigest() + '"}]}'
        )
        self.server.current = "g0"
        self.server.current_etag = '"e0"'
        self.server.change_current_on_head = False
        self.server.put_conditions = []
        for key, value in ENDPOINT_KEYS.items():
            os.environ[key] = value
        os.environ["R2_S3_ENDPOINT"] = f"http://127.0.0.1:{self.server.server_port}"

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        os.environ.clear()
        os.environ.update(self.saved)

    def test_verifies_objects_then_replaces_current_conditionally(self):
        assert r2.s3_promote("bucket", "g1", None) == "g1"
        self.assertEqual(self.server.current, "g1")
        self.assertEqual(self.server.put_conditions, [('"e0"', None, "g1")])

    def test_competing_writer_gets_412_and_does_not_overwrite(self):
        self.server.change_current_on_head = True
        with self.assertRaises(SystemExit):
            r2.s3_promote("bucket", "g1", None)
        self.assertEqual(self.server.current, "g2")

    def test_missing_generation_object_fails_before_current_write(self):
        self.server.objects = {}
        with self.assertRaises(SystemExit):
            r2.s3_promote("bucket", "g1", None)
        self.assertEqual(self.server.put_conditions, [])
        self.assertEqual(self.server.current, "g0")

    def test_expected_generation_mismatch_does_not_write(self):
        with self.assertRaises(SystemExit):
            r2.s3_promote("bucket", "g1", "other")
        self.assertEqual(self.server.put_conditions, [])

    def test_bootstrap_uses_if_none_match(self):
        self.server.current = None
        self.server.current_etag = ""
        assert r2.s3_promote("bucket", "g1", None) == "g1"
        self.assertEqual(self.server.put_conditions, [(None, "*", "g1")])


if __name__ == "__main__":
    unittest.main()
