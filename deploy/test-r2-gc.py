#!/usr/bin/env python3
import contextlib
import datetime
import fcntl
import hashlib
import http.server
import importlib.util
import io
import json
import os
import pathlib
import tempfile
import threading
import unittest
import urllib.parse
import xml.sax.saxutils

SCRIPT = pathlib.Path(__file__).with_name("r2-gc.py")
spec = importlib.util.spec_from_file_location("r2_gc", SCRIPT)
gc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gc)

ENDPOINT_KEYS = {
    "R2_S3_ENDPOINT": "http://127.0.0.1",
    "R2_S3_ACCESS_KEY_ID": "test-access",
    "R2_S3_SECRET_ACCESS_KEY": "test-secret",
    "R2_S3_REGION": "auto",
}

OLD = "2026-10-30T00:00:00.000Z"
BASE = datetime.datetime(2026, 10, 31, tzinfo=datetime.timezone.utc)


def stamp(hours):
    return (BASE + datetime.timedelta(hours=hours)).isoformat().replace("+00:00", "Z")


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

    def _request(self):
        parsed = urllib.parse.urlsplit(self.path)
        parts = parsed.path.lstrip("/").split("/", 1)
        bucket = urllib.parse.unquote(parts[0])
        key = urllib.parse.unquote(parts[1]) if len(parts) > 1 else ""
        return bucket, key, urllib.parse.parse_qs(parsed.query)

    def _handle(self):
        _, key, query = self._request()
        server = self.server
        if self.command == "GET" and query.get("list-type") == ["2"]:
            prefix = query.get("prefix", [""])[0]
            if prefix == gc.MANIFEST_PREFIX and server.change_current_on_manifest_list:
                server.change_current_on_manifest_list = False
                server.objects["current"]["body"] = b"g5"
            entries = []
            for name, obj in sorted(server.objects.items()):
                if not name.startswith(prefix):
                    continue
                body = obj["body"]
                entries.append(
                    "<Contents>"
                    f"<Key>{xml.sax.saxutils.escape(name)}</Key>"
                    f"<LastModified>{obj['time']}</LastModified>"
                    f"<Size>{len(body)}</Size>"
                    f"<ETag>{xml.sax.saxutils.escape(chr(34) + md5(body) + chr(34))}</ETag>"
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
            obj = server.objects.get(key)
            if obj is None:
                self._reply(404)
            else:
                self._reply(200, obj["body"], {"ETag": '"' + md5(obj["body"]) + '"'})
            return
        if self.command == "DELETE":
            server.deleted.append(key)
            if server.change_current_on_delete:
                server.change_current_on_delete = False
                server.objects["current"]["body"] = b"g5"
            if key in server.objects:
                del server.objects[key]
                self._reply(204)
            else:
                self._reply(404)
            return
        self._reply(405)

    do_GET = _handle
    do_DELETE = _handle


class GcTests(unittest.TestCase):
    def setUp(self):
        self.saved = dict(os.environ)
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), FakeR2)
        self.server.objects = {}
        self.server.deleted = []
        self.server.change_current_on_manifest_list = False
        self.server.change_current_on_delete = False
        self.state = tempfile.TemporaryDirectory()
        os.environ["STUFF_STATE_HOME"] = self.state.name
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        for key, value in ENDPOINT_KEYS.items():
            os.environ[key] = value
        os.environ["R2_S3_ENDPOINT"] = f"http://127.0.0.1:{self.server.server_port}"
        os.environ.pop("R2_S3_SESSION_TOKEN", None)
        self.add("current", b"g3", stamp(2))
        self.add("_probe/leftover", b"probe", OLD)
        for name in ("shared", "old1", "old2", "current", "new4", "new5"):
            self.add(f"blobs/{name}", f"blob {name}".encode(), stamp(0))
        for generation, shared in (
            ("g1", ("shared", "old1")),
            ("g2", ("shared", "old2")),
            ("g3", ("shared", "current")),
            ("g4", ("shared", "new4")),
            ("g5", ("shared", "new5")),
        ):
            self.add_generation(generation, stamp(int(generation[1:])), shared)
        self.add("generations/g6/stale", b"stale", OLD)

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.state.cleanup()
        os.environ.clear()
        os.environ.update(self.saved)

    def add(self, key, body, when):
        self.server.objects[key] = {"body": body, "time": when}

    def add_generation(self, generation, when, shared):
        tree = f"generations/{generation}/index.html"
        body = f"index {generation}".encode()
        self.add(tree, body, when)
        entries = []
        for key in (tree, *(f"blobs/{name}" for name in shared)):
            blob = self.server.objects[key]["body"]
            entries.append(
                {"key": key, "size": len(blob), "etag": md5(blob)}
            )
        manifest = {"schemaVersion": 1, "objects": entries}
        self.add(
            f"manifests/{generation}.json",
            json.dumps(manifest).encode(),
            when,
        )

    def run_gc(self, *args):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            gc.main(["bucket", *args])
        return output.getvalue()

    def test_drops_old_generations_stale_manifests_and_shared_objects(self):
        output = self.run_gc("--keep-previous", "1", "--keep-newer", "1")
        self.assertEqual(
            sorted(self.server.deleted),
            sorted(
                [
                    "generations/g1/index.html",
                    "generations/g4/index.html",
                    "generations/g6/stale",
                    "manifests/g1.json",
                    "manifests/g4.json",
                    "blobs/old1",
                    "blobs/new4",
                ]
            ),
        )
        expected = {
            "current",
            "_probe/leftover",
            "generations/g2/index.html",
            "generations/g3/index.html",
            "generations/g5/index.html",
            "manifests/g2.json",
            "manifests/g3.json",
            "manifests/g5.json",
            "blobs/shared",
            "blobs/old2",
            "blobs/current",
            "blobs/new5",
        }
        self.assertEqual(set(self.server.objects), expected)
        self.assertIn("keep generations: g3, g2, g5", output)
        self.assertIn("drop generations: g1, g4, g6", output)
        self.assertIn("reclaimed", output)
        self.assertIn("total:", output)

    def test_dry_run_changes_nothing_and_reports_projection(self):
        output = self.run_gc("--dry-run")
        self.assertEqual(self.server.deleted, [])
        self.assertIn("dry run: no objects deleted", output)
        self.assertIn("would be", output)
        self.assertIn("blobs/old1", set(self.server.objects))

    def test_recent_orphan_blocks_shared_store_deletion(self):
        self.add(
            "generations/g7/incoming",
            b"incoming",
            datetime.datetime.now(datetime.timezone.utc).isoformat().replace("+00:00", "Z"),
        )
        output = self.run_gc()
        self.assertIn("recent generation directories without a manifest", output)
        self.assertIn("blobs/old1", self.server.objects)
        self.assertIn("blobs/new4", self.server.objects)
        self.assertIn("generations/g7/incoming", self.server.objects)
        self.assertNotIn("generations/g1/index.html", self.server.objects)

    def test_a_second_run_deletes_nothing(self):
        self.run_gc()
        self.server.deleted.clear()
        output = self.run_gc()
        self.assertEqual(self.server.deleted, [])
        self.assertIn("reclaim 0 B", output)

    def test_refuses_when_current_changes_before_deletion(self):
        self.server.change_current_on_manifest_list = True
        with self.assertRaises(SystemExit):
            self.run_gc()
        self.assertEqual(self.server.deleted, [])
        self.assertEqual(self.server.objects["current"]["body"], b"g5")

    def test_refuses_when_current_has_no_manifest(self):
        del self.server.objects["manifests/g3.json"]
        with self.assertRaises(SystemExit):
            self.run_gc()
        self.assertEqual(self.server.deleted, [])

    def test_default_keeps_every_generation_newer_than_current(self):
        output = self.run_gc()
        self.assertIn("keep generations: g3, g2, g5, g4", output)
        for generation in ("g4", "g5"):
            self.assertIn(f"manifests/{generation}.json", self.server.objects)
            self.assertIn(f"generations/{generation}/index.html", self.server.objects)
        self.assertIn("blobs/new4", self.server.objects)
        self.assertNotIn("manifests/g1.json", self.server.objects)

    def test_stops_when_current_moves_between_delete_batches(self):
        gc.DELETE_BATCH = 1
        self.addCleanup(setattr, gc, "DELETE_BATCH", 256)
        self.server.change_current_on_delete = True
        with self.assertRaises(SystemExit):
            self.run_gc("--keep-newer", "0")
        self.assertEqual(len(self.server.deleted), 1)
        self.assertIn("blobs/old1", self.server.objects)

    def test_waits_for_the_registry_publish_lock(self):
        path = pathlib.Path(self.state.name, "bucket", "publish.lock")
        path.parent.mkdir(parents=True)
        holder = open(path, "a")
        fcntl.flock(holder, fcntl.LOCK_EX)
        worker = threading.Thread(target=self.run_gc)
        worker.start()
        worker.join(0.5)
        self.assertTrue(worker.is_alive())
        self.assertEqual(self.server.deleted, [])
        holder.close()
        worker.join(10)
        self.assertFalse(worker.is_alive())
        self.assertIn("manifests/g1.json", self.server.deleted)

    def test_zero_retention_keeps_only_current(self):
        self.run_gc("--keep-previous", "0", "--keep-newer", "0")
        expected = {
            "current",
            "_probe/leftover",
            "generations/g3/index.html",
            "manifests/g3.json",
            "blobs/shared",
            "blobs/current",
        }
        self.assertEqual(set(self.server.objects), expected)


if __name__ == "__main__":
    unittest.main()
