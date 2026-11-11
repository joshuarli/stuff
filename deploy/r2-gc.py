#!/usr/bin/env python3
"""Delete R2 objects no retained generation needs, then report storage before and after.

Usage: r2-gc.py BUCKET... [--keep-previous N] [--keep-newer N|all] [--dry-run]
       [--grace-minutes N] [--jobs N]

The `current` marker and its generation are always kept. Beyond that, the N
most recent generations older than `current` are kept (default one, so one
rollback target survives), and every generation newer than `current` is kept
so a rollback can roll forward again (--keep-newer N keeps only the N most
recent). Manifests outside that set are deleted, then their generation
trees, then shared-store objects no retained manifest references. A
generation directory with no manifest is deleted only after the grace period;
a recent one is a sync in flight and blocks shared-store deletion for that
bucket.

Each bucket is processed under the publish lock of the registry that owns it
in the stuff configuration (`stuff lock-path BUCKET`), the lock `stuff
publish` holds. Before every delete batch the helper re-reads
`current` and the manifest list and aborts if either changed.

Without R2_S3_* it mints bucket-scoped temporary credentials from the
authenticated cf CLI for each bucket.
"""

import argparse
import datetime
import json
import os
import re
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed

from r2_common import fail, mint_s3_credentials, object_url, publish_lock, s3_list_meta, s3_ready, safe_object_key

CURRENT_KEY = "current"
MANIFEST_PREFIX = "manifests/"
GENERATION_PREFIX = "generations/"
GENERATION_ID = re.compile(r"[A-Za-z0-9_-]{1,128}\Z")
S3_VARS = (
    "R2_S3_ENDPOINT",
    "R2_S3_ACCESS_KEY_ID",
    "R2_S3_SECRET_ACCESS_KEY",
    "R2_S3_SESSION_TOKEN",
)
DELETE_RETRIES = 5
DELETE_BATCH = 256
RETRYABLE = {429, 500, 502, 503, 504}


def human_bytes(value):
    if value >= 1_000_000_000:
        return f"{value / 1_000_000_000:.2f} GB"
    if value >= 1_000_000:
        return f"{value / 1_000_000:.1f} MB"
    if value >= 1_000:
        return f"{value / 1_000:.1f} KB"
    return f"{value} B"


def parse_time(value):
    if not value:
        return 0.0
    try:
        return datetime.datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()
    except ValueError:
        fail(f"invalid R2 LastModified value: {value!r}")


def clear_s3():
    for name in S3_VARS:
        os.environ.pop(name, None)


def mint_credentials(bucket):
    detail = mint_s3_credentials(bucket)
    if detail is not None:
        fail(f"could not mint temporary R2 credentials for {bucket}: {detail}")


def load_api():
    from r2_common import load_sibling
    return load_sibling("r2_gc_multipart", "r2-multipart.py")


def get_object(api, endpoint, bucket, key):
    status, body, _ = api.request("GET", object_url(endpoint, bucket, key))
    if status == 404:
        return None
    if status != 200:
        fail(f"GET {key} failed: HTTP {status}")
    return body


def generation_for(key):
    if not key.startswith(GENERATION_PREFIX):
        return None
    rest = key[len(GENERATION_PREFIX) :]
    if "/" not in rest:
        return None
    generation = rest.split("/", 1)[0]
    if not GENERATION_ID.match(generation):
        fail(f"bucket contains an invalid generation path: {key}")
    return generation


def load_manifests(bucket, objects, api, endpoint):
    manifests = {}
    for key in sorted(objects):
        if not key.startswith(MANIFEST_PREFIX) or not key.endswith(".json"):
            continue
        generation = key[len(MANIFEST_PREFIX) : -len(".json")]
        if not GENERATION_ID.match(generation):
            fail(f"bucket contains an invalid manifest key: {key}")
        body = get_object(api, endpoint, bucket, key)
        if body is None:
            fail(f"manifest disappeared while listing: {key}")
        try:
            document = json.loads(body)
        except json.JSONDecodeError as e:
            fail(f"manifest {key} is invalid JSON: {e}")
        if document.get("schemaVersion") != 1 or not isinstance(document.get("objects"), list):
            fail(f"manifest {key} has an unsupported shape")
        refs = {}
        for entry in document["objects"]:
            if not isinstance(entry, dict):
                fail(f"manifest {key} contains a non-object entry")
            ref_key = entry.get("key")
            size = entry.get("size")
            etag = entry.get("etag")
            if not safe_object_key(ref_key):
                fail(f"manifest {key} contains an unsafe key")
            if not isinstance(size, int) or size < 0:
                fail(f"manifest {key} contains an invalid size for {ref_key}")
            if not isinstance(etag, str) or not etag:
                fail(f"manifest {key} contains an invalid ETag for {ref_key}")
            refs[ref_key] = size
        manifests[generation] = {
            "key": key,
            "etag": objects[key][1],
            "time": parse_time(objects[key][2]),
            "refs": refs,
        }
    return manifests


def select_retained(manifests, current, keep_previous, keep_newer):
    if current not in manifests:
        fail(f"current generation {current} has no manifest")
    current_time = manifests[current]["time"]
    older = sorted(
        (g for g in manifests if g != current and manifests[g]["time"] <= current_time),
        key=lambda g: (manifests[g]["time"], g),
        reverse=True,
    )
    newer = sorted(
        (g for g in manifests if g != current and manifests[g]["time"] > current_time),
        key=lambda g: (manifests[g]["time"], g),
        reverse=True,
    )
    previous = older[:keep_previous]
    forward = newer if keep_newer is None else newer[:keep_newer]
    return {current, *previous, *forward}, previous, forward


def check_retained(manifests, retained, objects):
    generation_dirs = {generation_for(key) for key in objects} - {None}
    for generation in sorted(retained):
        manifest = manifests[generation]
        for key, size in manifest["refs"].items():
            found = objects.get(key)
            if found is None:
                fail(f"retained generation {generation} is missing {key}")
            if found[0] != size:
                fail(f"retained generation {generation} has the wrong size for {key}")
        if generation not in generation_dirs:
            fail(f"retained generation {generation} has no generation directory")


def build_plan(bucket, keep_previous, keep_newer, grace_minutes):
    api = load_api()
    endpoint = os.environ["R2_S3_ENDPOINT"].rstrip("/")
    objects = s3_list_meta(bucket)
    current_body = get_object(api, endpoint, bucket, CURRENT_KEY)
    if current_body is None:
        fail(f"bucket {bucket} has no current marker")
    current = current_body.decode("utf-8", "replace").strip()
    if not GENERATION_ID.match(current):
        fail(f"bucket {bucket} current marker is invalid: {current!r}")
    manifests = load_manifests(bucket, objects, api, endpoint)
    retained, previous, forward = select_retained(
        manifests, current, keep_previous, keep_newer
    )
    check_retained(manifests, retained, objects)

    generation_dirs = {generation_for(key) for key in objects} - {None}
    orphan_dirs = generation_dirs - set(manifests)
    cutoff = time.time() - grace_minutes * 60
    old_orphans = set()
    in_flight = set()
    for generation in orphan_dirs:
        newest = max(
            parse_time(objects[key][2])
            for key in objects
            if generation_for(key) == generation
        )
        if newest < cutoff:
            old_orphans.add(generation)
        else:
            in_flight.add(generation)

    dropped = (set(manifests) - retained) | old_orphans
    drop_generation = {
        key for key in objects if generation_for(key) in dropped
    }
    drop_manifests = {
        manifests[generation]["key"]
        for generation in manifests
        if generation not in retained
    }

    shared_prefixes = set()
    retained_shared = set()
    for generation, manifest in manifests.items():
        for key in manifest["refs"]:
            if (
                generation_for(key) is None
                and not key.startswith(MANIFEST_PREFIX)
                and key != CURRENT_KEY
            ):
                shared_prefixes.add(key.split("/", 1)[0])
                if generation in retained:
                    retained_shared.add(key)

    drop_shared = set()
    unmanaged = set()
    for key in objects:
        if key == CURRENT_KEY or key in drop_generation or key in drop_manifests:
            continue
        if key.startswith(GENERATION_PREFIX) or key.startswith(MANIFEST_PREFIX):
            continue
        if key in retained_shared:
            continue
        if key.split("/", 1)[0] in shared_prefixes:
            drop_shared.add(key)
        else:
            unmanaged.add(key)

    if in_flight:
        drop_shared.clear()

    drop_keys = drop_generation | drop_manifests | drop_shared
    for generation in retained:
        overlap = manifests[generation]["refs"].keys() & drop_keys
        if overlap:
            fail(f"refusing to delete retained objects: {', '.join(sorted(overlap))}")
    if CURRENT_KEY in drop_keys:
        fail("refusing to delete the current marker")

    return {
        "bucket": bucket,
        "api": api,
        "endpoint": endpoint,
        "objects": objects,
        "current": current,
        "manifests": manifests,
        "retained": retained,
        "previous": previous,
        "forward": forward,
        "in_flight": in_flight,
        "drop_generations": sorted(dropped),
        "drop_generation": drop_generation,
        "drop_manifests": drop_manifests,
        "drop_shared": drop_shared,
        "unmanaged": unmanaged,
        "drop_keys": drop_keys,
    }


def manifest_signature(bucket):
    return {
        key: (value[0], value[1])
        for key, value in s3_list_meta(bucket, MANIFEST_PREFIX).items()
    }


def assert_unchanged(plan, deleted):
    api = plan["api"]
    before = manifest_signature(plan["bucket"])
    expected = {manifest["key"] for manifest in plan["manifests"].values()} - deleted
    if set(before) != expected:
        fail(f"{plan['bucket']} manifest set changed; rerun gc")
    for key, (_, etag) in before.items():
        generation = key[len(MANIFEST_PREFIX) : -len(".json")]
        manifest = plan["manifests"].get(generation)
        if manifest is None or manifest["etag"] != etag:
            fail(f"{plan['bucket']} manifest {key} changed; rerun gc")
    body = get_object(api, plan["endpoint"], plan["bucket"], CURRENT_KEY)
    current = body.decode("utf-8", "replace").strip() if body is not None else ""
    if current != plan["current"]:
        fail(
            f"{plan['bucket']} current changed from {plan['current']} to "
            f"{current or '<missing>'}; rerun gc"
        )


def delete_one(api, endpoint, bucket, key):
    url = object_url(endpoint, bucket, key)
    for attempt in range(DELETE_RETRIES):
        status, _, _ = api.request("DELETE", url)
        if status in (200, 204, 404):
            return None
        if status in RETRYABLE and attempt + 1 < DELETE_RETRIES:
            time.sleep(min(2**attempt, 30))
            continue
        return f"{key}: HTTP {status}"
    return f"{key}: retries exhausted"


def delete_batches(plan):
    for phase in ("drop_manifests", "drop_generation", "drop_shared"):
        keys = sorted(plan[phase])
        for start in range(0, len(keys), DELETE_BATCH):
            yield keys[start : start + DELETE_BATCH]


def delete_keys(plan, jobs):
    failures = []
    deleted = set()
    with ThreadPoolExecutor(max_workers=jobs) as pool:
        for batch in delete_batches(plan):
            assert_unchanged(plan, deleted)
            futures = {
                pool.submit(
                    delete_one, plan["api"], plan["endpoint"], plan["bucket"], key
                ): key
                for key in batch
            }
            for future in as_completed(futures):
                error = future.result()
                if error is not None:
                    failures.append(error)
            deleted.update(batch)
            if failures:
                break
    if failures:
        fail(
            f"{plan['bucket']} could not delete {len(failures)} objects: "
            + "; ".join(failures[:5])
        )


def summarize(objects, keys):
    return len(keys), sum(objects[key][0] for key in keys)


def format_generations(generations):
    return ", ".join(generations) if generations else "<none>"


def run_bucket(bucket, args):
    plan = build_plan(bucket, args.keep_previous, args.keep_newer, args.grace_minutes)
    objects = plan["objects"]
    before_bytes = sum(value[0] for value in objects.values())
    before_count = len(objects)
    drop_bytes = sum(objects[key][0] for key in plan["drop_keys"])
    after_bytes = before_bytes - drop_bytes
    after_count = before_count - len(plan["drop_keys"])

    kept = [plan["current"]]
    kept.extend(plan["previous"])
    kept.extend(plan["forward"])
    print(f"{bucket}: current {plan['current']}")
    print(f"  keep generations: {format_generations(kept)}")
    if plan["drop_generations"]:
        print(f"  drop generations: {format_generations(plan['drop_generations'])}")
    for label, keys in (
        ("generation objects", plan["drop_generation"]),
        ("manifests", plan["drop_manifests"]),
        ("shared objects", plan["drop_shared"]),
    ):
        count, size = summarize(objects, keys)
        if count:
            print(f"  drop {label}: {count:,} objects, {human_bytes(size)}")
    if plan["in_flight"]:
        print(
            "  note: recent generation directories without a manifest; "
            f"keeping all shared objects: {format_generations(sorted(plan['in_flight']))}"
        )
    if plan["unmanaged"]:
        count, size = summarize(objects, plan["unmanaged"])
        print(f"  keep unmanaged objects: {count:,}, {human_bytes(size)}")
    print(
        f"  size: {human_bytes(before_bytes)} ({before_count:,} objects) -> "
        f"{human_bytes(after_bytes)} ({after_count:,} objects); "
        f"reclaim {human_bytes(drop_bytes)}"
    )
    if args.dry_run:
        print("  dry run: no objects deleted")
        return before_bytes, before_count, after_bytes, after_count

    if not plan["drop_keys"]:
        return before_bytes, before_count, before_bytes, before_count
    delete_keys(plan, args.jobs)
    after = s3_list_meta(bucket)
    current_body = get_object(plan["api"], plan["endpoint"], bucket, CURRENT_KEY)
    current = current_body.decode("utf-8", "replace").strip() if current_body is not None else ""
    if current != plan["current"]:
        fail(f"{bucket} current changed during gc: {current or '<missing>'}")
    final_bytes = sum(value[0] for value in after.values())
    final_count = len(after)
    print(
        f"  after: {human_bytes(final_bytes)} ({final_count:,} objects); "
        f"reclaimed {human_bytes(before_bytes - final_bytes)}"
    )
    return before_bytes, before_count, final_bytes, final_count


def keep_count(value):
    return None if value == "all" else int(value)


def main(argv=None):
    parser = argparse.ArgumentParser(description="Delete R2 objects no retained generation needs")
    parser.add_argument("buckets", nargs="+")
    parser.add_argument("--keep-previous", type=int, default=1)
    parser.add_argument("--keep-newer", type=keep_count, default=None)
    parser.add_argument("--grace-minutes", type=int, default=60)
    parser.add_argument("--jobs", type=int, default=int(os.environ.get("R2_GC_JOBS", "8")))
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args(argv)
    if args.keep_previous < 0 or (args.keep_newer is not None and args.keep_newer < 0):
        fail("--keep-previous and --keep-newer must be non-negative")
    if args.grace_minutes < 0:
        fail("--grace-minutes must be non-negative")
    if args.jobs < 1:
        fail("--jobs must be at least 1")

    user_s3 = s3_ready()
    totals = [0, 0, 0, 0]
    for index, bucket in enumerate(args.buckets):
        if index:
            print()
        if not user_s3:
            clear_s3()
            mint_credentials(bucket)
        with publish_lock(bucket):
            result = run_bucket(bucket, args)
        for position, value in enumerate(result):
            totals[position] += value
    print()
    after = f"would be {human_bytes(totals[2])}" if args.dry_run else human_bytes(totals[2])
    action = "would reclaim" if args.dry_run else "reclaimed"
    print(
        f"total: {human_bytes(totals[0])} ({totals[1]:,} objects) -> "
        f"{after} ({totals[3]:,} objects); "
        f"{action} {human_bytes(totals[0] - totals[2])}"
    )


if __name__ == "__main__":
    main()
