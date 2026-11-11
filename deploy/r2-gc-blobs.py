#!/usr/bin/env python3
"""Delete OCI blobs that no retained metadata generation references.

Usage: r2-gc-blobs.py [META_BUCKET] [BLOB_BUCKET] [options]

Reads `blobs.json` from every retained generation in the metadata bucket,
lists `sha256/*` in the blob bucket, and deletes the unreferenced keys.
Retention matches r2-gc.py in S3 mode. Under CF_LOCAL_STATE the listing is
read through the cf CLI and every generation with a manifest is retained,
because the local simulation is a single-writer rehearsal.

Anything younger than --grace-minutes (default 60, the upload session
lifetime) is kept: blobs uploaded within it, generations whose manifest was
written within it, and the blobs.json of a recent generation that has no
manifest yet. A recent generation without a readable blobs.json blocks all
deletion. Before every delete batch the helper re-reads `current`, the
manifests, and the generation set, and aborts if any of them changed.
"""

import argparse
import importlib.util
import json
import os
import pathlib
import re
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed

from r2_common import cf_command, fail, publish_lock, s3_list_meta, s3_ready

BLOB_PREFIX = "sha256/"
BLOB_KEY = re.compile(r"sha256/[0-9a-f]{64}")
GENERATION_PREFIX = "generations/"
MANIFEST_PREFIX = "manifests/"
CURRENT_KEY = "current"
DELETE_BATCH = 256
DELETE_RETRIES = 5
RETRYABLE = {429, 500, 502, 503, 504}


def load_gc():
    path = pathlib.Path(__file__).with_name("r2-gc.py")
    spec = importlib.util.spec_from_file_location("r2_gc", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def parse_args(argv):
    parser = argparse.ArgumentParser(prog="r2-gc-blobs.py")
    parser.add_argument("meta_bucket", nargs="?", default="stuff-oci")
    parser.add_argument("blob_bucket", nargs="?", default="stuff-oci-blobs")
    parser.add_argument("--keep-previous", type=int, default=1)
    parser.add_argument("--keep-newer", default="all")
    parser.add_argument("--grace-minutes", type=int, default=60)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--jobs", type=int, default=int(os.environ.get("R2_GC_JOBS", "8")))
    return parser.parse_args(argv)


def local_list(bucket):
    page = int(os.environ.get("R2_LIST_PAGE", "1000"))
    objects = {}
    after = None
    seen = set()
    while True:
        args = ["list", "--per-page", str(page)]
        if after:
            args += ["--start-after", after]
        done = subprocess.run(cf_command(args, bucket), capture_output=True, text=True)
        if done.returncode != 0:
            fail(f"listing {bucket} failed: {done.stderr.strip()}")
        entries = json.loads(done.stdout)
        repeated = next((entry["key"] for entry in entries if entry["key"] in seen), None)
        if repeated is not None:
            fail(f"listing repeated {repeated} after {after}")
        seen.update(entry["key"] for entry in entries)
        for entry in entries:
            objects[entry["key"]] = (
                entry["size"],
                entry["etag"],
                entry.get("last_modified", ""),
            )
        if len(entries) < page:
            return objects
        after = entries[-1]["key"]


def local_get(bucket, key):
    done = subprocess.run(cf_command(["get", key], bucket), capture_output=True)
    if done.returncode != 0:
        fail(f"GET {bucket}/{key} failed: {done.stderr.decode('utf-8', 'replace').strip()}")
    return done.stdout


def local_delete(bucket, key):
    done = subprocess.run(
        cf_command(["delete", key, "--force"], bucket),
        capture_output=True,
        text=True,
    )
    if done.returncode == 0:
        return None
    detail = (done.stderr or done.stdout).strip()
    return f"{key}: {detail or 'delete failed'}"


def generation_of(key):
    if not key.startswith(GENERATION_PREFIX) or "/" not in key[len(GENERATION_PREFIX) :]:
        return None
    return key[len(GENERATION_PREFIX) :].split("/", 1)[0]


def manifest_times(gc, meta_objects):
    manifests = {}
    for key in meta_objects:
        if not key.startswith(MANIFEST_PREFIX) or not key.endswith(".json"):
            continue
        generation = key[len(MANIFEST_PREFIX) : -len(".json")]
        if not gc.GENERATION_ID.match(generation):
            fail(f"invalid manifest key {key}")
        manifests[generation] = {"time": gc.parse_time(meta_objects[key][2])}
    return manifests


def retained_generations(args, meta_objects, get_body, cutoff):
    gc = load_gc()
    manifests = manifest_times(gc, meta_objects)
    if not manifests:
        fail(f"bucket {args.meta_bucket} has no generation manifests")
    current = None
    if CURRENT_KEY in meta_objects:
        current = get_body(args.meta_bucket, CURRENT_KEY).decode("utf-8", "replace").strip()
    if not current or not gc.GENERATION_ID.match(current):
        fail(f"bucket {args.meta_bucket} has no valid current marker")
    if not s3_ready():
        return set(manifests), current
    keep_newer = None if args.keep_newer == "all" else int(args.keep_newer)
    retained, _, _ = gc.select_retained(manifests, current, args.keep_previous, keep_newer)
    recent = {generation for generation, entry in manifests.items() if entry["time"] >= cutoff}
    return retained | recent, current


def in_flight_generations(meta_objects, cutoff):
    gc = load_gc()
    manifests = set(manifest_times(gc, meta_objects))
    newest = {}
    for key, (_, _, modified) in meta_objects.items():
        generation = generation_of(key)
        if generation is None or generation in manifests:
            continue
        newest[generation] = max(newest.get(generation, 0.0), gc.parse_time(modified))
    return {generation for generation, when in newest.items() if when >= cutoff}


def meta_signature(meta_objects, current):
    manifests = frozenset(
        (key, meta_objects[key][1]) for key in meta_objects if key.startswith(MANIFEST_PREFIX)
    )
    generations = frozenset(generation_of(key) for key in meta_objects) - {None}
    return current, manifests, generations


def plan_drop(args, meta_objects, blobs, get_body):
    gc = load_gc()
    cutoff = time.time() - args.grace_minutes * 60
    retained, current = retained_generations(args, meta_objects, get_body, cutoff)
    in_flight = in_flight_generations(meta_objects, cutoff)
    unreadable = sorted(
        generation
        for generation in in_flight
        if f"{GENERATION_PREFIX}{generation}/blobs.json" not in meta_objects
    )
    referenced = referenced_blobs(args.meta_bucket, retained | (in_flight - set(unreadable)), get_body)
    managed = {key for key in blobs if BLOB_KEY.fullmatch(key)}
    recent = {key for key in managed if gc.parse_time(blobs[key][2]) >= cutoff}
    drop = [] if unreadable else sorted(managed - referenced - recent)
    return {
        "current": current,
        "retained": retained,
        "in_flight": sorted(in_flight),
        "unreadable": unreadable,
        "kept": len(managed) - len(drop),
        "recent": len(recent - referenced),
        "drop": drop,
        "signature": meta_signature(meta_objects, current),
    }


def report(args, plan):
    print(f"meta: {args.meta_bucket}; current: {plan['current']}")
    print(f"retain generations: {', '.join(sorted(plan['retained']))}")
    if plan["in_flight"]:
        print(f"in-flight generations: {', '.join(plan['in_flight'])}")
    if plan["unreadable"]:
        print(
            "keeping every blob: recent generations without blobs.json: "
            + ", ".join(plan["unreadable"])
        )
    print(
        f"keep blobs: {plan['kept']} ({plan['recent']} within the grace period), "
        f"drop blobs: {len(plan['drop'])}"
    )
    for key in plan["drop"]:
        print(f"drop {key}")


def delete_batches(args, plan, list_meta, get_body, delete_batch):
    for start in range(0, len(plan["drop"]), DELETE_BATCH):
        meta_objects = list_meta(args.meta_bucket)
        current = None
        if CURRENT_KEY in meta_objects:
            body = get_body(args.meta_bucket, CURRENT_KEY)
            current = body.decode("utf-8", "replace").strip() if body is not None else None
        if meta_signature(meta_objects, current) != plan["signature"]:
            fail(f"{args.meta_bucket} metadata changed during blob gc; rerun gc")
        delete_batch(plan["drop"][start : start + DELETE_BATCH])


def referenced_blobs(meta_bucket, retained, get_body):
    referenced = set()
    for generation in sorted(retained):
        key = f"{GENERATION_PREFIX}{generation}/blobs.json"
        body = get_body(meta_bucket, key)
        try:
            document = json.loads(body)
        except json.JSONDecodeError as error:
            fail(f"{meta_bucket}/{key} is invalid JSON: {error}")
        if not isinstance(document, dict) or document.get("schemaVersion") != 1:
            fail(f"{meta_bucket}/{key} has an unsupported schema")
        blobs = document.get("blobs")
        if not isinstance(blobs, list):
            fail(f"{meta_bucket}/{key} has no blob list")
        for digest in blobs:
            if not isinstance(digest, str) or not digest.startswith("sha256:"):
                fail(f"{meta_bucket}/{key} has an invalid digest")
            blob_key = BLOB_PREFIX + digest[len("sha256:") :]
            if not BLOB_KEY.fullmatch(blob_key):
                fail(f"{meta_bucket}/{key} has an invalid digest {digest}")
            referenced.add(blob_key)
    return referenced


def s3_get_body(api, endpoint, bucket, key):
    gc = load_gc()
    return gc.get_object(api, endpoint, bucket, key)


def delete_s3(api, endpoint, bucket, keys, jobs):
    gc = load_gc()
    failures = []
    with ThreadPoolExecutor(max_workers=jobs) as pool:
        futures = {
            pool.submit(gc.delete_one, api, endpoint, bucket, key): key for key in keys
        }
        for future in as_completed(futures):
            error = future.result()
            if error is not None:
                failures.append(error)
    if failures:
        fail(
            f"{bucket} could not delete {len(failures)} blobs: "
            + "; ".join(failures[:5])
        )


def delete_local(bucket, keys, jobs):
    failures = []

    def worker(key):
        return local_delete(bucket, key)

    with ThreadPoolExecutor(max_workers=jobs) as pool:
        futures = {pool.submit(worker, key): key for key in keys}
        for future in as_completed(futures):
            error = future.result()
            if error is not None:
                failures.append(error)
    if failures:
        fail(
            f"{bucket} could not delete {len(failures)} blobs: "
            + "; ".join(failures[:5])
        )


def main(argv):
    args = parse_args(argv)
    if args.grace_minutes < 0:
        fail("--grace-minutes must be non-negative")
    jobs = max(1, args.jobs)
    if s3_ready():
        gc = load_gc()
        api = gc.load_api()
        endpoint = os.environ["R2_S3_ENDPOINT"].rstrip("/")

        def get_body(bucket, key):
            return s3_get_body(api, endpoint, bucket, key)

        list_meta = s3_list_meta

        def delete_batch(keys):
            delete_s3(api, endpoint, args.blob_bucket, keys, jobs)
    else:
        get_body = local_get
        list_meta = local_list

        def delete_batch(keys):
            delete_local(args.blob_bucket, keys, jobs)

    with publish_lock(args.meta_bucket):
        meta_objects = list_meta(args.meta_bucket)
        blobs = list_meta(args.blob_bucket)
        plan = plan_drop(args, meta_objects, blobs, get_body)
        report(args, plan)
        if args.dry_run:
            print("dry run: no objects deleted")
            return 0
        delete_batches(args, plan, list_meta, get_body, delete_batch)
    print(f"deleted {len(plan['drop'])} blobs from {args.blob_bucket}; kept {plan['kept']}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
