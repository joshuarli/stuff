#!/usr/bin/env python3
"""A small S3/R2 stand-in shared by the deployment tests and Rust parity tests."""

import base64
import hashlib
import hmac
import http.server
import importlib.util
import json
import os
import signal
import sys
import threading
import urllib.parse
import xml.sax.saxutils

CONTROL_PREFIX = "/_fake/"
_R2_MODULE = None


def md5_etag(body):
    return '"' + hashlib.md5(body).hexdigest() + '"'


def _load_r2():
    global _R2_MODULE
    if _R2_MODULE is None:
        path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "r2-multipart.py")
        spec = importlib.util.spec_from_file_location("fake_s3_r2_multipart", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        _R2_MODULE = module
    return _R2_MODULE


class FakeS3Server(http.server.ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, server_address=("127.0.0.1", 0), handler=None):
        super().__init__(server_address, handler or FakeS3Handler)
        self.objects = {}
        self.etags = {}
        self.requests = []
        self.log = []
        self.request_log_style = None
        self.verify_signature = False
        self.fail_remaining = 0
        self.fail_status = 429
        self.fail_retry_after = "0"
        self.fail_complete = False
        self.current = None
        self.current_etag = ""
        self.change_current_on_head = False
        self.put_conditions = []
        self.manifest_key = None
        self.manifest = b""
        self.uploads = {}

    @property
    def fail_next(self):
        return self.fail_remaining

    @fail_next.setter
    def fail_next(self, value):
        self.fail_remaining = value

    @property
    def endpoint(self):
        return f"http://127.0.0.1:{self.server_port}"

    def etag(self, bucket, key, body):
        return self.etags.get((bucket, key), md5_etag(body))

    def close(self):
        self.shutdown()
        self.server_close()


def serve(host="127.0.0.1", port=0):
    server = FakeS3Server((host, port), FakeS3Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server


class FakeS3Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def _reply(self, status, body=b"", headers=None):
        self.send_response(status)
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if body and self.command != "HEAD":
            self.wfile.write(body)

    def _read_body(self):
        length = int(self.headers.get("Content-Length", "0") or "0")
        return self.rfile.read(length) if length else b""

    def _bucket_key(self, path):
        trimmed = urllib.parse.unquote(path).lstrip("/")
        if not trimmed:
            return None, None
        bucket, _, key = trimmed.partition("/")
        return bucket, (key or None)

    def _authorized(self, body):
        authorization = self.headers.get("Authorization", "")
        if not authorization.startswith("AWS4-HMAC-SHA256 "):
            return False
        try:
            credential = authorization.split("Credential=", 1)[1].split(",", 1)[0]
            signed = authorization.split("SignedHeaders=", 1)[1].split(",", 1)[0].split(";")
            signature = authorization.split("Signature=", 1)[1].split(",", 1)[0].strip()
            access_key, datestamp, region, service, terminator = credential.split("/")
            expected_access = os.environ.get("R2_S3_ACCESS_KEY_ID")
            if expected_access and access_key != expected_access:
                return False
            amzdate = self.headers["x-amz-date"]
            payload_hash = (
                self.headers.get("x-amz-content-sha256")
                or hashlib.sha256(body).hexdigest()
            )
            parsed = urllib.parse.urlsplit(self.path)
            signed_headers = {}
            for name in signed:
                lowered = name.lower()
                value = self.headers.get(lowered)
                if value is None:
                    return False
                signed_headers[lowered] = " ".join(value.split())
            names = sorted(signed_headers)
            canonical_headers = "".join(
                f"{name}:{signed_headers[name]}\n" for name in names
            )
            canonical = "\n".join(
                [
                    self.command,
                    parsed.path or "/",
                    _load_r2().canonical_query(parsed.query),
                    canonical_headers,
                    ";".join(names),
                    payload_hash,
                ]
            )
            scope = f"{datestamp}/{region}/{service}/{terminator}"
            string_to_sign = "\n".join(
                [
                    "AWS4-HMAC-SHA256",
                    amzdate,
                    scope,
                    hashlib.sha256(canonical.encode()).hexdigest(),
                ]
            )
            secret = os.environ["R2_S3_SECRET_ACCESS_KEY"]
        except (KeyError, IndexError, ValueError):
            return False
        key = ("AWS4" + secret).encode()
        for part in (datestamp, region, service, terminator):
            key = hmac.new(key, part.encode(), hashlib.sha256).digest()
        expected = hmac.new(key, string_to_sign.encode(), hashlib.sha256).hexdigest()
        return hmac.compare_digest(expected, signature)

    def _control(self, parsed, body):
        server = self.server
        if parsed.path == "/_fake/state" and self.command == "GET":
            items = []
            for (bucket, key), (data, content_type) in server.objects.items():
                items.append(
                    {
                        "bucket": bucket,
                        "key": key,
                        "size": len(data),
                        "etag": server.etag(bucket, key, data),
                        "content_type": content_type,
                        "body_base64": base64.b64encode(data).decode(),
                    }
                )
            self._reply(
                200,
                json.dumps({"objects": items}).encode(),
                {"Content-Type": "application/json"},
            )
            return
        if parsed.path == "/_fake/requests" and self.command == "GET":
            self._reply(
                200,
                json.dumps({"requests": server.log}).encode(),
                {"Content-Type": "application/json"},
            )
            return
        if parsed.path == "/_fake/seed" and self.command == "POST":
            document = json.loads(body or b"{}")
            bucket = document.get("bucket")
            key = document.get("key")
            if not isinstance(bucket, str) or not bucket or not isinstance(key, str) or not key:
                self._reply(400, b'{"error":"bucket and key are required"}')
                return
            if "body_base64" in document:
                data = base64.b64decode(document["body_base64"])
            else:
                data = str(document.get("body", "")).encode()
            content_type = document.get("content_type", "")
            etag = document.get("etag") or md5_etag(data)
            server.objects[(bucket, key)] = (data, content_type)
            server.etags[(bucket, key)] = etag
            if key == "current":
                server.current = data.decode("utf-8", "replace")
                server.current_etag = etag
            self._reply(200, b'{"ok":true}')
            return
        if parsed.path == "/_fake/fail" and self.command == "POST":
            document = json.loads(body or b"{}")
            server.fail_remaining = int(document.get("count", 1))
            server.fail_status = int(document.get("status", 429))
            server.fail_retry_after = str(document.get("retry_after", "0"))
            self._reply(200, b'{"ok":true}')
            return
        if parsed.path == "/_fake/conditions" and self.command == "GET":
            conditions = [
                {"if_match": condition, "if_none_match": none_match, "body": text}
                for condition, none_match, text in server.put_conditions
            ]
            self._reply(
                200,
                json.dumps({"conditions": conditions}).encode(),
                {"Content-Type": "application/json"},
            )
            return
        if parsed.path == "/_fake/current-on-head" and self.command == "POST":
            document = json.loads(body or b"{}")
            server.change_current_on_head = bool(document.get("enabled", 1))
            self._reply(200, b'{"ok":true}')
            return
        self._reply(404)

    def _list(self, parsed, query):
        server = self.server
        if server.change_current_on_head:
            server.change_current_on_head = False
            server.current = "g2"
            server.current_etag = '"e2"'
        bucket, _ = self._bucket_key(parsed.path)
        if not bucket:
            self._reply(404)
            return
        prefix = query.get("prefix", [""])[0]
        after = query.get("continuation-token", [""])[0]
        page = int(query.get("max-keys", ["1000"])[0])
        keys = sorted(
            key
            for name, key in server.objects
            if name == bucket and key.startswith(prefix)
        )
        if after:
            keys = [key for key in keys if key > after]
        selected = keys[:page]
        truncated = len(keys) > len(selected)
        entries = []
        for key in selected:
            body, _ = server.objects[(bucket, key)]
            etag = server.etag(bucket, key, body).strip('"')
            entries.append(
                "<Contents>"
                f"<Key>{xml.sax.saxutils.escape(key)}</Key>"
                f"<Size>{len(body)}</Size>"
                f"<ETag>&quot;{etag}&quot;</ETag>"
                "</Contents>"
            )
        token = (
            f"<NextContinuationToken>{xml.sax.saxutils.escape(selected[-1])}</NextContinuationToken>"
            if truncated
            else ""
        )
        body = (
            '<?xml version="1.0" encoding="UTF-8"?>'
            '<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">'
            f"<IsTruncated>{'true' if truncated else 'false'}</IsTruncated>"
            f"{''.join(entries)}{token}"
            "</ListBucketResult>"
        ).encode()
        self._reply(200, body)

    def _object(self, parsed):
        server = self.server
        bucket, key = self._bucket_key(parsed.path)
        if not bucket or not key:
            self._reply(404)
            return
        if self.command == "HEAD" and server.change_current_on_head:
            server.change_current_on_head = False
            server.current = "g2"
            server.current_etag = '"e2"'
        if server.manifest_key and key == server.manifest_key:
            body = server.manifest.encode() if isinstance(server.manifest, str) else server.manifest
            self._reply(200, body, {"ETag": '"manifest"'})
            return
        if key == "current" and server.current is not None:
            self._reply(
                200,
                server.current.encode(),
                {"ETag": server.current_etag},
            )
            return
        found = server.objects.get((bucket, key))
        if found is None:
            self._reply(404)
            return
        body, content_type = found
        headers = {"ETag": server.etag(bucket, key, body)}
        if content_type:
            headers["Content-Type"] = content_type
        self._reply(200, body, headers)

    def _put(self, parsed, body):
        server = self.server
        if parsed.query:
            self._reply(404)
            return
        bucket, key = self._bucket_key(parsed.path)
        if not bucket or not key:
            self._reply(404)
            return
        if key == "current":
            condition = self.headers.get("if-match")
            none_match = self.headers.get("if-none-match")
            text = body.decode("utf-8", "replace")
            server.put_conditions.append((condition, none_match, text))
            if condition is not None and condition != server.current_etag:
                self._reply(412)
                return
            if none_match == "*" and server.current is not None:
                self._reply(412)
                return
            etag = md5_etag(body)
            server.current = text
            server.current_etag = etag
            server.objects[(bucket, key)] = (body, self.headers.get("Content-Type", ""))
            server.etags[(bucket, key)] = etag
            self._log_put(bucket, key)
            self._reply(200, b"", {"ETag": etag})
            return
        etag = md5_etag(body)
        server.objects[(bucket, key)] = (body, self.headers.get("Content-Type", ""))
        server.etags[(bucket, key)] = etag
        self._log_put(bucket, key)
        self._reply(200, b"", {"ETag": etag})

    def _log_put(self, bucket, key):
        if self.server.request_log_style == "put":
            self.server.requests.append(("PUT", bucket, key))

    def _handle(self):
        body = self._read_body()
        parsed = urllib.parse.urlsplit(self.path)
        query = urllib.parse.parse_qs(parsed.query, keep_blank_values=True)
        server = self.server
        if parsed.path.startswith(CONTROL_PREFIX):
            self._control(parsed, body)
            return
        if server.verify_signature and not self._authorized(body):
            self._reply(403, b"<Error><Code>SignatureDoesNotMatch</Code></Error>")
            return
        server.log.append(
            {
                "method": self.command,
                "path": parsed.path,
                "query": query,
                "body_base64": base64.b64encode(body).decode(),
            }
        )
        if server.request_log_style == "full":
            server.requests.append((self.command, parsed.path, query, body))
        if server.fail_remaining > 0:
            server.fail_remaining -= 1
            headers = {}
            if server.fail_retry_after is not None:
                headers["Retry-After"] = server.fail_retry_after
            self._reply(server.fail_status, b"injected failure", headers)
            return
        if self.command == "POST" and "uploads" in query:
            upload_id = f"test-upload-{len(server.uploads) + 1}"
            server.uploads[upload_id] = {}
            self._reply(
                200,
                (
                    "<InitiateMultipartUploadResult>"
                    f"<UploadId>{upload_id}</UploadId>"
                    "</InitiateMultipartUploadResult>"
                ).encode(),
            )
            return
        if self.command == "PUT" and "partNumber" in query:
            upload_id = query.get("uploadId", [""])[0]
            number = int(query["partNumber"][0])
            server.uploads.setdefault(upload_id, {})[number] = body
            self._reply(200, b"", {"ETag": md5_etag(body)})
            return
        if self.command == "POST" and "uploadId" in query:
            if server.fail_complete:
                self._reply(
                    200,
                    b"<Error><Code>InternalError</Code><Message>retry</Message></Error>",
                )
                return
            upload_id = query["uploadId"][0]
            parts = server.uploads.pop(upload_id, {})
            order = sorted(parts)
            data = b"".join(parts[number] for number in order)
            combined = hashlib.md5(b"".join(hashlib.md5(parts[n]).digest() for n in order))
            etag = f'"{combined.hexdigest()}-{len(order)}"'
            bucket, key = self._bucket_key(parsed.path)
            if bucket and key:
                server.objects[(bucket, key)] = (data, self.headers.get("Content-Type", ""))
                server.etags[(bucket, key)] = etag
            self._reply(200, b"<CompleteMultipartUploadResult></CompleteMultipartUploadResult>")
            return
        if self.command == "DELETE":
            upload_id = query.get("uploadId", [None])[0]
            if upload_id:
                server.uploads.pop(upload_id, None)
            self._reply(204)
            return
        if self.command in ("GET", "HEAD") and query.get("list-type") == ["2"]:
            self._list(parsed, query)
            return
        if self.command in ("GET", "HEAD"):
            self._object(parsed)
            return
        if self.command == "PUT":
            self._put(parsed, body)
            return
        self._reply(404)

    do_GET = _handle
    do_HEAD = _handle
    do_PUT = _handle
    do_POST = _handle
    do_DELETE = _handle


def _watch_stdin(stop):
    import select

    while not stop.is_set():
        try:
            ready, _, _ = select.select([sys.stdin], [], [], 0.1)
        except (OSError, ValueError):
            stop.set()
            return
        if not ready:
            continue
        try:
            if not sys.stdin.buffer.read(1):
                stop.set()
                return
        except (OSError, ValueError):
            stop.set()
            return


def main():
    server = FakeS3Server(("127.0.0.1", 0), FakeS3Handler)
    server.verify_signature = "--verify-signature" in sys.argv[1:]
    print(f"PORT {server.server_port}", flush=True)
    stop = threading.Event()

    def terminate(*_args):
        stop.set()

    signal.signal(signal.SIGTERM, terminate)
    signal.signal(signal.SIGINT, terminate)
    watcher = threading.Thread(target=_watch_stdin, args=(stop,))
    watcher.start()
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()
    stop.wait()
    server.shutdown()
    server.server_close()
    watcher.join(timeout=6)
    server_thread.join(timeout=6)


if __name__ == "__main__":
    main()
