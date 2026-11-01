#!/usr/bin/env python3
"""Refuse a promotion whose catalog drops a version the live catalog serves.

Usage: r2-catalog-guard.py SITE_DIR BUCKET GENERATION_ID [--allow-drops]
Env: R2_S3_* reads the bucket through the S3 API; otherwise the `cf` CLI
     (CF_LOCAL_STATE selects the local simulation).

Reads the bucket's `current` marker and that generation's catalog.json, then
compares it with SITE_DIR/generations/GENERATION_ID/catalog.json. Every
(source, package, version) in the live catalog must be in the new one. Yank
changes are not drops. A bucket with no `current`, or already on the new
generation, passes. Read-only against the bucket.
"""

import json
import os
import sys

from r2_common import fail, load_sibling, s3_ready

LISTED_DROPS = 20


def read_remote(bucket, key):
    promote = load_sibling("r2_promote", "r2-promote.py")
    if s3_ready():
        found = promote.s3_get(bucket, key)
        return None if found is None else found[0]
    return promote.local_get(bucket, key)


def versions(text, origin):
    try:
        document = json.loads(text)
    except json.JSONDecodeError as e:
        fail(f"{origin} is invalid JSON: {e}")
    packages = document.get("packages") if isinstance(document, dict) else None
    if not isinstance(packages, list):
        fail(f"{origin} has no packages list")
    found = set()
    for package in packages:
        try:
            name = package["name"]
            source = package.get("source")
            for entry in package["versions"]:
                found.add((source or "", name, entry["version"]))
        except (KeyError, TypeError, AttributeError):
            fail(f"{origin} has a malformed package entry")
    return found


def main():
    args = sys.argv[1:]
    allow = "--allow-drops" in args
    args = [a for a in args if a != "--allow-drops"]
    if len(args) != 3:
        fail("usage: r2-catalog-guard.py SITE_DIR BUCKET GENERATION_ID [--allow-drops]")
    site_dir, bucket, generation = args

    current = read_remote(bucket, "current")
    current = current.strip() if current else ""
    if not current or current == generation:
        return

    live_key = f"generations/{current}/catalog.json"
    live_text = read_remote(bucket, live_key)
    if live_text is None:
        if allow:
            print(f"note: live generation {current} has no catalog.json; dropped versions cannot be checked")
            return
        fail(
            f"live generation {current} has no catalog.json, so dropped versions cannot be checked; "
            "pass --allow-drops to promote anyway"
        )
    live = versions(live_text, f"remote {live_key}")

    local_path = os.path.join(site_dir, "generations", generation, "catalog.json")
    try:
        with open(local_path, encoding="utf-8") as f:
            local_text = f.read()
    except OSError as e:
        fail(f"cannot read {local_path}: {e}")
    dropped = sorted(live - versions(local_text, local_path))
    if not dropped:
        return

    shown = "\n".join(
        f"  {name} {version}" + (f" ({source})" if source else "")
        for source, name, version in dropped[:LISTED_DROPS]
    )
    more = len(dropped) - LISTED_DROPS
    if more > 0:
        shown += f"\n  ... and {more} more"
    if allow:
        print(f"note: generation {generation} drops {len(dropped)} versions served by {current}:\n{shown}")
        return
    fail(
        f"generation {generation} drops {len(dropped)} versions served by live generation {current}:\n"
        f"{shown}\n"
        "re-adopt the live publication into the local state, or pass --allow-drops to remove them on purpose"
    )


if __name__ == "__main__":
    main()
