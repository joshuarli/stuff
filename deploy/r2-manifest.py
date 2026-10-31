#!/usr/bin/env python3
"""Write the immutable object manifest for one published generation.

Usage: r2-manifest.py SITE_DIR MANIFEST_PATH KEYS_FILE
Env: R2_SINGLE_PUT_MAX (bytes, default 300 MiB)

KEYS_FILE holds bucket keys relative to SITE_DIR. The manifest records each
object's size and expected remote ETag (whole-file MD5 for single PUTs, the
multipart ETag for larger files) so promotion can verify the generation
without re-downloading it.
"""

import importlib.util
import json
import os
import sys

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))


def load_plan_module():
    spec = importlib.util.spec_from_file_location(
        "r2_plan", os.path.join(SCRIPT_DIR, "r2-plan.py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def fail(msg):
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def main():
    if len(sys.argv) != 4:
        fail("usage: r2-manifest.py SITE_DIR MANIFEST_PATH KEYS_FILE")
    site, manifest_path, keys_file = sys.argv[1:4]
    plan = load_plan_module()
    part = plan.part_size()
    single_max = int(os.environ.get("R2_SINGLE_PUT_MAX", str(300 * 1024 * 1024)))
    with open(keys_file) as f:
        keys = sorted(line.rstrip("\n") for line in f if line.strip())
    objects = []
    for key in keys:
        path = os.path.join(site, key)
        size = os.path.getsize(path)
        etag = plan.multipart_etag(path, part) if size > single_max else plan.md5_hex(path)
        objects.append({"key": key, "size": size, "etag": etag})
    document = {"schemaVersion": 1, "objects": objects}
    with open(manifest_path, "w", encoding="utf-8", newline="\n") as f:
        json.dump(document, f, indent=2, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
