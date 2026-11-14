#!/usr/bin/env python3
"""Copy and verify one bucket between S3-compatible stores, resumably.

  store-copy.py copy   SOURCE_BUCKET DEST_BUCKET --ledger FILE
  store-copy.py verify SOURCE_BUCKET DEST_BUCKET --ledger FILE [--deep]

Source credentials come from STUFF_COPY_SOURCE_S3_{ENDPOINT,ACCESS_KEY_ID,
SECRET_ACCESS_KEY,REGION,SESSION_TOKEN}, or, when those are unset, from
`stuff credentials SOURCE_BUCKET` (Cloudflare R2). Destination credentials come
from STUFF_COPY_DEST_S3_*. Credentials are never printed or passed in argv.

Exit codes: 0 copied or verified, 1 a mismatch or transfer failure, 2 usage.
"""
import argparse
import contextlib
import hashlib
import json
import os
import shlex
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import xml.etree.ElementTree as ET

from r2_common import PART_SIZE, load_sibling, object_url, quote, s3_list_meta

S3 = load_sibling("r2_multipart", "r2-multipart.py")
FIELDS = ("ENDPOINT", "ACCESS_KEY_ID", "SECRET_ACCESS_KEY", "REGION", "SESSION_TOKEN")
KEPT_HEADERS = ("Content-Type", "Cache-Control", "Content-Encoding", "Content-Disposition")
RETRIES = 5
CHUNK = 1 << 20


class Failure(Exception):
    pass


def credentials(prefix, bucket):
    found = {field: os.environ.get(f"{prefix}{field}") for field in FIELDS}
    if found["ENDPOINT"] and found["ACCESS_KEY_ID"] and found["SECRET_ACCESS_KEY"]:
        return {field: value for field, value in found.items() if value}
    if prefix != "STUFF_COPY_SOURCE_S3_":
        raise Failure(f"{prefix}ENDPOINT, {prefix}ACCESS_KEY_ID and {prefix}SECRET_ACCESS_KEY are required")
    done = subprocess.run([os.environ.get("STUFF", "stuff"), "credentials", bucket], capture_output=True, text=True)
    if done.returncode != 0:
        raise Failure(f"stuff credentials {bucket} failed")
    minted = {}
    for line in done.stdout.splitlines():
        if line.startswith("export R2_S3_"):
            name, _, raw = line[len("export R2_S3_"):].partition("=")
            minted[name] = shlex.split(raw)[0]
    return minted


@contextlib.contextmanager
def store(creds):
    saved = {field: os.environ.get(f"R2_S3_{field}") for field in FIELDS}
    try:
        for field in FIELDS:
            os.environ.pop(f"R2_S3_{field}", None)
            if field in creds:
                os.environ[f"R2_S3_{field}"] = creds[field]
        yield creds["ENDPOINT"].rstrip("/")
    finally:
        for field, value in saved.items():
            os.environ.pop(f"R2_S3_{field}", None)
            if value is not None:
                os.environ[f"R2_S3_{field}"] = value


def listing(creds, bucket):
    with store(creds):
        return s3_list_meta(bucket)


def retrying(action, describe):
    last = ""
    for attempt in range(1, RETRIES + 1):
        try:
            return action()
        except (urllib.error.URLError, OSError, Failure) as error:
            last = str(error)
            time.sleep(min(30, 2 ** attempt))
    raise Failure(f"{describe}: {last}")


def head(creds, bucket, key, part=None):
    with store(creds) as endpoint:
        url = object_url(endpoint, bucket, key) + (f"?partNumber={part}" if part is not None else "")
        status, _, headers = S3.request("HEAD", url)
    if status == 404:
        return None
    if status not in (200, 206):
        raise Failure(f"HEAD {key} answered {status}")
    return {name.lower(): value for name, value in headers.items()}


def download(creds, bucket, key, path):
    with store(creds) as endpoint:
        url = object_url(endpoint, bucket, key)
        headers = S3.signature_headers("GET", url, S3.EMPTY_SHA256)
        request = urllib.request.Request(url, headers=headers, method="GET")
        sha, md5, size = hashlib.sha256(), hashlib.md5(), 0
        with urllib.request.urlopen(request) as response, open(path, "wb") as out:
            kept = {name: response.headers[name] for name in KEPT_HEADERS if response.headers.get(name)}
            for chunk in iter(lambda: response.read(CHUNK), b""):
                out.write(chunk)
                sha.update(chunk)
                md5.update(chunk)
                size += len(chunk)
            out.flush()
            os.fsync(out.fileno())
    return {"size": size, "sha256": sha.hexdigest(), "md5": md5.hexdigest(), "headers": kept}


def part_sizes(creds, bucket, key, parts):
    sizes = []
    for part in range(1, parts + 1):
        found = head(creds, bucket, key, part)
        length = None if found is None else found.get("content-length")
        if length is None:
            return None
        sizes.append(int(length))
    return sizes


def put_streaming(base, path, sha256, headers):
    content_type = headers.get("Content-Type", "application/octet-stream")
    extra = {name: value for name, value in headers.items() if name != "Content-Type"}
    signed = S3.signature_headers("PUT", base, sha256, content_type, extra)
    signed["Content-Type"] = content_type
    signed["Content-Length"] = str(os.path.getsize(path))
    with open(path, "rb") as body:
        request = urllib.request.Request(base, data=body, headers=signed, method="PUT")
        try:
            with urllib.request.urlopen(request) as response:
                status = response.status
        except urllib.error.HTTPError as error:
            status = error.code
            error.close()
    if status != 200:
        raise Failure(f"PUT answered {status}")


def upload(creds, bucket, key, path, sha256, headers, sizes):
    with store(creds) as endpoint:
        base = object_url(endpoint, bucket, key)
        if sizes is None:
            put_streaming(base, path, sha256, headers)
            return
        content_type = headers.get("Content-Type", "application/octet-stream")
        extra = {name: value for name, value in headers.items() if name != "Content-Type"}
        status, body, _ = S3.request("POST", base + "?uploads", b"", content_type, extra)
        if status != 200:
            raise Failure(f"initiating {key} answered {status}")
        upload_id = S3.find_text(ET.fromstring(body), "UploadId")
        etags = []
        try:
            with open(path, "rb") as f:
                for number, size in enumerate(sizes, start=1):
                    chunk = f.read(size)
                    status, _, reply = S3.request("PUT", base + f"?partNumber={number}&uploadId={quote(upload_id)}", chunk, "application/octet-stream")
                    if status != 200:
                        raise Failure(f"part {number} of {key} answered {status}")
                    etags.append((number, reply.get("ETag") or reply.get("Etag") or ""))
            parts = "".join(f"<Part><PartNumber>{n}</PartNumber><ETag>{e}</ETag></Part>" for n, e in etags)
            complete = f'<?xml version="1.0" encoding="UTF-8"?><CompleteMultipartUpload>{parts}</CompleteMultipartUpload>'.encode()
            status, body, _ = S3.request("POST", base + f"?uploadId={quote(upload_id)}", complete, "application/xml")
            if status != 200 or S3.completion_error(body):
                raise Failure(f"completing {key} answered {status}")
        except BaseException:
            S3.request("DELETE", base + f"?uploadId={quote(upload_id)}")
            raise


def multipart_etag_of(path, sizes):
    digests = []
    with open(path, "rb") as f:
        for size in sizes:
            digests.append(hashlib.md5(f.read(size)).digest())
    return f"{hashlib.md5(b''.join(digests)).hexdigest()}-{len(sizes)}"


def default_parts(size):
    return [min(PART_SIZE, size - offset) for offset in range(0, size, PART_SIZE)]


def read_ledger(path):
    entries = {}
    if os.path.exists(path):
        with open(path, encoding="utf-8") as f:
            for line in f:
                if line.strip():
                    entry = json.loads(line)
                    entries[entry["key"]] = entry
    return entries


def append_ledger(path, entry):
    with open(path, "a", encoding="utf-8") as f:
        f.write(json.dumps(entry, sort_keys=True) + "\n")
        f.flush()
        os.fsync(f.fileno())


def copy_object(source, dest, src_bucket, dst_bucket, key, size, etag, scratch):
    path = os.path.join(scratch, "object")
    got = retrying(lambda: download(source, src_bucket, key, path), f"downloading {key}")
    if got["size"] != size:
        raise Failure(f"{key}: downloaded {got['size']} bytes, listing says {size}")
    if "-" in etag:
        sizes = part_sizes(source, src_bucket, key, int(etag.rsplit("-", 1)[1])) or default_parts(size)
        if sum(sizes) != size:
            sizes = default_parts(size)
        want = multipart_etag_of(path, sizes)
    else:
        if got["md5"] != etag:
            raise Failure(f"{key}: source bytes do not match the source ETag")
        sizes = None
        want = got["md5"]
    retrying(lambda: upload(dest, dst_bucket, key, path, got["sha256"], got["headers"], sizes), f"uploading {key}")
    placed = head(dest, dst_bucket, key)
    dest_etag = (placed or {}).get("etag", "").strip('"')
    if placed is None or int(placed.get("content-length", -1)) != size or dest_etag != want:
        raise Failure(f"{key}: destination holds {dest_etag or 'nothing'}, expected {want}")
    os.remove(path)
    return {"key": key, "size": size, "sha256": got["sha256"], "sourceEtag": etag, "destEtag": dest_etag, "etagPreserved": dest_etag == etag, "contentType": got["headers"].get("Content-Type"), "copiedAtMs": int(time.time() * 1000)}


def copy(args):
    source = credentials("STUFF_COPY_SOURCE_S3_", args.source)
    dest = credentials("STUFF_COPY_DEST_S3_", args.dest)
    ledger = read_ledger(args.ledger)
    objects = listing(source, args.source)
    present = listing(dest, args.dest)
    copied = skipped = changed = 0
    with tempfile.TemporaryDirectory(prefix="store-copy-", dir=os.environ.get("TMPDIR")) as scratch:
        for key in sorted(objects):
            size, etag, _ = objects[key]
            done = ledger.get(key)
            if done is not None and done["size"] == size and done["sourceEtag"] == etag and present.get(key, (None, None))[:2] == (size, done["destEtag"]):
                skipped += 1
                continue
            if done is not None:
                changed += 1
            entry = copy_object(source, dest, args.source, args.dest, key, size, etag, scratch)
            append_ledger(args.ledger, entry)
            copied += 1
            if copied % 500 == 0:
                print(f"{args.source}: {copied} copied, {skipped} already verified", file=sys.stderr)
    print(json.dumps({"bucket": args.source, "objects": len(objects), "copied": copied, "skipped": skipped, "recopiedChanged": changed}))


def verify(args):
    source = credentials("STUFF_COPY_SOURCE_S3_", args.source)
    dest = credentials("STUFF_COPY_DEST_S3_", args.dest)
    ledger = read_ledger(args.ledger)
    objects = listing(source, args.source)
    present = listing(dest, args.dest)
    problems = []
    for key in sorted(set(objects) | set(present)):
        if key not in present:
            problems.append(f"missing at destination: {key}")
            continue
        if key not in objects:
            problems.append(f"extra at destination: {key}")
            continue
        entry = ledger.get(key)
        size, etag, _ = objects[key]
        if entry is None or entry["size"] != size or entry["sourceEtag"] != etag:
            problems.append(f"source changed or never copied: {key}")
            continue
        if present[key][:2] != (size, entry["destEtag"]):
            problems.append(f"destination differs from the ledger: {key}")
            continue
        if args.deep:
            with tempfile.TemporaryDirectory(prefix="store-verify-", dir=os.environ.get("TMPDIR")) as scratch:
                got = retrying(lambda: download(dest, args.dest, key, os.path.join(scratch, "object")), f"downloading {key}")
            if got["sha256"] != entry["sha256"]:
                problems.append(f"destination bytes differ from the source digest: {key}")
    preserved = sum(1 for key in objects if key in ledger and ledger[key]["etagPreserved"])
    print(json.dumps({"bucket": args.source, "objects": len(objects), "destination": len(present), "etagPreserved": preserved, "problems": len(problems), "deep": args.deep}))
    for problem in problems[:50]:
        print(problem, file=sys.stderr)
    return 1 if problems else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("copy", "verify"):
        command = sub.add_parser(name)
        command.add_argument("source")
        command.add_argument("dest")
        command.add_argument("--ledger", required=True)
        if name == "verify":
            command.add_argument("--deep", action="store_true")
    args = parser.parse_args()
    try:
        if args.command == "copy":
            copy(args)
            return 0
        return verify(args)
    except Failure as error:
        print(f"store-copy: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
