#!/usr/bin/env python3
"""Shared stdlib-only infrastructure for the R2 deployment helpers."""

import hashlib
import importlib.util
import json
import os
import subprocess
import sys
import threading
import time
import urllib.parse
import xml.etree.ElementTree as ET

PART_SIZE = 64 * 1024 * 1024
DEFAULT_SINGLE_PUT_MAX = 300 * 1024 * 1024
DEFAULT_LIST_PAGE = 1000
S3_CREDENTIALS = ("R2_S3_ENDPOINT", "R2_S3_ACCESS_KEY_ID", "R2_S3_SECRET_ACCESS_KEY")


def fail(msg):
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def load_sibling(name, filename):
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), filename)
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def list_page():
    return int(os.environ.get("R2_LIST_PAGE", str(DEFAULT_LIST_PAGE)))


def single_put_max():
    return int(os.environ.get("R2_SINGLE_PUT_MAX", str(DEFAULT_SINGLE_PUT_MAX)))


def s3_ready():
    if os.environ.get("CF_LOCAL_STATE"):
        return False
    return all(os.environ.get(name) for name in S3_CREDENTIALS)


class Pacer:
    def __init__(self, interval):
        self.interval = interval
        self.lock = threading.Lock()
        self.next_slot = 0.0

    def wait(self):
        with self.lock:
            now = time.monotonic()
            slot = max(now, self.next_slot)
            self.next_slot = slot + self.interval
        if slot > now:
            time.sleep(slot - now)

    def penalize(self, seconds):
        with self.lock:
            self.next_slot = max(self.next_slot, time.monotonic() + seconds)


def cf_command(args, bucket=None):
    command = ["cf", "r2", "objects", *args]
    if bucket is not None:
        command += ["--bucket-name", bucket]
    state = os.environ.get("CF_LOCAL_STATE")
    if state:
        command += ["--local", "--persist-to", state]
    return command


def list_prefix(bucket, prefix, page):
    args = ["list", "--per-page", str(page)]
    if prefix:
        args += ["--prefix", prefix]
    done = subprocess.run(cf_command(args, bucket), capture_output=True, text=True)
    if done.returncode != 0:
        fail(f"listing prefix '{prefix}' failed: {done.stderr.strip()}")
    try:
        objects = json.loads(done.stdout)
    except json.JSONDecodeError as e:
        fail(f"listing prefix '{prefix}' returned invalid JSON: {e}")
    if len(objects) > page:
        fail(f"listing prefix '{prefix}' returned {len(objects)} objects for page size {page}")
    return objects


def _child_text(element, name):
    for child in element:
        if child.tag == name or child.tag.endswith("}" + name):
            return (child.text or "").strip()
    return ""


def s3_list(bucket):
    multipart = load_sibling("r2_multipart", "r2-multipart.py")
    endpoint = os.environ["R2_S3_ENDPOINT"].rstrip("/")
    base = f"{endpoint}/{quote(bucket)}"
    page = min(list_page(), 1000)
    objects = {}
    token = ""
    while True:
        query = f"list-type=2&max-keys={page}"
        if token:
            query += f"&continuation-token={quote(token)}"
        status, body, _ = multipart.request("GET", f"{base}?{query}")
        if status != 200:
            fail(f"listing bucket '{bucket}' failed: HTTP {status}")
        try:
            root = ET.fromstring(body)
        except ET.ParseError as e:
            fail(f"listing bucket '{bucket}' returned invalid XML: {e}")
        for contents in root:
            if contents.tag != "Contents" and not contents.tag.endswith("}Contents"):
                continue
            key = _child_text(contents, "Key")
            size = _child_text(contents, "Size")
            etag = _child_text(contents, "ETag")
            if not key or not size or not etag:
                fail(f"listing bucket '{bucket}' returned an incomplete object")
            try:
                objects[key] = (int(size), etag.strip('"'))
            except ValueError:
                fail(f"listing bucket '{bucket}' returned an invalid size for {key}")
        truncated = multipart.find_text(root, "IsTruncated").lower() == "true"
        next_token = multipart.find_text(root, "NextContinuationToken")
        if not next_token:
            if truncated:
                fail(f"listing bucket '{bucket}' was truncated without a continuation token")
            return objects
        if next_token == token:
            fail(f"listing bucket '{bucket}' repeated a continuation token")
        token = next_token


def remote_index(bucket, keys, page):
    if s3_ready():
        objects = s3_list(bucket)
        return {key: objects[key] for key in keys if key in objects}
    index = {}
    pending = [("", keys)]
    while pending:
        prefix, scoped = pending.pop()
        objects = list_prefix(bucket, prefix, page)
        for o in objects:
            index[o["key"]] = (o["size"], o["etag"].strip('"'))
        if len(objects) < page:
            continue
        children = {}
        for k in scoped:
            if len(k) > len(prefix):
                children.setdefault(k[: len(prefix) + 1], []).append(k)
        pending.extend(children.items())
    return index


def md5_hex(path):
    h = hashlib.md5()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def multipart_etag(path, part):
    digests = []
    with open(path, "rb") as f:
        while True:
            chunk = f.read(part)
            if not chunk:
                break
            digests.append(hashlib.md5(chunk).digest())
    return f"{hashlib.md5(b''.join(digests)).hexdigest()}-{len(digests)}"


def quote(value):
    return urllib.parse.quote(value, safe="-_.~")


def object_url(endpoint, bucket, key):
    quoted = "/".join(quote(part) for part in key.split("/"))
    return f"{endpoint.rstrip('/')}/{quote(bucket)}/{quoted}"


def safe_object_key(key):
    return (
        isinstance(key, str)
        and bool(key)
        and not key.startswith("/")
        and all(part not in ("", ".", "..") for part in key.split("/"))
    )


def safe_relative(key):
    if not safe_object_key(key):
        fail(f"unsafe object key: {key!r}")
    return os.path.join(*key.split("/"))
