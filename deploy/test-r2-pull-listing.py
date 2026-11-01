#!/usr/bin/env python3
import importlib.util
import json
import os
import pathlib
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

    def test_a_listing_that_ignores_the_cursor_fails(self):
        os.environ["FAKE_CF_IGNORE_CURSOR"] = "1"
        with self.assertRaises(SystemExit):
            pull.list_all("bucket")


if __name__ == "__main__":
    unittest.main()
