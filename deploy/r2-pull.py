#!/usr/bin/env python3
"""Pull the live publication of one bucket into a local site directory (stdlib only).

Usage: r2-pull.py BUCKET DEST_DIR
Env: CF_LOCAL_STATE (local simulation dir), R2_LIST_PAGE (objects per listing
     request, default 1000), R2_PULL_JOBS (parallel downloads, default 8).

Read-only against the bucket. DEST_DIR must not exist or must be empty. Pulls
the generation named by the `current` marker, the manifest of that generation
when the bucket has one, and every shared immutable store (blobs/, tarballs/,
wheels/). Each object is verified by size and MD5 ETag against the listing.
Then it restores the layout r2-sync leaves out: the `current` symlink and,
when the bucket has a wheels/ store, the generation's `wheels` symlink to it.
Other generations are not pulled.
"""

import hashlib
import json
import os
import re
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor

PAGE = int(os.environ.get("R2_LIST_PAGE", "1000"))
JOBS = int(os.environ.get("R2_PULL_JOBS", "8"))
RETRIES = 4
GENERATION_ID = re.compile(r"[A-Za-z0-9_-]+")


def fail(msg):
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def cf(args, bucket):
    cmd = ["cf", "r2", "objects"] + args + ["--bucket-name", bucket]
    state = os.environ.get("CF_LOCAL_STATE")
    if state:
        cmd += ["--local", "--persist-to", state]
    return cmd


def list_all(bucket):
    objects = []
    after = None
    while True:
        args = ["list", "--per-page", str(PAGE)]
        if after:
            args += ["--start-after", after]
        done = subprocess.run(cf(args, bucket), capture_output=True, text=True)
        if done.returncode != 0:
            fail(f"listing failed: {done.stderr.strip()}")
        page = json.loads(done.stdout)
        if page and after and page[0]["key"] <= after:
            fail(f"listing did not advance past {after}")
        objects.extend(page)
        if len(page) < PAGE:
            return objects
        after = page[-1]["key"]


def current_generation(bucket):
    done = subprocess.run(cf(["get", "current", "--text"], bucket), capture_output=True, text=True)
    if done.returncode != 0:
        fail(f"cannot read the current marker: {done.stderr.strip()}")
    generation = done.stdout.strip()
    if not GENERATION_ID.fullmatch(generation):
        fail(f"bad generation id in the current marker: {generation!r}")
    return generation


def wanted(key, generation):
    if key == "current":
        return False
    if key.startswith("generations/"):
        return key.startswith(f"generations/{generation}/")
    if key.startswith("manifests/"):
        return key == f"manifests/{generation}.json"
    return True


def safe_relative(key):
    parts = key.split("/")
    if key.startswith("/") or any(p in ("", ".", "..") for p in parts):
        fail(f"unsafe object key: {key!r}")
    return os.path.join(*parts)


def download(bucket, dest, obj):
    key = obj["key"]
    etag = obj["etag"].strip('"')
    if "-" in etag:
        fail(f"{key} is a multipart object; its ETag cannot be verified by MD5")
    path = os.path.join(dest, safe_relative(key))
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = path + ".part"
    last = ""
    for _ in range(RETRIES):
        with open(tmp, "wb") as out:
            done = subprocess.run(cf(["get", key], bucket), stdout=out, stderr=subprocess.PIPE)
        if done.returncode != 0:
            last = done.stderr.decode("utf-8", "replace").strip()
            continue
        digest = hashlib.md5()
        with open(tmp, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                digest.update(chunk)
        if os.path.getsize(tmp) != obj["size"] or digest.hexdigest() != etag:
            last = "size or MD5 differs from the listing"
            continue
        os.rename(tmp, path)
        return
    fail(f"could not pull {key}: {last}")


def main():
    if len(sys.argv) != 3:
        fail("usage: r2-pull.py BUCKET DEST_DIR")
    bucket, dest = sys.argv[1:3]
    if os.path.lexists(dest) and (not os.path.isdir(dest) or os.listdir(dest)):
        fail(f"{dest} exists and is not an empty directory")
    generation = current_generation(bucket)
    objects = [o for o in list_all(bucket) if wanted(o["key"], generation)]
    if not any(o["key"].startswith(f"generations/{generation}/") for o in objects):
        fail(f"generation {generation} has no objects in {bucket}")
    os.makedirs(dest, exist_ok=True)
    with ThreadPoolExecutor(max_workers=JOBS) as pool:
        for _ in pool.map(lambda o: download(bucket, dest, o), objects):
            pass
    os.symlink(f"generations/{generation}", os.path.join(dest, "current"))
    wheels_store = os.path.join(dest, "wheels")
    wheels_alias = os.path.join(dest, "generations", generation, "wheels")
    if os.path.isdir(wheels_store) and not os.path.lexists(wheels_alias):
        os.symlink("../../wheels", wheels_alias)
    total = sum(o["size"] for o in objects)
    print(f"pulled {len(objects)} objects ({total} bytes) from {bucket}; current is {generation}")


if __name__ == "__main__":
    main()
