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

    def tearDown(self):
        pull.PAGE = self.saved_page
        for name, value in self.saved.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value
        self.work.cleanup()

    def test_follows_the_backend_listing_order_across_pages(self):
        listed = [o["key"] for o in pull.list_all("bucket")]
        self.assertEqual(listed, BACKEND_ORDER)

    def test_a_listing_that_ignores_the_cursor_fails(self):
        os.environ["FAKE_CF_IGNORE_CURSOR"] = "1"
        with self.assertRaises(SystemExit):
            pull.list_all("bucket")


if __name__ == "__main__":
    unittest.main()
