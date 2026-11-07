#!/usr/bin/env python3
import hashlib
import importlib.util
import os
import pathlib
import sys
import tempfile
import time
import unittest

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fake_s3
import r2_common

spec = importlib.util.spec_from_file_location("r2_put", HERE / "r2-put.py")
r2 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(r2)

ENV_NAMES = (
    "PATH",
    "CF_LOCAL_STATE",
    "STUFF_STATE_HOME",
    "R2_S3_ENDPOINT",
    "R2_S3_ACCESS_KEY_ID",
    "R2_S3_SECRET_ACCESS_KEY",
    "R2_S3_REGION",
    "R2_S3_SESSION_TOKEN",
    "R2_PUT_JOBS",
    "R2_PUT_ATTEMPTS",
    "R2_PUT_INTERVAL",
    "R2_LIST_PAGE",
    "R2_PROMOTE_UNCONDITIONAL",
    "FAKE_CF_LOG",
)


class R2PutTests(unittest.TestCase):
    def setUp(self):
        self.saved = {name: os.environ.get(name) for name in ENV_NAMES}
        self.work = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.work.name)
        self.site = self.root / "site"
        self.site.mkdir()
        self.server = fake_s3.serve()
        self.server.request_log_style = "put"

    def tearDown(self):
        self.server.close()
        for name, value in self.saved.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value
        self.work.cleanup()

    def write_site(self, files):
        keys = []
        for key, body in files.items():
            path = self.site / key
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(body)
            keys.append(key)
        keys_file = self.root / "keys"
        keys_file.write_text("\n".join(keys) + "\n")
        return keys_file

    def use_s3(self):
        os.environ["R2_S3_ENDPOINT"] = self.server.endpoint
        os.environ["R2_S3_ACCESS_KEY_ID"] = "key"
        os.environ["R2_S3_SECRET_ACCESS_KEY"] = "secret"
        os.environ.pop("CF_LOCAL_STATE", None)

    def test_content_types_match_the_site_layout(self):
        cases = {
            "catalog.json": "application/json",
            "index.html": "text/html; charset=utf-8",
            "llms.txt": "text/markdown; charset=utf-8",
            "pyproject.toml": "text/plain; charset=utf-8",
            "blobs/a.crate": "application/x-tar",
            "tarballs/a.tgz": "application/octet-stream",
            "wheels/a.whl": "application/octet-stream",
            "meta.metadata": "text/plain; charset=utf-8",
            "other.bin": "application/octet-stream",
        }
        for key, kind in cases.items():
            self.assertEqual(r2.content_type(key), kind, key)

    def test_uploads_through_s3_and_retries_rate_limits(self):
        self.use_s3()
        os.environ["R2_PUT_JOBS"] = "4"
        os.environ["R2_PUT_ATTEMPTS"] = "2"
        os.environ["R2_PUT_INTERVAL"] = "0"
        files = {
            "catalog.json": b'{"generation":"g"}',
            "index.html": b"<html></html>",
            "blobs/a.crate": b"crate bytes",
            "empty.txt": b"",
        }
        keys_file = self.write_site(files)
        self.server.fail_next = 1
        self.assertEqual(r2.main([str(self.site), "bucket", str(keys_file)]), 0)
        for key, body in files.items():
            self.assertEqual(self.server.objects[("bucket", key)][0], body, key)
        self.assertEqual(
            self.server.objects[("bucket", "catalog.json")][1], "application/json"
        )
        self.assertEqual(
            self.server.objects[("bucket", "index.html")][1],
            "text/html; charset=utf-8",
        )
        puts = [key for method, _, key in self.server.requests if method == "PUT"]
        self.assertCountEqual(puts, files)

    def test_shared_pacer_spaces_concurrent_syncs(self):
        path = self.root / "pace"
        first = r2_common.Pacer(0.15, shared_path=str(path))
        second = r2_common.Pacer(0.15, shared_path=str(path))
        started = time.monotonic()
        first.wait()
        second.wait()
        self.assertGreaterEqual(time.monotonic() - started, 0.12)
        first.penalize(0.1)
        started = time.monotonic()
        second.wait()
        self.assertGreaterEqual(time.monotonic() - started, 0.08)

    def test_remote_index_uses_paginated_s3_listing(self):
        self.use_s3()
        os.environ["R2_LIST_PAGE"] = "2"
        keys = [f"generations/g/{name}" for name in ("a", "b", "c", "d", "e")]
        for key in keys:
            body = key.encode()
            self.server.objects[("bucket", key)] = (body, "text/plain")
        index = r2_common.remote_index("bucket", keys, 2)
        self.assertEqual(set(index), set(keys))
        for key in keys:
            body = key.encode()
            self.assertEqual(index[key], (len(body), hashlib.md5(body).hexdigest()))

    def test_falls_back_to_paced_cf_uploads(self):
        for name in (
            "R2_S3_ENDPOINT",
            "R2_S3_ACCESS_KEY_ID",
            "R2_S3_SECRET_ACCESS_KEY",
            "CF_LOCAL_STATE",
        ):
            os.environ.pop(name, None)
        os.environ["R2_PUT_JOBS"] = "1"
        log = self.root / "cf.log"
        os.environ["FAKE_CF_LOG"] = str(log)
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        cf = bin_dir / "cf"
        cf.write_text(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$FAKE_CF_LOG\"\n"
        )
        cf.chmod(0o755)
        os.environ["PATH"] = f"{bin_dir}:{self.saved.get('PATH') or ''}"
        keys_file = self.write_site({"catalog.json": b"{}"})
        self.assertEqual(r2.main([str(self.site), "bucket", str(keys_file)]), 0)
        self.assertIn("put catalog.json", log.read_text())
        self.assertIn("--bucket-name bucket", log.read_text())


if __name__ == "__main__":
    unittest.main()
