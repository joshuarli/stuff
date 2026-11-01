#!/usr/bin/env python3
import hashlib
import http.server
import importlib.util
import os
import pathlib
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import urllib.parse
import xml.sax.saxutils

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import r2_common

spec = importlib.util.spec_from_file_location("r2_put", HERE / "r2-put.py")
r2 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(r2)

ENV_NAMES = (
    "PATH",
    "CF_LOCAL_STATE",
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


class FakeS3(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def respond(self, status, body=b"", headers=None):
        self.send_response(status)
        self.send_header("Content-Length", str(len(body)))
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.end_headers()
        if body:
            self.wfile.write(body)

    def do_PUT(self):
        parsed = urllib.parse.urlsplit(self.path)
        if parsed.query:
            self.respond(404)
            return
        parts = parsed.path.split("/", 2)
        if len(parts) != 3 or not parts[1] or not parts[2]:
            self.respond(404)
            return
        bucket = urllib.parse.unquote(parts[1])
        key = urllib.parse.unquote(parts[2])
        body = self.rfile.read(int(self.headers["Content-Length"]))
        if self.server.fail_next:
            self.server.fail_next -= 1
            self.respond(429, b"slow down", {"Retry-After": "0"})
            return
        self.server.objects[(bucket, key)] = (body, self.headers.get("Content-Type", ""))
        self.server.requests.append(("PUT", bucket, key))
        digest = hashlib.md5(body).hexdigest()
        self.respond(200, headers={"ETag": f'"{digest}"'})

    def do_GET(self):
        parsed = urllib.parse.urlsplit(self.path)
        query = urllib.parse.parse_qs(parsed.query)
        if query.get("list-type") != ["2"]:
            parts = parsed.path.split("/", 2)
            if len(parts) != 3 or not parts[1] or not parts[2]:
                self.respond(404)
                return
            bucket = urllib.parse.unquote(parts[1])
            key = urllib.parse.unquote(parts[2])
            found = self.server.objects.get((bucket, key))
            if found is None:
                self.respond(404)
                return
            body, kind = found
            digest = hashlib.md5(body).hexdigest()
            self.respond(
                200,
                body,
                {"ETag": f'"{digest}"', "Content-Type": kind or "application/octet-stream"},
            )
            return
        bucket = urllib.parse.unquote(parsed.path.strip("/"))
        after = query.get("continuation-token", [""])[0]
        page = int(query.get("max-keys", ["1000"])[0])
        keys = sorted(key for name, key in self.server.objects if name == bucket)
        if after:
            keys = [key for key in keys if key > after]
        selected = keys[:page]
        truncated = len(keys) > len(selected)
        entries = []
        for key in selected:
            body, _ = self.server.objects[(bucket, key)]
            etag = hashlib.md5(body).hexdigest()
            entries.append(
                "<Contents>"
                f"<Key>{xml.sax.saxutils.escape(key)}</Key>"
                f"<Size>{len(body)}</Size>"
                f'<ETag>&quot;{etag}&quot;</ETag>'
                "</Contents>"
            )
        token = (
            f"<NextContinuationToken>{xml.sax.saxutils.escape(selected[-1])}</NextContinuationToken>"
            if truncated
            else ""
        )
        body = (
            '<?xml version="1.0" encoding="UTF-8"?>'
            '<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">'
            f"<IsTruncated>{'true' if truncated else 'false'}</IsTruncated>"
            f"{''.join(entries)}{token}"
            "</ListBucketResult>"
        ).encode()
        self.respond(200, body)


class FakeS3Server:
    def __init__(self):
        self.httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), FakeS3)
        self.httpd.objects = {}
        self.httpd.requests = []
        self.httpd.fail_next = 0
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()

    @property
    def objects(self):
        return self.httpd.objects

    @property
    def requests(self):
        return self.httpd.requests

    @property
    def fail_next(self):
        return self.httpd.fail_next

    @fail_next.setter
    def fail_next(self, value):
        self.httpd.fail_next = value

    def close(self):
        self.httpd.shutdown()
        self.httpd.server_close()

    @property
    def endpoint(self):
        return f"http://127.0.0.1:{self.httpd.server_port}"


class R2PutTests(unittest.TestCase):
    def setUp(self):
        self.saved = {name: os.environ.get(name) for name in ENV_NAMES}
        self.work = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.work.name)
        self.site = self.root / "site"
        self.site.mkdir()
        self.server = FakeS3Server()

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

    def test_sync_promotes_through_s3(self):
        self.use_s3()
        generation = self.site / "generations" / "g1"
        generation.mkdir(parents=True)
        (generation / "catalog.json").write_text("{}\n")
        os.symlink("generations/g1", self.site / "current")
        os.environ["R2_PUT_JOBS"] = "2"
        os.environ.pop("R2_PROMOTE_UNCONDITIONAL", None)
        os.environ.pop("CF_LOCAL_STATE", None)
        done = subprocess.run(
            [str(HERE / "r2-sync.sh"), str(self.site), "bucket", "--promote=s3"],
            env=os.environ.copy(),
            capture_output=True,
            text=True,
        )
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(
            self.server.objects[("bucket", "current")][0], b"g1"
        )
        self.assertIn(("bucket", "generations/g1/catalog.json"), self.server.objects)
        self.assertIn(("bucket", "manifests/g1.json"), self.server.objects)

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
