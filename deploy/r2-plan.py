#!/usr/bin/env python3
"""Decide which local files an R2 sync must upload (stdlib only).

Usage: r2-plan.py SITE_DIR BUCKET KEYS_FILE
Env: CF_LOCAL_STATE (local simulation dir), R2_LIST_PAGE (objects per
     listing request, default 1000, the R2 maximum), R2_SINGLE_PUT_MAX
     (bytes, default 300 MiB).

KEYS_FILE holds one object key per line, relative to SITE_DIR. Prints one
line per key: "skip KEY", "put KEY", "multipart KEY SIZE" or "conflict KEY".
A key is skipped when the remote object has the same size and content hash:
MD5 for single-PUT objects, the multipart ETag (MD5 of the part MD5s,
suffixed with the part count) for files above the single-PUT cap. An existing
object with different bytes is a conflict unless R2_REPAIR=1 requests an
explicit overwrite.

Remote objects are discovered by prefix listing, which does not depend on
cursors: a listing shorter than the page size is complete for its prefix;
a full page is split by the next character of the local keys under it.
"""

import hashlib
import importlib.util
import json
import os
import subprocess
import sys

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
PAGE = int(os.environ.get("R2_LIST_PAGE", "1000"))
SINGLE_PUT_MAX = int(os.environ.get("R2_SINGLE_PUT_MAX", str(300 * 1024 * 1024)))


def part_size():
    spec = importlib.util.spec_from_file_location(
        "r2_multipart", os.path.join(SCRIPT_DIR, "r2-multipart.py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.PART_SIZE


def fail(msg):
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def list_prefix(bucket, prefix):
    cmd = ["cf", "r2", "objects", "list", "--bucket-name", bucket, "--per-page", str(PAGE)]
    if prefix:
        cmd += ["--prefix", prefix]
    state = os.environ.get("CF_LOCAL_STATE")
    if state:
        cmd += ["--local", "--persist-to", state]
    done = subprocess.run(cmd, capture_output=True, text=True)
    if done.returncode != 0:
        fail(f"listing prefix '{prefix}' failed: {done.stderr.strip()}")
    try:
        objects = json.loads(done.stdout)
    except json.JSONDecodeError as e:
        fail(f"listing prefix '{prefix}' returned invalid JSON: {e}")
    if len(objects) > PAGE:
        fail(f"listing prefix '{prefix}' returned {len(objects)} objects for page size {PAGE}")
    return objects


def remote_index(bucket, keys):
    index = {}
    pending = [("", keys)]
    while pending:
        prefix, scoped = pending.pop()
        objects = list_prefix(bucket, prefix)
        for o in objects:
            index[o["key"]] = (o["size"], o["etag"].strip('"'))
        if len(objects) < PAGE:
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


def main():
    if len(sys.argv) != 4:
        fail("usage: r2-plan.py SITE_DIR BUCKET KEYS_FILE")
    site, bucket, keys_file = sys.argv[1:4]
    with open(keys_file) as f:
        keys = sorted(line.rstrip("\n") for line in f if line.strip())
    index = remote_index(bucket, keys)
    part = part_size()
    out = []
    for key in keys:
        path = os.path.join(site, key)
        size = os.path.getsize(path)
        large = size > SINGLE_PUT_MAX
        remote = index.get(key)
        unchanged = False
        conflict = False
        if remote is not None and remote[0] == size:
            local = multipart_etag(path, part) if large else md5_hex(path)
            unchanged = remote[1] == local
            conflict = not unchanged
        elif remote is not None:
            conflict = True
        if unchanged:
            out.append(f"skip {key}")
        elif conflict:
            if os.environ.get("R2_REPAIR") == "1":
                out.append(f"multipart {key} {size}" if large else f"put {key}")
            else:
                out.append(f"conflict {key}")
        elif large:
            out.append(f"multipart {key} {size}")
        else:
            out.append(f"put {key}")
    sys.stdout.write("\n".join(out) + ("\n" if out else ""))


if __name__ == "__main__":
    main()
