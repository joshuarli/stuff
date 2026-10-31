#!/usr/bin/env python3
"""Write the immutable object manifest for one published generation.

Usage: r2-manifest.py SITE_DIR MANIFEST_PATH KEYS_FILE
Env: R2_SINGLE_PUT_MAX (bytes, default 300 MiB)

KEYS_FILE holds bucket keys relative to SITE_DIR. The manifest records each
object's size and expected remote ETag (whole-file MD5 for single PUTs, the
multipart ETag for larger files) so promotion can verify the generation
without re-downloading it.
"""

import json
import os
import sys

from r2_common import PART_SIZE, fail, md5_hex, multipart_etag, single_put_max


def main():
    if len(sys.argv) != 4:
        fail("usage: r2-manifest.py SITE_DIR MANIFEST_PATH KEYS_FILE")
    site, manifest_path, keys_file = sys.argv[1:4]
    part = PART_SIZE
    single_max = single_put_max()
    with open(keys_file) as f:
        keys = sorted(line.rstrip("\n") for line in f if line.strip())
    objects = []
    for key in keys:
        path = os.path.join(site, key)
        size = os.path.getsize(path)
        etag = multipart_etag(path, part) if size > single_max else md5_hex(path)
        objects.append({"key": key, "size": size, "etag": etag})
    document = {"schemaVersion": 1, "objects": objects}
    with open(manifest_path, "w", encoding="utf-8", newline="\n") as f:
        json.dump(document, f, indent=2, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
