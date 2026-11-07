#!/usr/bin/env python3
import hashlib
import importlib.util
import json
import os
import pathlib
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name("r2-pull.py")
spec = importlib.util.spec_from_file_location("r2_pull", SCRIPT)
pull = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pull)

FAKE_CF = """#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
page = 1000
after = None
i = 0
while i < len(args):
    if args[i] == "--per-page":
        page = int(args[i + 1])
        i += 2
    elif args[i] == "--start-after":
        after = args[i + 1]
        i += 2
    else:
        i += 1
objects = json.load(open(os.environ["FAKE_CF_OBJECTS"]))
if after is not None and os.environ.get("FAKE_CF_IGNORE_CURSOR") != "1":
    if os.environ.get("FAKE_CF_RAW_CURSOR") == "1":
        objects = [o for o in objects if o["key"] > after]
    else:
        keys = [o["key"] for o in objects]
        objects = objects[keys.index(after) + 1:]
json.dump(objects[:page], sys.stdout)
"""

FAKE_GET = """#!/usr/bin/env python3
import os, sys
marker = os.environ["FAKE_CF_GET_ATTEMPTS"]
count = int(open(marker).read()) if os.path.exists(marker) else 0
open(marker, "w").write(str(count + 1))
if count < int(os.environ["FAKE_CF_GET_FAILURES"]):
    sys.stderr.write("429 Too Many Requests")
    sys.exit(1)
sys.stdout.write("body")
"""

BACKEND_ORDER = [
    "generations/g/crates-io/ts/-r/ts-rs-macros",
    "generations/g/crates-io/ts/-r/ts-rs",
    "generations/g/crates-io/tu/rb/turboshake",
    "generations/g/crates-io/tw/ow/twoway",
    "generations/g/crates-io/ty/pe/type-map",
]


class ListingTests(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        root = pathlib.Path(self.work.name)
        (root / "bin").mkdir()
        cf = root / "bin" / "cf"
        cf.write_text(FAKE_CF)
        cf.chmod(0o755)
        remote = [{"key": key, "size": 1, "etag": "x"} for key in BACKEND_ORDER]
        (root / "objects.json").write_text(json.dumps(remote))
        self.saved = {name: os.environ.get(name) for name in ("PATH", "FAKE_CF_OBJECTS", "FAKE_CF_IGNORE_CURSOR")}
        os.environ["PATH"] = f"{root / 'bin'}:{self.saved['PATH']}"
        os.environ["FAKE_CF_OBJECTS"] = str(root / "objects.json")
        os.environ.pop("FAKE_CF_IGNORE_CURSOR", None)
        self.saved_page = pull.PAGE
        pull.PAGE = 1
        self.saved_pacer = pull.PACER
        pull.PACER = pull.Pacer(0)

    def tearDown(self):
        pull.PAGE = self.saved_page
        pull.PACER = self.saved_pacer
        for name, value in self.saved.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value
        self.work.cleanup()

    def test_follows_the_backend_listing_order_across_pages(self):
        listed = [o["key"] for o in pull.list_all("bucket")]
        self.assertEqual(listed, BACKEND_ORDER)

    def test_download_waits_out_rate_limits(self):
        root = pathlib.Path(self.work.name)
        cf = root / "bin" / "cf"
        cf.write_text(FAKE_GET)
        os.environ["FAKE_CF_GET_ATTEMPTS"] = str(root / "attempts")
        os.environ["FAKE_CF_GET_FAILURES"] = "2"
        saved_backoff = pull.RATE_LIMIT_BACKOFF_SECONDS
        pull.RATE_LIMIT_BACKOFF_SECONDS = 0
        try:
            dest = root / "dest"
            dest.mkdir()
            body_md5 = "841a2d689ad86bd1611447453c22c6fc"
            pull.download("bucket", str(dest), {"key": "blobs/a", "size": 4, "etag": body_md5})
            self.assertEqual((dest / "blobs" / "a").read_text(), "body")
            self.assertEqual((root / "attempts").read_text(), "3")
            os.environ["FAKE_CF_GET_FAILURES"] = "99"
            (root / "attempts").unlink()
            with self.assertRaises(SystemExit):
                pull.download("bucket", str(dest), {"key": "blobs/b", "size": 4, "etag": body_md5})
        finally:
            pull.RATE_LIMIT_BACKOFF_SECONDS = saved_backoff
            os.environ.pop("FAKE_CF_GET_ATTEMPTS", None)
            os.environ.pop("FAKE_CF_GET_FAILURES", None)

    def test_a_cursor_on_a_key_that_prefixes_others_lists_each_key_once(self):
        root = pathlib.Path(self.work.name)
        r2_order = [
            "generations/g/crates-io/si/gn/signal-hook-mio",
            "generations/g/crates-io/si/gn/signal-hook-registry",
            "generations/g/crates-io/si/gn/signal-hook",
            "generations/g/crates-io/si/gn/signature",
        ]
        (root / "objects.json").write_text(
            json.dumps([{"key": key, "size": 1, "etag": "x"} for key in r2_order])
        )
        os.environ["FAKE_CF_RAW_CURSOR"] = "1"
        pull.PAGE = 3
        try:
            listed = [o["key"] for o in pull.list_all("bucket")]
        finally:
            os.environ.pop("FAKE_CF_RAW_CURSOR", None)
        self.assertEqual(listed, r2_order)

    def test_a_listing_that_ignores_the_cursor_fails(self):
        os.environ["FAKE_CF_IGNORE_CURSOR"] = "1"
        with self.assertRaises(SystemExit):
            pull.list_all("bucket")


sys.path.insert(0, str(pathlib.Path(__file__).parent))
import fake_s3

S3_ENV = (
    "PATH",
    "CF_LOCAL_STATE",
    "R2_PULL_CF_ONLY",
    "R2_LIST_PAGE",
    "R2_S3_ENDPOINT",
    "R2_S3_ACCESS_KEY_ID",
    "R2_S3_SECRET_ACCESS_KEY",
    "R2_S3_SESSION_TOKEN",
)


class S3PullTests(unittest.TestCase):
    def setUp(self):
        self.saved = {name: os.environ.get(name) for name in S3_ENV}
        self.work = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.work.name)
        self.server = fake_s3.serve()
        os.environ["R2_S3_ENDPOINT"] = self.server.endpoint
        os.environ["R2_S3_ACCESS_KEY_ID"] = "key"
        os.environ["R2_S3_SECRET_ACCESS_KEY"] = "secret"
        os.environ.pop("CF_LOCAL_STATE", None)
        os.environ.pop("R2_PULL_CF_ONLY", None)
        os.environ.pop("R2_S3_SESSION_TOKEN", None)
        self.saved_page = pull.PAGE
        self.saved_pacer = pull.PACER
        pull.PACER = pull.Pacer(0)

    def tearDown(self):
        self.server.close()
        pull.PAGE = self.saved_page
        pull.PACER = self.saved_pacer
        for name, value in self.saved.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value
        self.work.cleanup()

    def test_listing_uses_paginated_s3(self):
        pull.PAGE = 2
        keys = ["generations/g/a", "generations/g/b", "generations/g/c"]
        for key in keys:
            self.server.objects[("bucket", key)] = (key.encode(), "text/plain")
        listed = pull.list_all("bucket")
        self.assertEqual([o["key"] for o in listed], keys)
        self.assertEqual(listed[0]["size"], len(keys[0]))
        self.assertEqual(listed[0]["etag"], hashlib.md5(keys[0].encode()).hexdigest())

    def test_download_uses_s3_and_checks_the_etag(self):
        body = b"s3 pulled bytes"
        self.server.objects[("bucket", "blobs/a")] = (body, "text/plain")
        dest = self.root / "dest"
        dest.mkdir()
        etag = hashlib.md5(body).hexdigest()
        pull.download("bucket", str(dest), {"key": "blobs/a", "size": len(body), "etag": etag})
        self.assertEqual((dest / "blobs" / "a").read_bytes(), body)
        self.assertFalse((dest / "blobs" / "a.part").exists())

    def test_main_pulls_the_current_generation_through_s3(self):
        objects = {
            "current": b"g",
            "generations/g/catalog.json": b'{"gen":1}',
            "generations/old/catalog.json": b'{"gen":0}',
            "blobs/0000.crate": b"crate bytes",
        }
        for key, body in objects.items():
            self.server.objects[("bucket", key)] = (body, "text/plain")
        dest = self.root / "dest"
        saved_argv = sys.argv
        sys.argv = ["r2-pull.py", "bucket", str(dest)]
        try:
            pull.main()
        finally:
            sys.argv = saved_argv
        self.assertEqual((dest / "current").readlink().as_posix(), "generations/g")
        self.assertEqual(
            (dest / "generations" / "g" / "catalog.json").read_bytes(),
            objects["generations/g/catalog.json"],
        )
        self.assertFalse((dest / "generations" / "old").exists())
        self.assertEqual((dest / "blobs" / "0000.crate").read_bytes(), objects["blobs/0000.crate"])

    def test_mints_credentials_through_stuff_when_they_are_absent(self):
        for name in ("R2_S3_ENDPOINT", "R2_S3_ACCESS_KEY_ID", "R2_S3_SECRET_ACCESS_KEY"):
            os.environ.pop(name, None)
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        stuff = bin_dir / "stuff"
        stuff.write_text(
            "#!/bin/sh\n"
            "[ \"$1 $2\" = 'credentials bucket' ] || exit 2\n"
            "echo \"export R2_S3_ENDPOINT='https://acct.r2.cloudflarestorage.com'\"\n"
            "echo \"export R2_S3_ACCESS_KEY_ID='minted'\"\n"
            "echo \"export R2_S3_SECRET_ACCESS_KEY='minted-secret'\"\n"
            "echo \"export R2_S3_SESSION_TOKEN='minted-token'\"\n"
        )
        stuff.chmod(0o755)
        os.environ["STUFF"] = str(stuff)
        try:
            self.assertTrue(pull.enable_s3("bucket"))
        finally:
            os.environ.pop("STUFF", None)
        self.assertEqual(os.environ["R2_S3_ACCESS_KEY_ID"], "minted")
        self.assertEqual(os.environ["R2_S3_SESSION_TOKEN"], "minted-token")

if __name__ == "__main__":
    unittest.main()
