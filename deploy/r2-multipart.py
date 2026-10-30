#!/usr/bin/env python3
"""Multipart-upload one file to R2 over the S3-compatible API (stdlib only).

Usage: r2-multipart.py FILE BUCKET KEY CONTENT_TYPE
Env: R2_S3_ENDPOINT (e.g. https://<account>.r2.cloudflarestorage.com),
     R2_S3_ACCESS_KEY_ID, R2_S3_SECRET_ACCESS_KEY, optional R2_S3_REGION
     (default "auto"). Short-lived credentials work:
     cf r2 temporary-credentials create.

Parts are 64 MiB. Any failure aborts the multipart upload so no partial
state is left behind.
"""

import hashlib
import hmac
import os
import sys
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

PART_SIZE = 64 * 1024 * 1024


def fail(msg):
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def sign(key, msg):
    return hmac.new(key, msg.encode("utf-8"), hashlib.sha256).digest()


def signature_headers(method, url, payload_hash, content_type=""):
    endpoint = os.environ["R2_S3_ENDPOINT"].rstrip("/")
    access = os.environ["R2_S3_ACCESS_KEY_ID"]
    secret = os.environ["R2_S3_SECRET_ACCESS_KEY"]
    region = os.environ.get("R2_S3_REGION", "auto")
    parsed = urllib.parse.urlsplit(url)
    host = parsed.hostname or ""
    now = __import__("datetime").datetime.now(__import__("datetime").timezone.utc)
    amzdate = now.strftime("%Y%m%dT%H%M%SZ")
    datestamp = now.strftime("%Y%m%d")
    query = parsed.query
    canonical_headers = f"host:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amzdate}\n"
    signed_headers = "host;x-amz-content-sha256;x-amz-date"
    if content_type:
        canonical_headers += f"content-type:{content_type}\n"
        signed_headers += ";content-type"
    canonical = "\n".join(
        [method, parsed.path or "/", query, canonical_headers, signed_headers, payload_hash]
    )
    scope = f"{datestamp}/{region}/s3/aws4_request"
    string_to_sign = "\n".join(
        ["AWS4-HMAC-SHA256", amzdate, scope, hashlib.sha256(canonical.encode()).hexdigest()]
    )
    k_date = sign(("AWS4" + secret).encode(), datestamp)
    k_region = hmac.new(k_date, region.encode(), hashlib.sha256).digest()
    k_service = hmac.new(k_region, b"s3", hashlib.sha256).digest()
    k_signing = hmac.new(k_service, b"aws4_request", hashlib.sha256).digest()
    signature = hmac.new(k_signing, string_to_sign.encode(), hashlib.sha256).hexdigest()
    return {
        "x-amz-date": amzdate,
        "x-amz-content-sha256": payload_hash,
        "Authorization": (
            f"AWS4-HMAC-SHA256 Credential={access}/{scope}, "
            f"SignedHeaders={signed_headers}, Signature={signature}"
        ),
    }


def request(method, url, body=b"", content_type=""):
    payload_hash = hashlib.sha256(body).hexdigest()
    headers = signature_headers(method, url, payload_hash, content_type)
    if content_type:
        headers["Content-Type"] = content_type
    req = urllib.request.Request(url, data=body if method != "GET" else None, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req) as resp:
            return resp.status, resp.read(), dict(resp.headers)
    except urllib.error.HTTPError as e:
        return e.code, e.read(), dict(e.headers)


def find_text(root, local):
    for elem in root.iter():
        if elem.tag == local or elem.tag.endswith("}" + local):
            return (elem.text or "").strip()
    return ""


def main():
    if len(sys.argv) != 5:
        fail("usage: r2-multipart.py FILE BUCKET KEY CONTENT_TYPE")
    path, bucket, key, content_type = sys.argv[1:5]
    for var in ("R2_S3_ENDPOINT", "R2_S3_ACCESS_KEY_ID", "R2_S3_SECRET_ACCESS_KEY"):
        if not os.environ.get(var):
            fail(f"{var} is not set")
    endpoint = os.environ["R2_S3_ENDPOINT"].rstrip("/")
    quoted = "/".join(urllib.parse.quote(p, safe="") for p in key.split("/"))
    base = f"{endpoint}/{bucket}/{quoted}"
    size = os.path.getsize(path)

    status, body, _ = request("POST", base + "?uploads", b"")
    if status != 200:
        fail(f"initiate multipart upload failed: HTTP {status}")
    upload_id = find_text(ET.fromstring(body), "UploadId")
    if not upload_id:
        fail("initiate multipart upload returned no UploadId")

    def abort():
        request("DELETE", base + f"?uploadId={urllib.parse.quote(upload_id, safe='')}")

    etags = []
    try:
        with open(path, "rb") as f:
            part = 1
            while True:
                chunk = f.read(PART_SIZE)
                if not chunk:
                    break
                status, _, headers = request(
                    "PUT",
                    base + f"?partNumber={part}&uploadId={urllib.parse.quote(upload_id, safe='')}",
                    chunk,
                    "application/octet-stream",
                )
                if status != 200:
                    abort()
                    fail(f"part {part} upload failed: HTTP {status}")
                etag = headers.get("ETag") or headers.get("Etag") or ""
                if not etag:
                    abort()
                    fail(f"part {part} upload returned no ETag")
                etags.append((part, etag))
                part += 1
        parts_xml = "".join(
            f"<Part><PartNumber>{n}</PartNumber><ETag>{e}</ETag></Part>"
            for n, e in etags
        )
        complete = (
            '<?xml version="1.0" encoding="UTF-8"?>'
            f"<CompleteMultipartUpload>{parts_xml}</CompleteMultipartUpload>"
        ).encode()
        status, body, _ = request(
            "POST",
            base + f"?uploadId={urllib.parse.quote(upload_id, safe='')}",
            complete,
            content_type,
        )
        if status != 200:
            abort()
            fail(f"complete multipart upload failed: HTTP {status}")
    except OSError as e:
        abort()
        fail(f"could not read {path}: {e}")
    print(f"multipart {key} ({size} bytes, {len(etags)} parts)")


main()
