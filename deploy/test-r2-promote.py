#!/usr/bin/env python3
import hashlib
import http.server
import importlib.util
import os
import pathlib
import threading
import unittest
import urllib.parse

SCRIPT = pathlib.Path(__file__).with_name("r2-promote.py")
spec = importlib.util.spec_from_file_location("r2_promote", SCRIPT)
r2 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(r2)

ENDPOINT_KEYS = {
    "R2_S3_ENDPOINT": "http://127.0.0.1",
    "R2_S3_ACCESS_KEY_ID": "test-access",
    "R2_S3_SECRET_ACCESS_KEY": "test-secret",
    "R2_S3_REGION": "auto",
}


class FakeR2(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def _reply(self, status, body=b"", headers=None):
        self.send_response(status)
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if body and self.command != "HEAD":
            self.wfile.write(body)

    def _path(self):
        path = urllib.parse.unquote(urllib.parse.urlsplit(self.path).path)
        return path.lstrip("/").split("/", 1)[1]

    def _handle(self):
        key = self._path()
        server = self.server
        if self.command == "GET" and key == "current":
            if server.current is None:
                self._reply(404)
            else:
                self._reply(200, server.current.encode(), {"ETag": server.current_etag})
            return
        if self.command == "GET" and key == server.manifest_key:
            self._reply(200, server.manifest.encode(), {"ETag": '"manifest"'})
            return
        if self.command == "HEAD":
            if server.change_current_on_head:
                server.change_current_on_head = False
                server.current = "g2"
                server.current_etag = '"e2"'
            found = server.objects.get(key)
            if found is None:
                self._reply(404)
            else:
                body, etag = found
                self._reply(200, body, {"ETag": etag})
            return
        if self.command == "PUT" and key == "current":
            condition = self.headers.get("if-match")
            none_match = self.headers.get("if-none-match")
            body = self.rfile.read(int(self.headers.get("Content-Length", "0"))).decode()
            server.put_conditions.append((condition, none_match, body))
            if condition is not None and condition != server.current_etag:
                self._reply(412)
                return
            if none_match == "*" and server.current is not None:
                self._reply(412)
                return
            server.current = body
            server.current_etag = '"e3"'
            self._reply(200)
            return
        self._reply(404)

    do_GET = _handle
    do_HEAD = _handle
    do_PUT = _handle


class PromoteTests(unittest.TestCase):
    def setUp(self):
        self.saved = dict(os.environ)
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), FakeR2)
        body = b"artifact"
        self.server.objects = {
            "blobs/a": (body, '"' + hashlib.md5(body).hexdigest() + '"'),
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
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
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
