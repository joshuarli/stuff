#!/usr/bin/env python3
"""Multipart-upload one file to R2 over the S3-compatible API (stdlib only).

Usage: r2-multipart.py FILE BUCKET KEY CONTENT_TYPE
Env: R2_S3_ENDPOINT (e.g. https://<account>.r2.cloudflarestorage.com),
     R2_S3_ACCESS_KEY_ID, R2_S3_SECRET_ACCESS_KEY, optional R2_S3_REGION
     (default "auto"), optional R2_S3_SESSION_TOKEN for temporary credentials.

Parts are 64 MiB. Any failure aborts the multipart upload so no partial
state is left behind.
"""

import datetime
import hashlib
import hmac
import os
import sys
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

from r2_common import PART_SIZE, fail, object_url, quote

EMPTY_SHA256 = hashlib.sha256(b"").hexdigest()


def sign(key, msg):
    return hmac.new(key, msg.encode("utf-8"), hashlib.sha256).digest()


def canonical_query(query):
    pairs = urllib.parse.parse_qsl(query, keep_blank_values=True)
    encoded = sorted((quote(key), quote(value)) for key, value in pairs)
    return "&".join(f"{key}={value}" for key, value in encoded)


def normalize_header(value):
    return " ".join(value.split())


def canonical_request(
    method, url, payload_hash, content_type="", amzdate=None, extra_headers=None
):
    parsed = urllib.parse.urlsplit(url)
    headers = {
        "host": parsed.netloc,
        "x-amz-content-sha256": payload_hash,
    }
    if amzdate is not None:
        headers["x-amz-date"] = amzdate
    if content_type:
        headers["content-type"] = content_type
    token = os.environ.get("R2_S3_SESSION_TOKEN")
    if token:
        headers["x-amz-security-token"] = token
    if extra_headers:
        for key, value in extra_headers.items():
            headers[key.lower()] = value
    names = sorted(headers)
    canonical_headers = "".join(
        f"{name}:{normalize_header(headers[name])}\n" for name in names
    )
    signed_headers = ";".join(names)
    canonical = "\n".join(
        [
            method,
            parsed.path or "/",
            canonical_query(parsed.query),
            canonical_headers,
            signed_headers,
            payload_hash,
        ]
    )
    return canonical, signed_headers


def signature_headers(
    method,
    url,
    payload_hash,
    content_type="",
    extra_headers=None,
    now=None,
):
    access = os.environ["R2_S3_ACCESS_KEY_ID"]
    secret = os.environ["R2_S3_SECRET_ACCESS_KEY"]
    region = os.environ.get("R2_S3_REGION", "auto")
    if now is None:
        now = datetime.datetime.now(datetime.timezone.utc)
    amzdate = now.strftime("%Y%m%dT%H%M%SZ")
    datestamp = now.strftime("%Y%m%d")
    headers = dict(extra_headers or {})
    canonical, signed_headers = canonical_request(
        method, url, payload_hash, content_type, amzdate, headers
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
    result = {
        "x-amz-date": amzdate,
        "x-amz-content-sha256": payload_hash,
        "Authorization": (
            f"AWS4-HMAC-SHA256 Credential={access}/{scope}, "
            f"SignedHeaders={signed_headers}, Signature={signature}"
        ),
    }
    token = os.environ.get("R2_S3_SESSION_TOKEN")
    if token:
        result["x-amz-security-token"] = token
    for key, value in headers.items():
        result.setdefault(key, value)
    return result


def request(method, url, body=b"", content_type="", extra_headers=None):
    payload_hash = hashlib.sha256(body).hexdigest()
    headers = signature_headers(
        method, url, payload_hash, content_type, extra_headers=extra_headers
    )
    if content_type:
        headers["Content-Type"] = content_type
    req = urllib.request.Request(
        url, data=body if method != "GET" else None, headers=headers, method=method
    )
    try:
        with urllib.request.urlopen(req) as resp:
            return resp.status, resp.read(), dict(resp.headers)
    except urllib.error.HTTPError as e:
        status = e.code
        data = e.read()
        headers = dict(e.headers)
        e.close()
        return status, data, headers


def find_text(root, local):
    for elem in root.iter():
        if elem.tag == local or elem.tag.endswith("}" + local):
            return (elem.text or "").strip()
    return ""


def completion_error(body):
    if not body.strip():
        return ""
    try:
        root = ET.fromstring(body)
    except ET.ParseError:
        return ""
    return find_text(root, "Code") or find_text(root, "Message")


def main():
    if len(sys.argv) != 5:
        fail("usage: r2-multipart.py FILE BUCKET KEY CONTENT_TYPE")
    path, bucket, key, content_type = sys.argv[1:5]
    for var in ("R2_S3_ENDPOINT", "R2_S3_ACCESS_KEY_ID", "R2_S3_SECRET_ACCESS_KEY"):
        if not os.environ.get(var):
            fail(f"{var} is not set")
    endpoint = os.environ["R2_S3_ENDPOINT"].rstrip("/")
    base = object_url(endpoint, bucket, key)
    size = os.path.getsize(path)

    status, body, _ = request("POST", base + "?uploads", b"")
    if status != 200:
        fail(f"initiate multipart upload failed: HTTP {status}")
    upload_id = find_text(ET.fromstring(body), "UploadId")
    if not upload_id:
        fail("initiate multipart upload returned no UploadId")

    def abort():
        request("DELETE", base + f"?uploadId={quote(upload_id)}")

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
                    base + f"?partNumber={part}&uploadId={quote(upload_id)}",
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
            base + f"?uploadId={quote(upload_id)}",
            complete,
            content_type,
        )
        error = completion_error(body)
        if status != 200 or error:
            abort()
            detail = f": {error}" if error else ""
            fail(f"complete multipart upload failed: HTTP {status}{detail}")
    except OSError as e:
        abort()
        fail(f"could not read {path}: {e}")
    print(f"multipart {key} ({size} bytes, {len(etags)} parts)")


if __name__ == "__main__":
    main()
