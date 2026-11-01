#!/usr/bin/env python3
"""Promote one fully uploaded generation, atomically.

Usage: r2-promote.py BUCKET GENERATION_ID [--expected GENERATION_ID]
Env: R2_S3_ENDPOINT, R2_S3_ACCESS_KEY_ID, R2_S3_SECRET_ACCESS_KEY,
     R2_S3_REGION, optional R2_S3_SESSION_TOKEN.
     CF_LOCAL_STATE switches to the local `cf` simulation (no conditional
     writes; for tests only).
     R2_PROMOTE_UNCONDITIONAL=1 promotes through the authenticated `cf` CLI
     against the real bucket instead: the same verification, but the marker
     write is a plain put, so a competing publisher is not detected.

The remote generation must carry manifests/<id>.json. Every object it lists
is verified by size and ETag before `current` is touched. The marker write is
conditional: If-Match on the ETag read before verification, or If-None-Match:*
for the first publication. A competing publication therefore receives 412
instead of silently overwriting the winner.
"""

import json
import os
import subprocess
import sys
import tempfile

from r2_common import cf_command, fail, list_page, load_sibling, object_url, remote_index
from r2_common import safe_object_key


def load_multipart():
    return load_sibling("r2_multipart", "r2-multipart.py")


def header(headers, name):
    wanted = name.lower()
    for key, value in headers.items():
        if key.lower() == wanted:
            return value
    return ""


def local_args():
    state = os.environ.get("CF_LOCAL_STATE")
    if state:
        return ["--local", "--persist-to", state]
    if os.environ.get("R2_PROMOTE_UNCONDITIONAL"):
        return []
    return None


def cf(args, bucket=None, text=False):
    done = subprocess.run(cf_command(args, bucket), capture_output=True)
    if done.returncode != 0:
        fail(f"cf {' '.join(args)} failed: {done.stderr.decode('utf-8', 'replace').strip()}")
    if text:
        return done.stdout.decode("utf-8", "replace")
    return done.stdout


def local_get(bucket, key):
    done = subprocess.run(cf_command(["get", key, "--text"], bucket), capture_output=True)
    if done.returncode != 0:
        return None
    return done.stdout.decode("utf-8", "replace")


def local_verify(bucket, manifest):
    keys = [obj["key"] for obj in manifest["objects"]]
    index = remote_index(bucket, keys, list_page())
    for obj in manifest["objects"]:
        key = obj["key"]
        found = index.get(key)
        if found is None:
            fail(f"generation is incomplete: missing {key}")
        if int(found[0]) != obj["size"]:
            fail(f"generation object has the wrong size: {key}")
        if str(found[1]).strip('"') != obj["etag"]:
            fail(f"generation object has the wrong ETag: {key}")


def local_current(bucket):
    text = local_get(bucket, "current")
    if text is None:
        return None
    return text.strip()


def local_put_current(bucket, generation):
    fd, path = tempfile.mkstemp(prefix="stuff-current-")
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as f:
            f.write(generation)
        cf(
            ["put", "current", "--file", path, "--content-type", "text/plain"],
            bucket,
        )
    finally:
        os.unlink(path)


def s3_get(bucket, key):
    multipart = load_multipart()
    endpoint = os.environ["R2_S3_ENDPOINT"].rstrip("/")
    status, body, headers = multipart.request("GET", object_url(endpoint, bucket, key))
    if status == 404:
        return None
    if status != 200:
        fail(f"GET {key} failed: HTTP {status}")
    return body.decode("utf-8", "replace"), headers


def s3_verify(bucket, manifest):
    keys = [obj["key"] for obj in manifest["objects"]]
    index = remote_index(bucket, keys, list_page())
    for obj in manifest["objects"]:
        key = obj["key"]
        found = index.get(key)
        if found is None:
            fail(f"generation is incomplete: missing {key}")
        if int(found[0]) != obj["size"]:
            fail(f"generation object has the wrong size: {key}")
        if str(found[1]).strip('"') != obj["etag"]:
            fail(f"generation object has the wrong ETag: {key}")


def s3_promote(bucket, generation, expected):
    multipart = load_multipart()
    endpoint = os.environ["R2_S3_ENDPOINT"].rstrip("/")
    current_url = object_url(endpoint, bucket, "current")
    status, body, headers = multipart.request("GET", current_url)
    if status == 404:
        current = None
        current_etag = ""
    elif status == 200:
        current = body.decode("utf-8", "replace").strip()
        current_etag = header(headers, "ETag")
    else:
        fail(f"could not read current marker: HTTP {status}")
    if expected is not None and current != expected:
        fail(
            f"remote current is '{current or '<missing>'}', expected '{expected}'; "
            "refusing to overwrite a competing publication"
        )

    manifest_body = s3_get(bucket, f"manifests/{generation}.json")
    if manifest_body is None:
        fail(f"remote generation {generation} has no manifests/{generation}.json")
    manifest = parse_manifest(manifest_body[0], generation)
    s3_verify(bucket, manifest)
    if current == generation:
        return generation

    condition = {"if-none-match": "*"} if current is None else {"if-match": current_etag}
    status, _, _ = multipart.request(
        "PUT",
        current_url,
        generation.encode(),
        "text/plain",
        extra_headers=condition,
    )
    if status == 412:
        fail("current changed during promotion; refusing to overwrite a competing publication")
    if status not in (200, 201, 204):
        fail(f"conditional promotion failed: HTTP {status}")

    status, body, _ = multipart.request("GET", current_url)
    committed = body.decode("utf-8", "replace").strip() if status == 200 else ""
    if committed != generation:
        fail(f"current is '{committed or '<missing>'}' after promotion, expected '{generation}'")
    return committed


def parse_manifest(text, generation):
    try:
        document = json.loads(text)
    except json.JSONDecodeError as e:
        fail(f"manifest for {generation} is invalid JSON: {e}")
    if document.get("schemaVersion") != 1 or not isinstance(document.get("objects"), list):
        fail(f"manifest for {generation} has an unsupported shape")
    for obj in document["objects"]:
        if not isinstance(obj, dict):
            fail(f"manifest for {generation} contains a non-object entry")
        key = obj.get("key")
        if not safe_object_key(key):
            fail(f"manifest for {generation} contains an unsafe key")
        if not isinstance(obj.get("size"), int) or obj["size"] < 0:
            fail(f"manifest for {generation} contains an invalid size")
        if not isinstance(obj.get("etag"), str) or not obj["etag"]:
            fail(f"manifest for {generation} contains an invalid ETag")
    return document


def main():
    if len(sys.argv) < 3 or len(sys.argv) > 5:
        fail("usage: r2-promote.py BUCKET GENERATION_ID [--expected GENERATION_ID]")
    bucket, generation = sys.argv[1:3]
    if not generation or len(generation) > 128 or any(
        char not in "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-"
        for char in generation
    ):
        fail(f"bad generation id: {generation}")
    expected = None
    if len(sys.argv) > 3:
        if len(sys.argv) != 5 or sys.argv[3] != "--expected":
            fail("usage: r2-promote.py BUCKET GENERATION_ID [--expected GENERATION_ID]")
        expected = sys.argv[4]
        if not expected:
            fail("--expected requires a generation id")

    local = local_args()
    if local is not None:
        current = local_current(bucket)
        if expected is not None and current != expected:
            fail(
                f"remote current is '{current or '<missing>'}', expected '{expected}'; "
                "refusing to overwrite a competing publication"
            )
        text = local_get(bucket, f"manifests/{generation}.json")
        if text is None:
            fail(f"remote generation {generation} has no manifests/{generation}.json")
        manifest = parse_manifest(text, generation)
        local_verify(bucket, manifest)
        if current == generation:
            print(f"verified current is {generation}")
            return
        local_put_current(bucket, generation)
        committed = local_current(bucket)
        if committed != generation:
            fail(f"current is '{committed or '<missing>'}' after promotion, expected '{generation}'")
        print(f"verified current is {generation}")
        return

    for var in ("R2_S3_ENDPOINT", "R2_S3_ACCESS_KEY_ID", "R2_S3_SECRET_ACCESS_KEY"):
        if not os.environ.get(var):
            fail(f"{var} is not set; conditional promotion needs the R2 S3 API")
    print(f"verified current is {s3_promote(bucket, generation, expected)}")


if __name__ == "__main__":
    main()
