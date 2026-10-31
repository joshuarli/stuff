#!/usr/bin/env python3
import datetime
import hashlib
import http.server
import importlib.util
import os
import pathlib
import sys
import tempfile
import threading
import unittest
import urllib.parse

SCRIPT = pathlib.Path(__file__).with_name("r2-multipart.py")
spec = importlib.util.spec_from_file_location("r2_multipart", SCRIPT)
r2 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(r2)

AWS_ACCESS = "AKIAIOSFODNN7EXAMPLE"
AWS_SECRET = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"
MAY_24_2013 = datetime.datetime(2013, 5, 24, tzinfo=datetime.timezone.utc)


class SigV4Tests(unittest.TestCase):
    def setUp(self):
        self.saved = dict(os.environ)
        os.environ["R2_S3_ENDPOINT"] = "https://examplebucket.s3.amazonaws.com"
        os.environ["R2_S3_ACCESS_KEY_ID"] = AWS_ACCESS
        os.environ["R2_S3_SECRET_ACCESS_KEY"] = AWS_SECRET
        os.environ["R2_S3_REGION"] = "us-east-1"
        os.environ.pop("R2_S3_SESSION_TOKEN", None)

    def tearDown(self):
        os.environ.clear()
        os.environ.update(self.saved)

    def test_matches_aws_get_object_vector(self):
        headers = r2.signature_headers(
            "GET",
            "https://examplebucket.s3.amazonaws.com/test.txt",
            r2.EMPTY_SHA256,
            now=MAY_24_2013,
            extra_headers={"range": "bytes=0-9"},
        )
        self.assertTrue(
            headers["Authorization"].endswith(
                "SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, "
                "Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
            ),
            headers["Authorization"],
        )

    def test_initiation_canonicalizes_empty_query_as_uploads_equals(self):
        canonical, signed = r2.canonical_request(
            "POST",
            "https://examplebucket.s3.amazonaws.com/bucket/key?uploads",
            r2.EMPTY_SHA256,
            amzdate="20130524T000000Z",
        )
        self.assertIn("\nuploads=\n", canonical)
        self.assertEqual(signed, "host;x-amz-content-sha256;x-amz-date")

    def test_part_upload_sorts_content_type_before_amz_headers(self):
        canonical, signed = r2.canonical_request(
            "PUT",
            "https://examplebucket.s3.amazonaws.com/bucket/key?partNumber=1&uploadId=abc",
            "a" * 64,
            content_type="application/octet-stream",
            amzdate="20130524T000000Z",
        )
        self.assertIn("\npartNumber=1&uploadId=abc\n", canonical)
        self.assertEqual(
            signed,
            "content-type;host;x-amz-content-sha256;x-amz-date",
        )
        self.assertIn(
            "content-type:application/octet-stream\nhost:examplebucket.s3.amazonaws.com\n",
            canonical,
        )

    def test_completion_canonicalizes_query_in_sorted_order(self):
        canonical, _ = r2.canonical_request(
            "POST",
            "https://examplebucket.s3.amazonaws.com/bucket/key?uploadId=abc",
            "a" * 64,
            content_type="text/plain",
            amzdate="20130524T000000Z",
        )
        self.assertIn("\nuploadId=abc\n", canonical)

    def test_session_token_is_signed_and_sent(self):
        os.environ["R2_S3_SESSION_TOKEN"] = "temporary-token"
        headers = r2.signature_headers(
            "POST",
            "https://examplebucket.s3.amazonaws.com/bucket/key?uploads",
            r2.EMPTY_SHA256,
            now=MAY_24_2013,
        )
        self.assertIn("x-amz-security-token", headers["Authorization"])
        self.assertIn(
            "SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-security-token",
            headers["Authorization"],
        )
        self.assertEqual(headers["x-amz-security-token"], "temporary-token")

    def test_detects_error_body_inside_http_200(self):
        body = b'<?xml version="1.0"?><Error><Code>InternalError</Code><Message>try again</Message></Error>'
        self.assertEqual(r2.completion_error(body), "InternalError")
        self.assertEqual(r2.completion_error(b""), "")


class FakeS3(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def _reply(self, status, body=b"", headers=None):
        self.send_response(status)
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if body:
            self.wfile.write(body)

    def _handle(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
        url = f"http://{self.headers['host']}{self.path}"
        authorization = self.headers["Authorization"]
        signed = authorization.split("SignedHeaders=", 1)[1].split(",", 1)[0].split(";")
        content_type = self.headers.get("content-type", "") if "content-type" in signed else ""
        amzdate = self.headers["x-amz-date"]
        now = datetime.datetime.strptime(amzdate, "%Y%m%dT%H%M%SZ").replace(
            tzinfo=datetime.timezone.utc
        )
        expected = r2.signature_headers(
            self.command,
            url,
            hashlib.sha256(body).hexdigest(),
            content_type,
            now=now,
        )
        if self.headers.get("Authorization") != expected["Authorization"]:
            self._reply(403, b"<Error><Code>SignatureDoesNotMatch</Code></Error>")
            return
        parsed = urllib.parse.urlsplit(self.path)
        query = urllib.parse.parse_qs(parsed.query, keep_blank_values=True)
        self.server.requests.append((self.command, parsed.path, query, body))
        if self.command == "POST" and "uploads" in query:
            self._reply(
                200,
                b"<InitiateMultipartUploadResult><UploadId>test-upload</UploadId></InitiateMultipartUploadResult>",
            )
        elif self.command == "PUT" and "partNumber" in query:
            etag = '"' + hashlib.md5(body).hexdigest() + '"'
            self._reply(200, b"", {"ETag": etag})
        elif self.command == "POST" and "uploadId" in query:
            if self.server.fail_complete:
                self._reply(
                    200,
                    b"<Error><Code>InternalError</Code><Message>retry</Message></Error>",
                )
            else:
                self._reply(200, b"<CompleteMultipartUploadResult></CompleteMultipartUploadResult>")
        elif self.command == "DELETE":
            self._reply(204)
        else:
            self._reply(404)

    do_POST = _handle
    do_PUT = _handle
    do_DELETE = _handle


class MultipartFlowTests(unittest.TestCase):
    def setUp(self):
        self.saved = dict(os.environ)
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), FakeS3)
        self.server.requests = []
        self.server.fail_complete = False
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        os.environ["R2_S3_ENDPOINT"] = f"http://127.0.0.1:{self.server.server_port}"
        os.environ["R2_S3_ACCESS_KEY_ID"] = AWS_ACCESS
        os.environ["R2_S3_SECRET_ACCESS_KEY"] = AWS_SECRET
        os.environ["R2_S3_REGION"] = "auto"
        os.environ.pop("R2_S3_SESSION_TOKEN", None)
        self.saved_part = r2.PART_SIZE
        r2.PART_SIZE = 4
        self.saved_argv = sys.argv

    def tearDown(self):
        r2.PART_SIZE = self.saved_part
        sys.argv = self.saved_argv
        self.server.shutdown()
        self.server.server_close()
        os.environ.clear()
        os.environ.update(self.saved)

    def run_upload(self, payload):
        with tempfile.TemporaryDirectory() as work:
            path = os.path.join(work, "artifact")
            with open(path, "wb") as f:
                f.write(payload)
            sys.argv = ["r2-multipart.py", path, "bucket", "path/artifact", "text/plain"]
            r2.main()

    def test_uploads_parts_and_completes(self):
        self.run_upload(b"abcdefghij")
        methods = [(method, query) for method, _, query, _ in self.server.requests]
        self.assertEqual(methods[0][0], "POST")
        self.assertIn("uploads", methods[0][1])
        parts = [query["partNumber"][0] for method, _, query, _ in self.server.requests if method == "PUT"]
        self.assertEqual(parts, ["1", "2", "3"])
        completion = self.server.requests[-1]
        self.assertEqual(completion[0], "POST")
        self.assertIn(b"<PartNumber>3</PartNumber>", completion[3])

    def test_completion_error_inside_http_200_aborts(self):
        self.server.fail_complete = True
        with self.assertRaises(SystemExit):
            self.run_upload(b"abcdefghij")
        self.assertEqual(self.server.requests[-1][0], "DELETE")


if __name__ == "__main__":
    unittest.main()

