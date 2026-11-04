import hashlib
import hmac
import io
import mimetypes
import os
import re
import socket
import stat as stat_module
import sys
import threading
import time
import traceback
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote, urlsplit

from . import wirejson
from .config import LimitsConfig
from .crate_file import ScanLimits, parse_packaged_manifest, scan_crate_staged
from .errors import Error, InputError, IntegrityError, http_status
from .index import publish_to_index
from .publish import Publisher
from .store import NativeRecord, Store
from .wire import FRAMING_SLACK, decode_publish_payload

LOOPBACK_HOSTS = ("127.0.0.1", "localhost", "::1")
WRITE_TIMEOUT = 30
BLOB_PATH = re.compile(r"^/blobs/([0-9a-fA-F]{64})\.crate$")
YANK_PATH = re.compile(r"^/api/v1/crates/([^/]+)/([^/]+)/(yank|unyank)$")
PUBLISH_RESPONSE = '{"warnings":{"invalid_categories":[],"invalid_badges":[],"other":[]}}'
CHUNK = 65536
REASONS = {
    400: "Bad Request",
    401: "Unauthorized",
    404: "Not Found",
    408: "Request Timeout",
    409: "Conflict",
    411: "Length Required",
    413: "Payload Too Large",
    422: "Unprocessable Entity",
    502: "Bad Gateway",
    503: "Service Unavailable",
}


@dataclass
class ServeConfig:
    bind_host: str
    bind_port: int
    token: str
    auth_required: bool
    state_dir: Path
    site_dir: Path
    public_base_url: str
    limits: LimitsConfig
    header_timeout: float = 30
    body_timeout: float = 300
    log: object = None


def split_bind(bind):
    host, separator, port = bind.rpartition(":")
    if not separator or not port.isdigit():
        raise InputError(f"--bind must look like HOST:PORT, got {bind!r}")
    host = host.strip("[]")
    if host not in LOOPBACK_HOSTS:
        raise InputError(f"serve-publish only binds to loopback ({', '.join(LOOPBACK_HOSTS)}), not {host!r}")
    return host, int(port)


class DeadlineReader(io.RawIOBase):
    def __init__(self, connection, timeout):
        self._connection = connection
        self.deadline = time.monotonic() + timeout

    def reset(self, timeout):
        self.deadline = time.monotonic() + timeout

    def readable(self):
        return True

    def readinto(self, buffer):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("request deadline exceeded")
        self._connection.settimeout(remaining)
        try:
            return self._connection.recv_into(buffer)
        except socket.timeout:
            raise TimeoutError("request deadline exceeded") from None


class Admission:
    def __init__(self, limit):
        self.limit = limit
        self.active = 0
        self._mutex = threading.Lock()

    def try_enter(self):
        with self._mutex:
            if self.active >= self.limit:
                return False
            self.active += 1
            return True

    def leave(self):
        with self._mutex:
            self.active -= 1


class PublishServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True
    request_queue_size = 128

    def __init__(self, config):
        host, port = config.bind_host, config.bind_port
        if host not in LOOPBACK_HOSTS:
            raise InputError(f"serve-publish only binds to loopback, not {host!r}")
        if not config.token:
            raise InputError("refusing to serve the publish API without a token")
        self.address_family = socket.AF_INET6 if ":" in host else socket.AF_INET
        super().__init__((host, port), PublishHandler)
        self.config = config
        self.connections = Admission(config.limits.serve_connections)
        self.writes = Admission(config.limits.serve_writes)

    def process_request(self, request, client_address):
        if not self.connections.try_enter():
            try:
                body = _error_body("too many concurrent connections")
                request.sendall(_response_head(503, "application/json", len(body)) + body)
            except OSError:
                pass
            self.shutdown_request(request)
            return
        super().process_request(request, client_address)

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self.connections.leave()

    @property
    def base_url(self):
        host, port = self.server_address[:2]
        return f"http://{host}:{port}"


def _error_body(detail):
    return wirejson.compact({"errors": [{"detail": detail}]}).encode("utf-8")


def _response_head(status, content_type, length, extra=()):
    lines = [
        f"HTTP/1.1 {status} {REASONS.get(status, 'Internal Server Error')}",
        f"Content-Type: {content_type}",
        f"Content-Length: {length}",
        "Connection: close",
        *extra,
    ]
    return ("\r\n".join(lines) + "\r\n\r\n").encode("latin-1")


class PublishHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "mycrates"

    def setup(self):
        self.connection = self.request
        self.connection.settimeout(WRITE_TIMEOUT)
        self._reader = DeadlineReader(self.connection, self.server.config.header_timeout)
        self.rfile = io.BufferedReader(self._reader)
        self.wfile = self.connection.makefile("wb", 0)

    def log_message(self, format, *args):
        pass

    def log_failure(self, message):
        line = f"serve-publish: {self.command} {self.path}: {message}"
        log = self.server.config.log
        if log is None:
            print(line, file=sys.stderr, flush=True)
        else:
            log(line)

    def send_error(self, code, message=None, explain=None):
        self.send_json(code, _error_body(message or REASONS.get(code, "error")))

    def send_json(self, status, body, content_type="application/json"):
        if isinstance(body, str):
            body = body.encode("utf-8")
        self.close_connection = True
        try:
            self.wfile.write(_response_head(status, content_type, len(body)) + body)
        except OSError:
            pass

    def send_failure(self, error):
        self.log_failure(str(error))
        self.send_json(http_status(error), _error_body(str(error)))

    def handle_one_request(self):
        self.close_connection = True
        try:
            self.raw_requestline = self.rfile.readline(65537)
            if len(self.raw_requestline) > 65536:
                self.send_error(400, "request line too long")
                return
            if not self.raw_requestline:
                return
            if not self.parse_request():
                return
            self._reader.reset(self.server.config.body_timeout)
            self.dispatch()
        except TimeoutError:
            self.send_error(408, "request timed out")
        except Exception as error:
            traceback.print_exc()
            self.send_json(500, _error_body(f"internal error: {error}"))

    def authorized(self):
        config = self.server.config
        if self.command in ("GET", "HEAD") and not config.auth_required:
            return True
        header = (self.headers.get("Authorization") or "").strip()
        if not header:
            return False
        provided = header[len("Bearer ") :] if header.startswith("Bearer ") else header
        return hmac.compare_digest(
            hashlib.sha256(provided.strip().encode()).digest(), hashlib.sha256(config.token.encode()).digest()
        )

    def dispatch(self):
        if not self.authorized():
            self.send_error(401, "unauthorized")
            return
        path = urlsplit(self.path).path
        if self.command in ("GET", "HEAD"):
            self.serve_static(path)
            return
        if not self.server.writes.try_enter():
            self.send_error(503, "too many concurrent writes")
            return
        try:
            self.dispatch_write(path)
        finally:
            self.server.writes.leave()

    def dispatch_write(self, path):
        if self.command == "PUT" and path == "/api/v1/crates/new":
            self.handle_publish()
            return
        match = YANK_PATH.match(path)
        if match and (
            (self.command == "DELETE" and match.group(3) == "yank")
            or (self.command == "PUT" and match.group(3) == "unyank")
        ):
            self.handle_yank(unquote(match.group(1)), unquote(match.group(2)), self.command == "DELETE")
            return
        self.send_error(404, "not found")

    def publisher(self, store):
        config = self.server.config
        return Publisher(store, config.site_dir, config.public_base_url, config.auth_required)

    def handle_yank(self, name, version, yanked):
        store = Store(self.server.config.state_dir)
        try:
            with store.lock_exclusive():
                store.set_native_yank(name, version, yanked)
                self.publisher(store).publish()
        except InputError as error:
            self.send_error(404, error.message)
            return
        except Error as error:
            self.send_failure(error)
            return
        self.send_json(200, '{"ok":true}')

    def handle_publish(self):
        config = self.server.config
        if "chunked" in (self.headers.get("Transfer-Encoding") or "").lower():
            self.send_error(411, "chunked transfer-encoding not supported")
            return
        length_text = self.headers.get("Content-Length")
        if length_text is None or not length_text.strip().isdigit():
            self.send_error(411, "content-length required")
            return
        content_length = int(length_text)
        if content_length > config.limits.max_compressed + FRAMING_SLACK:
            self.send_error(413, "payload exceeds size limit")
            return
        store = Store(config.state_dir)
        store.ensure_dirs()
        staged = None
        try:
            staged = decode_publish_payload(self.rfile, content_length, config.limits.max_compressed, store.uploads_dir)
            with open(staged.path, "rb") as crate:
                scan = scan_crate_staged(
                    crate,
                    staged.crate_len,
                    config.limits.max_compressed,
                    staged.crate_sha256,
                    staged.body.name,
                    staged.body.vers,
                    ScanLimits.from_config(config.limits),
                )
            with store.lock_exclusive():
                if staged.crate_sha256 != scan.checksum:
                    raise IntegrityError("checksum mismatch")
                body = staged.body
                body.checksum = staged.crate_sha256
                entry = publish_to_index(body, parse_packaged_manifest(scan.cargo_toml))
                if store.ingest_staged_file(staged.path, staged.crate_sha256) != body.checksum:
                    raise IntegrityError("checksum mismatch")
                store.put_native_record(NativeRecord(entry))
                self.publisher(store).publish()
        except Error as error:
            self.send_failure(error)
            return
        finally:
            if staged is not None:
                staged.discard()
        self.send_json(200, PUBLISH_RESPONSE)

    def serve_static(self, path):
        site = self.server.config.site_dir
        segments = [unquote(s) for s in path.split("/") if s]
        if not segments or any(s.startswith(".") or "/" in s or "\0" in s for s in segments):
            self.send_error(404, "not found")
            return
        blob = BLOB_PATH.match(path)
        if segments[0] not in ("blobs", "current", "generations") or (segments[0] == "blobs" and not blob):
            self.send_error(404, "not found")
            return
        root = Path(os.path.realpath(site))
        target = Path(os.path.realpath(site.joinpath(*segments)))
        if root not in target.parents:
            self.send_error(404, "not found")
            return
        try:
            handle = open(target, "rb")
        except OSError:
            self.send_error(404, "not found")
            return
        with handle:
            stat = os.fstat(handle.fileno())
            if not stat_module.S_ISREG(stat.st_mode):
                self.send_error(404, "not found")
                return
            if blob:
                etag = f'"{blob.group(1)}"'
                cache = "public, max-age=31536000, immutable"
                content_type = "application/x-tar"
            else:
                etag = f'"{stat.st_size:x}-{stat.st_mtime_ns:x}"'
                cache = "public, max-age=60"
                content_type = "application/json" if target.suffix == ".json" else (
                    mimetypes.guess_type(target.name)[0] or "text/plain; charset=utf-8"
                )
            if self.headers.get("If-None-Match") == etag:
                self.close_connection = True
                self.wfile.write(
                    _response_head(304, content_type, 0, (f"ETag: {etag}", f"Cache-Control: {cache}"))
                )
                return
            self.close_connection = True
            head = _response_head(200, content_type, stat.st_size, (f"ETag: {etag}", f"Cache-Control: {cache}"))
            try:
                self.wfile.write(head)
                if self.command == "GET":
                    while chunk := handle.read(CHUNK):
                        self.wfile.write(chunk)
            except OSError:
                pass


def start_in_thread(server):
    thread = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
    thread.start()
    return thread

