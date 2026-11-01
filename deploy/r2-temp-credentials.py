#!/usr/bin/env python3
"""Mint short-lived R2 S3 credentials from the authenticated cf CLI.

Usage: r2-temp-credentials.py BUCKET

Prints shell export lines for R2_S3_ENDPOINT, R2_S3_ACCESS_KEY_ID,
R2_S3_SECRET_ACCESS_KEY and R2_S3_SESSION_TOKEN. The credentials are scoped to
one bucket with object read and write, and expire after
R2_TEMP_CREDENTIALS_TTL seconds (default 3600).

The parent R2 access key ID comes from R2_PARENT_ACCESS_KEY_ID when set, and
otherwise from the first active account token whose policies mention R2. The
account for the S3 endpoint comes from R2_ACCOUNT_ID when set, and otherwise
from `cf auth whoami`.
"""

import json
import os
import shlex
import subprocess
import sys
import time

TTL_DEFAULT = 3600
MAX_TTL = 604800
RETRIES = 3


class CommandError(Exception):
    pass


def fail(message):
    print(f"r2-temp-credentials: {message}", file=sys.stderr)
    sys.exit(1)


def run(args):
    for attempt in range(1, RETRIES + 1):
        done = subprocess.run(args, capture_output=True, text=True)
        if done.returncode == 0:
            return done.stdout
        detail = (done.stderr or done.stdout).strip()
        rate_limited = "429" in detail or "rate limit" in detail.lower()
        if rate_limited and attempt < RETRIES:
            time.sleep(2**attempt)
            continue
        raise CommandError(detail or f"{' '.join(args[:3])} failed")
    raise CommandError(f"{' '.join(args[:3])} failed")


def account_id():
    configured = os.environ.get("R2_ACCOUNT_ID")
    if configured:
        return configured
    try:
        document = json.loads(run(["cf", "auth", "whoami"]))
    except json.JSONDecodeError as e:
        raise CommandError(f"cf auth whoami returned invalid JSON: {e}")
    accounts = document.get("accounts") or []
    if not accounts or not accounts[0].get("id"):
        fail("cf auth whoami returned no account")
    return accounts[0]["id"]


def parent_keys():
    configured = os.environ.get("R2_PARENT_ACCESS_KEY_ID")
    if configured:
        return [configured]
    try:
        document = json.loads(
            run(["cf", "accounts", "tokens", "list", "--per-page", "50"])
        )
    except json.JSONDecodeError as e:
        raise CommandError(f"cf accounts tokens list returned invalid JSON: {e}")
    tokens = document.get("result", []) if isinstance(document, dict) else document
    active = [
        token
        for token in tokens
        if token.get("status") == "active" and token.get("id")
    ]
    active.sort(
        key=lambda token: "r2" in json.dumps(token.get("policies", [])).lower(),
        reverse=True,
    )
    return [token["id"] for token in active]


def ttl():
    value = int(os.environ.get("R2_TEMP_CREDENTIALS_TTL", str(TTL_DEFAULT)))
    if value <= 0 or value > MAX_TTL:
        fail(f"R2_TEMP_CREDENTIALS_TTL must be between 1 and {MAX_TTL}")
    return value


def mint(bucket, parent):
    output = run(
        [
            "cf",
            "r2",
            "temporary-credentials",
            "create",
            "--parent-access-key-id",
            parent,
            "--bucket",
            bucket,
            "--permission",
            "object-read-write",
            "--ttl-seconds",
            str(ttl()),
        ]
    )
    try:
        document = json.loads(output)
    except json.JSONDecodeError:
        fail("cf returned credentials that are not valid JSON")
    for name in ("accessKeyId", "secretAccessKey", "sessionToken"):
        if not document.get(name):
            fail(f"cf returned no {name}")
    return document


def main():
    if len(sys.argv) != 2:
        fail("usage: r2-temp-credentials.py BUCKET")
    bucket = sys.argv[1]
    if not bucket:
        fail("bucket is empty")
    try:
        account = account_id()
    except CommandError as e:
        fail(f"cannot determine the R2 account: {e}")
    try:
        parents = parent_keys()
    except CommandError as e:
        fail(f"cannot list account tokens: {e}")
    last = ""
    for parent in parents:
        try:
            credentials = mint(bucket, parent)
        except CommandError as e:
            last = str(e)
            continue
        values = {
            "R2_S3_ENDPOINT": f"https://{account}.r2.cloudflarestorage.com",
            "R2_S3_ACCESS_KEY_ID": credentials["accessKeyId"],
            "R2_S3_SECRET_ACCESS_KEY": credentials["secretAccessKey"],
            "R2_S3_SESSION_TOKEN": credentials["sessionToken"],
        }
        for name, value in values.items():
            print(f"export {name}={shlex.quote(value)}")
        return 0
    fail(last or "no active account token can mint R2 credentials")


if __name__ == "__main__":
    main()
