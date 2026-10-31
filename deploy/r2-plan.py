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

import os
import sys

from r2_common import PART_SIZE, fail, list_page, md5_hex, multipart_etag, single_put_max
from r2_common import remote_index as shared_remote_index

PAGE = list_page()


def part_size():
    return PART_SIZE


def remote_index(bucket, keys):
    return shared_remote_index(bucket, keys, PAGE)


def main():
    if len(sys.argv) != 4:
        fail("usage: r2-plan.py SITE_DIR BUCKET KEYS_FILE")
    site, bucket, keys_file = sys.argv[1:4]
    with open(keys_file) as f:
        keys = sorted(line.rstrip("\n") for line in f if line.strip())
    index = remote_index(bucket, keys)
    part = part_size()
    single_max = single_put_max()
    out = []
    for key in keys:
        path = os.path.join(site, key)
        size = os.path.getsize(path)
        large = size > single_max
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
