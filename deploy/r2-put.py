#!/usr/bin/env python3
"""Upload site files to R2 and print each completed key.

Usage: r2-put.py --type KEY
       r2-put.py SITE_DIR BUCKET KEYS_FILE

KEYS_FILE holds one object key per line, relative to SITE_DIR.

Uploads use the S3 API when R2_S3_ENDPOINT, R2_S3_ACCESS_KEY_ID and
R2_S3_SECRET_ACCESS_KEY are set, and the authenticated `cf` CLI otherwise.
CF_LOCAL_STATE always keeps uploads on the local `cf` simulation. The cf path
shares one request pacer because the Cloudflare API allows roughly four
requests a second per account; the S3 path runs R2_PUT_JOBS requests in
parallel (default 8, cf default 4, local default 1). R2_PUT_ATTEMPTS defaults
to 6.
"""

import hashlib
import http.client
import os
import subprocess
import sys
import threading
import time
import urllib.parse
from concurrent.futures import ThreadPoolExecutor, as_completed

from r2_common import (
    Pacer,
    cf_command,
    fail,
    load_sibling,
    object_url,
    safe_relative,
    s3_ready,
)

HASH_CHUNK = 1 << 20
SEND_CHUNK = 1 << 20
THREAD = threading.local()
CONNECTIONS = set()
CONNECTIONS_LOCK = threading.Lock()
MULTIPART = None


def content_type(key):
    if key.endswith(".json"):
        return "application/json"
    if key.endswith(".html"):
        return "text/html; charset=utf-8"
    if key.endswith((".txt", ".md")):
        return "text/markdown; charset=utf-8"
    if key.endswith(".toml"):
        return "text/plain; charset=utf-8"
    if key.endswith(".crate"):
        return "application/x-tar"
    if key.endswith((".tgz", ".whl", ".gz")):
        return "application/octet-stream"
    if key.endswith(".metadata"):
        return "text/plain; charset=utf-8"
    return "application/octet-stream"


def multipart():
    global MULTIPART
    if MULTIPART is None:
        MULTIPART = load_sibling("r2_multipart", "r2-multipart.py")
    return MULTIPART


class PutError(Exception):
    def __init__(self, detail, retryable=True, retry_after=None):
        super().__init__(detail)
        self.detail = detail
        self.retryable = retryable
        self.retry_after = retry_after


def reset_connection():
    current = getattr(THREAD, "connection", None)
    if current is not None:
        current.close()
        THREAD.connection = None
        with CONNECTIONS_LOCK:
            CONNECTIONS.discard(current)


def close_connections():
    with CONNECTIONS_LOCK:
        for current in CONNECTIONS:
            current.close()
        CONNECTIONS.clear()


def connection(parsed):
    current = getattr(THREAD, "connection", None)
    if current is not None and getattr(THREAD, "authority", None) == parsed.netloc:
        return current
    reset_connection()
    if parsed.scheme == "https":
        current = http.client.HTTPSConnection(parsed.hostname, parsed.port, timeout=120)
    else:
        current = http.client.HTTPConnection(parsed.hostname, parsed.port, timeout=120)
    THREAD.connection = current
    THREAD.authority = parsed.netloc
    with CONNECTIONS_LOCK:
        CONNECTIONS.add(current)
    return current


def s3_put(path, bucket, key, kind):
    endpoint = os.environ["R2_S3_ENDPOINT"].rstrip("/")
    url = object_url(endpoint, bucket, key)
    sha = hashlib.sha256()
    md5 = hashlib.md5()
    size = 0
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(HASH_CHUNK), b""):
            sha.update(chunk)
            md5.update(chunk)
            size += len(chunk)
    headers = multipart().signature_headers("PUT", url, sha.hexdigest(), kind)
    headers["Content-Type"] = kind
    headers["Content-Length"] = str(size)
    parsed = urllib.parse.urlsplit(url)
    target = parsed.path or "/"
    if parsed.query:
        target += "?" + parsed.query
    try:
        conn = connection(parsed)
        conn.putrequest("PUT", target, skip_accept_encoding=True)
        for name, value in headers.items():
            conn.putheader(name, value)
        conn.endheaders()
        with open(path, "rb") as f:
            for chunk in iter(lambda: f.read(SEND_CHUNK), b""):
                conn.send(chunk)
        response = conn.getresponse()
        body = response.read()
        status = response.status
        etag = (response.getheader("ETag") or "").strip('"')
        retry_after = response.getheader("Retry-After")
    except (OSError, http.client.HTTPException) as e:
        reset_connection()
        raise PutError(f"{type(e).__name__}: {e}")
    if status != 200:
        if status == 429 or status >= 500:
            reset_connection()
            raise PutError(
                f"HTTP {status}: {body[:200]!r}",
                retry_after=int(retry_after) if retry_after and retry_after.isdigit() else None,
            )
        raise PutError(f"HTTP {status}: {body[:200]!r}", retryable=False)
    if "-" not in etag and etag != md5.hexdigest():
        raise PutError(
            f"ETag mismatch: expected {md5.hexdigest()}, got {etag}", retryable=False
        )


def cf_put(path, bucket, key, kind, pacer):
    command = cf_command(
        ["put", key, "--file", path, "--content-type", kind], bucket
    )
    if pacer is not None:
        pacer.wait()
    done = subprocess.run(command, capture_output=True, text=True)
    if done.returncode == 0:
        return
    detail = (done.stderr or done.stdout).strip() or f"cf exited {done.returncode}"
    if "429" in detail or "rate limit" in detail.lower():
        raise PutError(detail, retry_after=15)
    raise PutError(detail)


def put_one(site, bucket, key, transport, pacer):
    path = os.path.join(site, safe_relative(key))
    kind = content_type(key)
    attempts = int(os.environ.get("R2_PUT_ATTEMPTS", "6"))
    for attempt in range(1, attempts + 1):
        try:
            if transport == "s3":
                s3_put(path, bucket, key, kind)
            else:
                cf_put(path, bucket, key, kind, pacer)
            sys.stdout.write(f"put {key}\n")
            sys.stdout.flush()
            return
        except PutError as e:
            if not e.retryable or attempt == attempts:
                raise PutError(e.detail, retryable=False)
            delay = e.retry_after if e.retry_after is not None else min(60, 2**attempt)
            if pacer is not None and e.retry_after is not None:
                pacer.penalize(delay)
            else:
                time.sleep(delay)


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if len(argv) == 2 and argv[0] == "--type":
        sys.stdout.write(content_type(argv[1]))
        return 0
    if len(argv) != 3:
        fail("usage: r2-put.py --type KEY | r2-put.py SITE_DIR BUCKET KEYS_FILE")
    site, bucket, keys_file = argv
    with open(keys_file, encoding="utf-8") as f:
        keys = [line.strip() for line in f if line.strip()]
    if not keys:
        return 0
    if os.environ.get("CF_LOCAL_STATE"):
        transport = "cf"
        pacer = Pacer(0)
        default_jobs = "1"
    elif s3_ready():
        transport = "s3"
        pacer = None
        default_jobs = "8"
    else:
        transport = "cf"
        pacer = Pacer(float(os.environ.get("R2_PUT_INTERVAL", "0.28")))
        default_jobs = "4"
    jobs = int(os.environ.get("R2_PUT_JOBS", default_jobs))
    failures = []
    with ThreadPoolExecutor(max_workers=jobs) as pool:
        pending = {
            pool.submit(put_one, site, bucket, key, transport, pacer): key
            for key in keys
        }
        for future in as_completed(pending):
            try:
                future.result()
            except PutError as e:
                failures.append((pending[future], e.detail))
    close_connections()
    if failures:
        for key, detail in failures:
            print(f"FAIL: put {key}: {detail}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
