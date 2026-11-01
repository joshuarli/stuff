#!/usr/bin/env python3
import contextlib
import importlib.util
import io
import json
import os
import pathlib
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name("r2-plan.py")
spec = importlib.util.spec_from_file_location("r2_plan", SCRIPT)
r2 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(r2)

S3_VARS = ("R2_S3_ENDPOINT", "R2_S3_ACCESS_KEY_ID", "R2_S3_SECRET_ACCESS_KEY")

FAKE_CF = """#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
prefix = ""
page = 1000
i = 0
while i < len(args):
    if args[i] == "--prefix":
        prefix = args[i + 1]
        i += 2
    elif args[i] == "--per-page":
        page = int(args[i + 1])
        i += 2
    else:
        i += 1
objects = json.load(open(os.environ["FAKE_CF_OBJECTS"]))
selected = [o for o in objects if o["key"].startswith(prefix)][:page]
json.dump(selected, sys.stdout)
"""


class PaginationTests(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.work.name)
        self.site = self.root / "site"
        self.objects = self.site / "objects"
        self.objects.mkdir(parents=True)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        cf = self.bin / "cf"
        cf.write_text(FAKE_CF)
        cf.chmod(0o755)
        self.keys = []
        remote = []
        for i in range(1100):
            key = f"objects/pkg-{i:04d}.tgz"
            body = f"object {i}\n".encode()
            (self.site / key).write_bytes(body)
            self.keys.append(key)
            remote.append(
                {"key": key, "size": len(body), "etag": r2.md5_hex(self.site / key)}
            )
        self.objects_file = self.root / "objects.json"
        self.objects_file.write_text(json.dumps(remote))
        self.saved_path = os.environ.get("PATH")
        os.environ["PATH"] = f"{self.bin}:{self.saved_path}"
        os.environ["FAKE_CF_OBJECTS"] = str(self.objects_file)
        self.saved_s3 = {name: os.environ.pop(name, None) for name in S3_VARS}
        self.saved_page = r2.PAGE
        r2.PAGE = 100

    def tearDown(self):
        r2.PAGE = self.saved_page
        if self.saved_path is None:
            os.environ.pop("PATH", None)
        else:
            os.environ["PATH"] = self.saved_path
        os.environ.pop("FAKE_CF_OBJECTS", None)
        for name, value in self.saved_s3.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value
        self.work.cleanup()

    def test_lists_past_one_page_without_a_cursor(self):
        index = r2.remote_index("bucket", self.keys)
        self.assertEqual(len(index), len(self.keys))
        for key in self.keys:
            self.assertIn(key, index)

    def test_unchanged_generation_is_skipped_beyond_one_page(self):
        keys_file = self.root / "keys"
        keys_file.write_text("\n".join(self.keys) + "\n")
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            old_argv = sys.argv
            sys.argv = ["r2-plan", str(self.site), "bucket", str(keys_file)]
            try:
                r2.main()
            finally:
                sys.argv = old_argv
        lines = output.getvalue().splitlines()
        self.assertEqual(len(lines), len(self.keys))
        self.assertTrue(all(line.startswith("skip ") for line in lines))


if __name__ == "__main__":
    unittest.main()
