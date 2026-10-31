#!/usr/bin/env python3
"""Opt-in live R2 multipart acceptance test.

Usage: R2_ACCEPTANCE_BUCKET=<bucket> python3 deploy/test-r2-multipart-live.py
Env: R2_S3_ENDPOINT, R2_S3_ACCESS_KEY_ID, R2_S3_SECRET_ACCESS_KEY,
     R2_S3_REGION, optional R2_S3_SESSION_TOKEN,
     R2_ACCEPTANCE_BUCKET (required), R2_ACCEPTANCE_PREFIX
     (default "_stuff-acceptance").

Uploads a small random object through r2-multipart.py, reads it back with a
signed GET, and deletes it. The bucket must permit those operations.
"""

import hashlib
import importlib.util
import os
import pathlib
import sys
import tempfile
import uuid

SCRIPT_DIR = pathlib.Path(__file__).resolve().parent


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, SCRIPT_DIR / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def fail(msg):
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def main():
    bucket = os.environ.get("R2_ACCEPTANCE_BUCKET")
    if not bucket:
        fail("R2_ACCEPTANCE_BUCKET is not set")
    for var in ("R2_S3_ENDPOINT", "R2_S3_ACCESS_KEY_ID", "R2_S3_SECRET_ACCESS_KEY"):
        if not os.environ.get(var):
            fail(f"{var} is not set")
    prefix = os.environ.get("R2_ACCEPTANCE_PREFIX", "_stuff-acceptance").strip("/") or "_stuff-acceptance"
    key = f"{prefix}/{uuid.uuid4().hex}.bin"
    multipart = load("r2_multipart", "r2-multipart.py")
    endpoint = os.environ["R2_S3_ENDPOINT"].rstrip("/")
    quoted = "/".join(multipart.quote(part) for part in key.split("/"))
    url = f"{endpoint}/{bucket}/{quoted}"
    payload = os.urandom(1024 * 1024)
    fd, path = tempfile.mkstemp(prefix="stuff-multipart-live-")
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(payload)
        sys.argv = ["r2-multipart.py", path, bucket, key, "application/octet-stream"]
        multipart.main()
        status, body, _ = multipart.request("GET", url)
        if status != 200:
            fail(f"GET failed: HTTP {status}")
        if hashlib.sha256(body).digest() != hashlib.sha256(payload).digest():
            fail("downloaded bytes differ from the uploaded payload")
        print(f"live multipart acceptance ok: {key}")
    finally:
        try:
            multipart.request("DELETE", url)
        finally:
            os.unlink(path)


if __name__ == "__main__":
    main()
