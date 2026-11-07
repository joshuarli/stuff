#!/usr/bin/env python3

import importlib.util
import json
import os
import sys

_BASE_PATH = os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "../../../../deploy/fake_s3.py"
)
_BASE_DIR = os.path.dirname(_BASE_PATH)
if _BASE_DIR not in sys.path:
    sys.path.insert(0, _BASE_DIR)
_spec = importlib.util.spec_from_file_location("fake_s3_base", _BASE_PATH)
base = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(base)


class RetryServer(base.FakeS3Server):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.reject_current_remaining = 0
        self.reject_current_generation = None


class RetryHandler(base.FakeS3Handler):
    def _control(self, parsed, body):
        if parsed.path == "/_fake/reject-current" and self.command == "POST":
            document = json.loads(body or b"{}")
            self.server.reject_current_remaining = int(document.get("count", 1))
            self.server.reject_current_generation = document.get("generation") or None
            self._reply(200, b'{"ok":true}')
            return
        super()._control(parsed, body)

    def _put(self, parsed, body):
        server = self.server
        bucket, key = self._bucket_key(parsed.path)
        if not parsed.query and key == "current" and server.reject_current_remaining > 0:
            server.reject_current_remaining -= 1
            server.put_conditions.append(
                (
                    self.headers.get("if-match"),
                    self.headers.get("if-none-match"),
                    body.decode("utf-8", "replace"),
                )
            )
            generation = server.reject_current_generation
            if generation:
                data = generation.encode()
                etag = base.md5_etag(data)
                if isinstance(server.current, dict):
                    server.current[bucket] = generation
                else:
                    server.current = {bucket: generation}
                if isinstance(server.current_etag, dict):
                    server.current_etag[bucket] = etag
                else:
                    server.current_etag = {bucket: etag}
                server.objects[(bucket, "current")] = (data, "text/plain")
                server.etags[(bucket, "current")] = etag
                server.reject_current_generation = None
            self._reply(412)
            return
        super()._put(parsed, body)


base.FakeS3Server = RetryServer
base.FakeS3Handler = RetryHandler

if __name__ == "__main__":
    base.main()
