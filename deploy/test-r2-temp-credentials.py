#!/usr/bin/env python3
import os
import pathlib
import shlex
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name("r2-temp-credentials.py")

FAKE_CF = """#!/bin/sh
case "$1 $2" in
  "auth whoami")
    printf '%s\\n' '{"accounts":[{"id":"acct"}]}'
    ;;
  "accounts tokens")
    if [ -n "$FAKE_CF_NO_TOKENS" ]; then
      printf '%s\\n' '[]'
    else
      printf '%s\\n' '[{"id":"other","status":"active","policies":[]},{"id":"r2","status":"active","policies":[{"permission_groups":[{"name":"Workers R2 Storage Write"}]}]}]'
    fi
    ;;
  "r2 temporary-credentials")
    parent=""
    while [ $# -gt 0 ]; do
      if [ "$1" = "--parent-access-key-id" ]; then parent=$2; fi
      shift
    done
    printf '%s\\n' "$parent" >> "$FAKE_CF_LOG"
    if [ -n "$FAKE_CF_ALLOW" ] || [ "$parent" = "r2" ]; then
      printf '%s\\n' '{"accessKeyId":"access","secretAccessKey":"secret","sessionToken":"session"}'
    else
      echo "denied" >&2
      exit 1
    fi
    ;;
  *)
    echo "unexpected cf invocation: $*" >&2
    exit 1
    ;;
esac
"""


class TempCredentialTests(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        root = pathlib.Path(self.work.name)
        bin_dir = root / "bin"
        bin_dir.mkdir()
        cf = bin_dir / "cf"
        cf.write_text(FAKE_CF)
        cf.chmod(0o755)
        self.log = root / "cf.log"
        self.saved = dict(os.environ)
        os.environ["PATH"] = f"{bin_dir}:{os.environ.get('PATH', '')}"
        os.environ["FAKE_CF_LOG"] = str(self.log)
        for name in ("R2_PARENT_ACCESS_KEY_ID", "R2_ACCOUNT_ID", "R2_TEMP_CREDENTIALS_TTL"):
            os.environ.pop(name, None)
        os.environ.pop("FAKE_CF_NO_TOKENS", None)
        os.environ.pop("FAKE_CF_ALLOW", None)

    def tearDown(self):
        os.environ.clear()
        os.environ.update(self.saved)
        self.work.cleanup()

    def run_helper(self):
        return subprocess.run(
            [sys.executable, str(SCRIPT), "bucket"],
            capture_output=True,
            text=True,
        )

    def exports(self, stdout):
        values = {}
        for line in stdout.splitlines():
            parts = shlex.split(line)
            self.assertEqual(len(parts), 2, line)
            self.assertEqual(parts[0], "export", line)
            name, value = parts[1].split("=", 1)
            values[name] = value
        return values

    def test_mints_scoped_credentials_from_the_r2_parent(self):
        done = self.run_helper()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.log.read_text().split(), ["r2"])
        values = self.exports(done.stdout)
        self.assertEqual(
            values,
            {
                "R2_S3_ENDPOINT": "https://acct.r2.cloudflarestorage.com",
                "R2_S3_ACCESS_KEY_ID": "access",
                "R2_S3_SECRET_ACCESS_KEY": "secret",
                "R2_S3_SESSION_TOKEN": "session",
            },
        )

    def test_parent_override_skips_token_discovery(self):
        os.environ["R2_PARENT_ACCESS_KEY_ID"] = "other"
        os.environ["FAKE_CF_ALLOW"] = "1"
        done = self.run_helper()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.log.read_text().split(), ["other"])

    def test_no_active_token_fails_without_printing_credentials(self):
        os.environ["FAKE_CF_NO_TOKENS"] = "1"
        done = self.run_helper()
        self.assertNotEqual(done.returncode, 0)
        self.assertNotIn("secret", done.stdout)
        self.assertIn("no active account token", done.stderr)

    def test_ttl_must_stay_within_the_api_limit(self):
        os.environ["R2_TEMP_CREDENTIALS_TTL"] = "604801"
        done = self.run_helper()
        self.assertNotEqual(done.returncode, 0)
        self.assertIn("R2_TEMP_CREDENTIALS_TTL", done.stderr)


if __name__ == "__main__":
    unittest.main()
