#!/usr/bin/env python3
"""Offline test for deploy/r2-gc-blobs.py against a fake S3 endpoint."""

import datetime
import hashlib
import http.server
import importlib.util
import json
import os
import pathlib
import tempfile
import threading
import unittest
import urllib.parse
import xml.sax.saxutils

SCRIPT = pathlib.Path(__file__).with_name("r2-gc-blobs.py")
STUFF = os.environ.get("STUFF") or str(SCRIPT.parents[1] / "target" / "debug" / "stuff")
spec = importlib.util.spec_from_file_location("r2_gc_blobs", SCRIPT)
gcblobs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gcblobs)

A = "a" * 64
B = "b" * 64
ORPHAN = "c" * 64
BASE = datetime.datetime(2026, 10, 31, tzinfo=datetime.timezone.utc)


def stamp(hours):
    return (BASE + datetime.timedelta(hours=hours)).isoformat().replace("+00:00", "Z")


def now_stamp():
    return datetime.datetime.now(datetime.timezone.utc).isoformat().replace("+00:00", "Z")


def blobs_json(*digests):
    return json.dumps({"schemaVersion": 1, "blobs": ["sha256:" + d for d in digests]}).encode()


def md5(body):
    return hashlib.md5(body).hexdigest()


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

    def _target(self):
        parsed = urllib.parse.urlsplit(self.path)
        parts = parsed.path.lstrip("/").split("/", 1)
        bucket = urllib.parse.unquote(parts[0])
        key = urllib.parse.unquote(parts[1]) if len(parts) > 1 else ""
        return bucket, key, urllib.parse.parse_qs(parsed.query)

    def _handle(self):
        bucket, key, query = self._target()
        objects = self.server.objects.get(bucket, {})
        if self.command == "GET" and query.get("list-type") == ["2"]:
            prefix = query.get("prefix", [""])[0]
            entries = []
            for name, obj in sorted(objects.items()):
                if not name.startswith(prefix):
                    continue
                entries.append(
                    "<Contents>"
                    f"<Key>{xml.sax.saxutils.escape(name)}</Key>"
                    f"<LastModified>{obj['time']}</LastModified>"
                    f"<Size>{len(obj['body'])}</Size>"
                    f"<ETag>{xml.sax.saxutils.escape(chr(34) + md5(obj['body']) + chr(34))}</ETag>"
                    "</Contents>"
                )
            body = (
                '<?xml version="1.0" encoding="UTF-8"?>'
                '<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">'
                "<IsTruncated>false</IsTruncated>"
                f"{''.join(entries)}</ListBucketResult>"
            ).encode()
            self._reply(200, body)
            return
        if self.command == "GET":
            obj = objects.get(key)
            if obj is None:
                self._reply(404)
            else:
                self._reply(200, obj["body"], {"ETag": '"' + md5(obj["body"]) + '"'})
            return
        if self.command == "DELETE":
            self.server.deleted.append(key)
            if self.server.on_delete is not None:
                self.server.on_delete(self.server)
                self.server.on_delete = None
            if key in objects:
                del objects[key]
                self._reply(204)
            else:
                self._reply(404)
            return
        self._reply(405)

    do_GET = _handle
    do_HEAD = _handle
    do_DELETE = _handle


def manifest(keys):
    return json.dumps(
        {
            "schemaVersion": 1,
            "objects": [
                {"key": key, "size": len(body), "etag": md5(body)}
                for key, body in keys
            ],
        }
    ).encode()


class GcBlobsTest(unittest.TestCase):
    def setUp(self):
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), FakeR2)
        self.server.objects = {}
        self.server.deleted = []
        self.server.on_delete = None
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.home = tempfile.TemporaryDirectory()
        self.env = {
            "R2_S3_ENDPOINT": f"http://127.0.0.1:{self.server.server_address[1]}",
            "R2_S3_ACCESS_KEY_ID": "test-access",
            "R2_S3_SECRET_ACCESS_KEY": "test-secret",
            "R2_S3_REGION": "auto",
            "STUFF_STATE_HOME": self.home.name,
            "STUFF_CONFIG_HOME": self.home.name,
            "STUFF": STUFF,
        }
        self.saved = {name: os.environ.get(name) for name in self.env}
        os.environ.update(self.env)
        os.environ.pop("CF_LOCAL_STATE", None)

        g1_files = [("generations/g1/catalog.json", b"one")]
        g2_files = [("generations/g2/catalog.json", b"two")]
        shared = [("oci/manifests/sha256/" + "d" * 64, b"manifest")]
        self.server.objects["stuff-oci"] = {
            "current": {"body": b"g2", "time": stamp(5)},
            "generations/g1/catalog.json": {"body": b"one", "time": stamp(1)},
            "generations/g2/catalog.json": {"body": b"two", "time": stamp(5)},
            "oci/manifests/sha256/" + "d" * 64: {"body": b"manifest", "time": stamp(1)},
            "manifests/g1.json": {"body": manifest(g1_files + shared), "time": stamp(1)},
            "manifests/g2.json": {"body": manifest(g2_files + shared), "time": stamp(5)},
            "generations/g1/blobs.json": {
                "body": json.dumps({"schemaVersion": 1, "blobs": ["sha256:" + A]}).encode(),
                "time": stamp(1),
            },
            "generations/g2/blobs.json": {
                "body": json.dumps({"schemaVersion": 1, "blobs": ["sha256:" + B]}).encode(),
                "time": stamp(5),
            },
        }
        self.server.objects["stuff-oci-blobs"] = {
            "sha256/" + A: {"body": b"blob-a", "time": stamp(1)},
            "sha256/" + B: {"body": b"blob-b", "time": stamp(5)},
            "sha256/" + ORPHAN: {"body": b"orphan", "time": stamp(5)},
        }

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        for name, value in self.saved.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value
        self.home.cleanup()

    def test_dry_run_keeps_previous_generation_blobs(self):
        self.assertEqual(
            gcblobs.main(
                ["stuff-oci", "stuff-oci-blobs", "--keep-previous", "1", "--dry-run"]
            ),
            0,
        )
        self.assertIn("sha256/" + ORPHAN, self.server.objects["stuff-oci-blobs"])
        self.assertEqual(self.server.deleted, [])

    def test_deletes_unreferenced_and_non_retained_blobs(self):
        self.assertEqual(
            gcblobs.main(["stuff-oci", "stuff-oci-blobs", "--keep-previous", "0"]),
            0,
        )
        remaining = set(self.server.objects["stuff-oci-blobs"])
        self.assertIn("sha256/" + B, remaining)
        self.assertNotIn("sha256/" + A, remaining)
        self.assertNotIn("sha256/" + ORPHAN, remaining)

    def meta(self, key, body, when):
        self.server.objects["stuff-oci"][key] = {"body": body, "time": when}

    def blob(self, digest, when):
        self.server.objects["stuff-oci-blobs"]["sha256/" + digest] = {"body": digest.encode(), "time": when}

    def remaining(self):
        return set(self.server.objects["stuff-oci-blobs"])

    def test_keeps_recent_uploads_until_the_grace_period_expires(self):
        fresh = "e" * 64
        self.blob(fresh, now_stamp())
        self.assertEqual(gcblobs.main(["stuff-oci", "stuff-oci-blobs", "--keep-previous", "0"]), 0)
        self.assertIn("sha256/" + fresh, self.remaining())
        self.assertNotIn("sha256/" + ORPHAN, self.remaining())
        self.assertEqual(
            gcblobs.main(["stuff-oci", "stuff-oci-blobs", "--keep-previous", "0", "--grace-minutes", "0"]),
            0,
        )
        self.assertNotIn("sha256/" + fresh, self.remaining())

    def test_a_recent_generation_without_a_manifest_keeps_its_blobs(self):
        self.meta("generations/g3/blobs.json", blobs_json(A), now_stamp())
        self.assertEqual(gcblobs.main(["stuff-oci", "stuff-oci-blobs", "--keep-previous", "0"]), 0)
        self.assertIn("sha256/" + A, self.remaining())
        self.assertNotIn("sha256/" + ORPHAN, self.remaining())

    def test_a_recent_generation_without_blobs_json_blocks_deletion(self):
        self.meta("generations/g3/catalog.json", b"three", now_stamp())
        self.assertEqual(gcblobs.main(["stuff-oci", "stuff-oci-blobs", "--keep-previous", "0"]), 0)
        self.assertEqual(self.server.deleted, [])

    def test_a_recent_unactivated_generation_is_retained(self):
        files = [("generations/g3/catalog.json", b"three")]
        self.meta("generations/g3/catalog.json", b"three", now_stamp())
        self.meta("generations/g3/blobs.json", blobs_json(A), now_stamp())
        self.meta("manifests/g3.json", manifest(files), now_stamp())
        self.assertEqual(
            gcblobs.main(["stuff-oci", "stuff-oci-blobs", "--keep-previous", "0", "--keep-newer", "0"]),
            0,
        )
        self.assertIn("sha256/" + A, self.remaining())

    def test_stops_when_current_moves_between_delete_batches(self):
        self.blob("f" * 64, stamp(1))
        gcblobs.DELETE_BATCH = 1
        self.addCleanup(setattr, gcblobs, "DELETE_BATCH", 256)

        def activate(server):
            server.objects["stuff-oci"]["current"] = {"body": b"g1", "time": now_stamp()}

        self.server.on_delete = activate
        with self.assertRaises(SystemExit):
            gcblobs.main(["stuff-oci", "stuff-oci-blobs", "--keep-previous", "0"])
        self.assertEqual(len(self.server.deleted), 1)


if __name__ == "__main__":
    unittest.main()
