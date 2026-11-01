#!/usr/bin/env python3
"""Pull the live publication of one bucket into a local site directory (stdlib only).

Usage: r2-pull.py BUCKET DEST_DIR
Env: CF_LOCAL_STATE (local simulation dir), R2_LIST_PAGE (objects per listing
     request, default 1000), R2_PULL_JOBS (parallel downloads, default 8),
     R2_PULL_CF_ONLY (force the paced cf transport).

Read-only against the bucket. DEST_DIR must not exist or must be empty. Pulls
the generation named by the `current` marker, the manifest of that generation
when the bucket has one, and every shared immutable store (blobs/, tarballs/,
wheels/). Each object is verified by size and ETag against the listing, then
every entry of the generation's manifest is checked against the pulled tree.
Then it restores the layout r2-sync leaves out: the `current` symlink and,
when the bucket has a wheels/ store, the generation's `wheels` symlink to it.
Other generations are not pulled.

Downloads use the S3 API with R2_PULL_JOBS parallel requests when R2_S3_* is
set, or with short-lived bucket-scoped credentials minted from the
authenticated cf CLI otherwise. If minting fails, it falls back to the cf CLI
with one shared pacer.
"""

import hashlib
import json
import os
import re
import shlex
import subprocess
import sys
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor

from r2_common import (
    PART_SIZE,
    Pacer,
    cf_command,
    fail,
    list_page,
    load_sibling,
    md5_hex,
    multipart_etag,
    object_url,
    s3_list,
    s3_ready,
    safe_relative,
)

PAGE = list_page()
JOBS = int(os.environ.get("R2_PULL_JOBS", "8"))
RETRIES = 6
RATE_LIMIT_BACKOFF_SECONDS = 15
GENERATION_ID = re.compile(r"[A-Za-z0-9_-]+")
PACER = Pacer(0 if os.environ.get("CF_LOCAL_STATE") else 0.35)
EMPTY_SHA256 = hashlib.sha256(b"").hexdigest()
S3_API = None


def s3_api():
    global S3_API
    if S3_API is None:
        S3_API = load_sibling("r2_pull_multipart", "r2-multipart.py")
    return S3_API


def enable_s3(bucket):
    if s3_ready():
        return True
    if os.environ.get("CF_LOCAL_STATE") or os.environ.get("R2_PULL_CF_ONLY"):
        return False
    script = os.path.join(os.path.dirname(os.path.abspath(__file__)), "r2-temp-credentials.py")
    done = subprocess.run([sys.executable, script, bucket], capture_output=True, text=True)
    if done.returncode != 0:
        detail = (done.stderr or done.stdout).strip()
        print(f"note: could not mint temporary R2 S3 credentials; using cf downloads", file=sys.stderr)
        if detail:
            print(f"note: {detail}", file=sys.stderr)
        return False
    for line in done.stdout.splitlines():
        if not line.startswith("export "):
            continue
        name, _, raw = line[len("export ") :].partition("=")
        os.environ[name] = shlex.split(raw)[0]
    return s3_ready()


def list_all(bucket):
    if s3_ready():
        return [
            {"key": key, "size": size, "etag": etag}
            for key, (size, etag) in sorted(s3_list(bucket).items())
        ]
    objects = []
    seen = set()
    after = None
    while True:
        args = ["list", "--per-page", str(PAGE)]
        if after:
            args += ["--start-after", after]
        PACER.wait()
        done = subprocess.run(cf_command(args, bucket), capture_output=True, text=True)
        if done.returncode != 0:
            fail(f"listing failed: {done.stderr.strip()}")
        page = json.loads(done.stdout)
        repeated = next((o["key"] for o in page if o["key"] in seen), None)
        if repeated is not None:
            fail(f"listing repeated {repeated} after {after}")
        seen.update(o["key"] for o in page)
        objects.extend(page)
        if len(page) < PAGE:
            return objects
        after = page[-1]["key"]


def current_generation(bucket):
    if s3_ready():
        endpoint = os.environ["R2_S3_ENDPOINT"].rstrip("/")
        status, body, _ = s3_api().request("GET", object_url(endpoint, bucket, "current"))
        if status != 200:
            fail(f"cannot read the current marker: HTTP {status}")
        generation = body.decode("utf-8", "replace").strip()
    else:
        PACER.wait()
        done = subprocess.run(
            cf_command(["get", "current", "--text"], bucket), capture_output=True, text=True
        )
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


def object_etag(path, etag):
    return multipart_etag(path, PART_SIZE) if "-" in etag else md5_hex(path)


def download(bucket, dest, obj):
    key = obj["key"]
    etag = obj["etag"].strip('"')
    path = os.path.join(dest, safe_relative(key))
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = path + ".part"
    if s3_ready():
        download_s3(bucket, key, path, tmp, obj["size"], etag)
        return
    last = ""
    for attempt in range(1, RETRIES + 1):
        PACER.wait()
        with open(tmp, "wb") as out:
            done = subprocess.run(
                cf_command(["get", key], bucket), stdout=out, stderr=subprocess.PIPE
            )
        if done.returncode != 0:
            last = done.stderr.decode("utf-8", "replace").strip()
            if "429" in last:
                PACER.penalize(min(60, RATE_LIMIT_BACKOFF_SECONDS * attempt))
            continue
        if os.path.getsize(tmp) != obj["size"] or object_etag(tmp, etag) != etag:
            last = "size or ETag differs from the listing"
            continue
        os.rename(tmp, path)
        return
    fail(f"could not pull {key}: {last}")


def download_s3(bucket, key, path, tmp, size, etag):
    endpoint = os.environ["R2_S3_ENDPOINT"].rstrip("/")
    url = object_url(endpoint, bucket, key)
    last = ""
    for attempt in range(1, RETRIES + 1):
        request = urllib.request.Request(
            url,
            headers=s3_api().signature_headers("GET", url, EMPTY_SHA256),
            method="GET",
        )
        try:
            with urllib.request.urlopen(request) as response, open(tmp, "wb") as out:
                digest = hashlib.md5()
                received = 0
                for chunk in iter(lambda: response.read(1 << 20), b""):
                    out.write(chunk)
                    digest.update(chunk)
                    received += len(chunk)
        except urllib.error.HTTPError as e:
            detail = e.read().decode("utf-8", "replace").strip()
            last = f"HTTP {e.code}{': ' + detail if detail else ''}"
            if e.code == 429 or e.code >= 500:
                time.sleep(min(60, RATE_LIMIT_BACKOFF_SECONDS * attempt))
                continue
            break
        except (urllib.error.URLError, OSError) as e:
            last = str(e)
            time.sleep(min(60, RATE_LIMIT_BACKOFF_SECONDS * attempt))
            continue
        actual = multipart_etag(tmp, PART_SIZE) if "-" in etag else digest.hexdigest()
        if received != size or actual != etag:
            last = "size or ETag differs from the listing"
            continue
        os.rename(tmp, path)
        return
    fail(f"could not pull {key}: {last}")


def verify_manifest(dest, generation):
    manifest = os.path.join(dest, "manifests", f"{generation}.json")
    if not os.path.isfile(manifest):
        print(
            f"note: generation {generation} has no manifest; object verification is skipped",
            file=sys.stderr,
        )
        return 0
    try:
        with open(manifest, encoding="utf-8") as f:
            document = json.load(f)
    except (OSError, json.JSONDecodeError) as e:
        fail(f"cannot read the manifest for generation {generation}: {e}")
    if not isinstance(document, dict) or document.get("schemaVersion") != 1:
        fail(f"manifest for generation {generation} has an unsupported schema")
    objects = document.get("objects")
    if not isinstance(objects, list):
        fail(f"manifest for generation {generation} has no object list")
    for entry in objects:
        if not isinstance(entry, dict):
            fail(f"manifest for generation {generation} has an invalid object entry")
        key = entry.get("key")
        size = entry.get("size")
        etag = entry.get("etag")
        if not isinstance(key, str) or not isinstance(size, int) or not isinstance(etag, str):
            fail(f"manifest for generation {generation} has an incomplete object entry")
        local = os.path.join(dest, safe_relative(key))
        try:
            actual_size = os.path.getsize(local)
        except OSError:
            fail(f"manifest lists {key}, which was not pulled for generation {generation}")
        if actual_size != size:
            fail(
                f"{key} is {actual_size} bytes, but its manifest records {size} for generation {generation}"
            )
        if object_etag(local, etag) != etag:
            fail(f"{key} differs from its manifest ETag for generation {generation}")
    return len(objects)


def main():
    if len(sys.argv) != 3:
        fail("usage: r2-pull.py BUCKET DEST_DIR")
    bucket, dest = sys.argv[1:3]
    if os.path.lexists(dest) and (not os.path.isdir(dest) or os.listdir(dest)):
        fail(f"{dest} exists and is not an empty directory")
    enable_s3(bucket)
    generation = current_generation(bucket)
    objects = [o for o in list_all(bucket) if wanted(o["key"], generation)]
    if not any(o["key"].startswith(f"generations/{generation}/") for o in objects):
        fail(f"generation {generation} has no objects in {bucket}")
    os.makedirs(dest, exist_ok=True)
    with ThreadPoolExecutor(max_workers=JOBS) as pool:
        for _ in pool.map(lambda o: download(bucket, dest, o), objects):
            pass
    verified = verify_manifest(dest, generation)
    os.symlink(f"generations/{generation}", os.path.join(dest, "current"))
    wheels_store = os.path.join(dest, "wheels")
    wheels_alias = os.path.join(dest, "generations", generation, "wheels")
    if os.path.isdir(wheels_store) and not os.path.lexists(wheels_alias):
        os.symlink("../../wheels", wheels_alias)
    total = sum(o["size"] for o in objects)
    print(f"pulled {len(objects)} objects ({total} bytes) from {bucket}; current is {generation}")
    if verified:
        print(f"verified {verified} manifest objects against the pulled tree")


if __name__ == "__main__":
    main()
