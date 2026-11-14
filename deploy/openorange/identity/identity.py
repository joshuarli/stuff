#!/usr/bin/env python3
"""Tailnet identity sidecar for the ingress `forward_auth` hook.

Caddy sends one subrequest per client request to a unix socket and puts the
TCP peer address (`ip:port`, taken from the accepted connection, never from a
client header) into `X-OO-Peer`. This program asks tailscaled's LocalAPI who
owns that address:

  GET /localapi/v0/whois?addr=<ip:port>   over /var/run/tailscale/tailscaled.sock

and answers

  200 + `Stats-Tailnet-Identity: user:<login>`          peer is a user's device
  200 + `Stats-Tailnet-Identity: tags:<tag>,<tag>...`   peer is a tagged node
  403                                                    not a tailnet peer, or malformed
  503                                                    tailscaled unavailable (fail closed)

There is no allowlist: whatever the tailnet policy lets reach the ingress is
admitted. Standard library only.
"""
import http.client
import ipaddress
import json
import os
import re
import signal
import socket
import socketserver
import sys
import time
from http.server import BaseHTTPRequestHandler

LOCALAPI_SOCKET = os.environ.get("OO_TAILSCALE_SOCKET", "/var/run/tailscale/tailscaled.sock")
LISTEN_SOCKET = os.environ.get("OO_IDENTITY_SOCKET", "/run/identity/identity.sock")
TIMEOUT = float(os.environ.get("OO_WHOIS_TIMEOUT", "3"))
IDENT_HEADER = "Stats-Tailnet-Identity"
PEER_HEADER = "X-OO-Peer"

TAILNET_V4 = ipaddress.ip_network("100.64.0.0/10")
TAILNET_V6 = ipaddress.ip_network("fd7a:115c:a1e0::/48")
# Printable ASCII only, no spaces: the value is put into an HTTP header.
SAFE = re.compile(r"^[\x21-\x7e]{1,256}$")


class LocalAPIUnavailable(Exception):
    pass


class _UnixHTTP(http.client.HTTPConnection):
    def __init__(self, path: str, timeout: float):
        super().__init__("local-tailscaled.sock", timeout=timeout)
        self._path = path

    def connect(self):
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(self.timeout)
        s.connect(self._path)
        self.sock = s


def parse_peer(value: str):
    """Return the normalized `ip:port` string for a tailnet address, else None."""
    if not value or len(value) > 64:
        return None
    host, sep, port = value.rpartition(":")
    if not sep or not port.isdigit() or not 0 < int(port) < 65536:
        return None
    host = host.strip("[]")
    try:
        ip = ipaddress.ip_address(host)
    except ValueError:
        return None
    if ip.version == 4 and ip not in TAILNET_V4:
        return None
    if ip.version == 6 and ip not in TAILNET_V6:
        return None
    return f"[{ip}]:{port}" if ip.version == 6 else f"{ip}:{port}"


def whois(addr: str, socket_path: str | None = None, timeout: float | None = None):
    """Return the parsed whois document, None for an unknown address."""
    conn = _UnixHTTP(socket_path or LOCALAPI_SOCKET, TIMEOUT if timeout is None else timeout)
    try:
        conn.request("GET", "/localapi/v0/whois?addr=" + addr.replace("[", "%5B").replace("]", "%5D"))
        r = conn.getresponse()
        body = r.read(1 << 20)
    except (OSError, http.client.HTTPException) as e:
        raise LocalAPIUnavailable(str(e)) from e
    finally:
        conn.close()
    if r.status == 404:
        return None
    if r.status != 200:
        if 400 <= r.status < 500:
            return None
        raise LocalAPIUnavailable(f"localapi status {r.status}")
    try:
        return json.loads(body)
    except ValueError as e:
        raise LocalAPIUnavailable("localapi returned invalid JSON") from e


def identity_of(doc) -> str | None:
    """Map a whois document to `user:<login>` / `tags:<list>`, else None."""
    if not isinstance(doc, dict):
        return None
    node = doc.get("Node") or {}
    tags = node.get("Tags") or []
    if tags:
        if not all(isinstance(t, str) and SAFE.match(t) and "," not in t for t in tags):
            return None
        return "tags:" + ",".join(sorted(tags))
    login = (doc.get("UserProfile") or {}).get("LoginName")
    if isinstance(login, str) and SAFE.match(login) and login != "tagged-devices":
        return "user:" + login
    return None


class Handler(BaseHTTPRequestHandler):
    server_version = "oo-identity"
    sys_version = ""
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):  # replaced by explicit decision logs
        pass

    def _decide(self):
        peers = self.headers.get_all(PEER_HEADER) or []
        if len(peers) != 1:
            return 403, None, "missing or repeated peer header"
        addr = parse_peer(peers[0])
        if addr is None:
            return 403, None, f"peer is not a tailnet address: {peers[0][:64]!r}"
        try:
            doc = whois(addr)
        except LocalAPIUnavailable as e:
            return 503, None, f"tailscaled unavailable: {e}"
        ident = identity_of(doc)
        if ident is None:
            return 403, None, f"no tailnet identity for {addr}"
        return 200, ident, f"{addr} -> {ident}"

    def _handle(self):
        status, ident, why = self._decide()
        sys.stderr.write(f"{time.strftime('%Y-%m-%dT%H:%M:%S%z')} {status} {why}\n")
        sys.stderr.flush()
        body = b"" if status == 200 else (b"forbidden\n" if status == 403 else b"identity unavailable\n")
        self.send_response(status)
        if ident:
            self.send_header(IDENT_HEADER, ident)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        if body and self.command != "HEAD":
            self.wfile.write(body)

    do_GET = do_HEAD = do_POST = do_PUT = do_PATCH = do_DELETE = do_OPTIONS = _handle


class Server(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True
    request_queue_size = 128

    def get_request(self):
        req, _ = super().get_request()
        return req, ("unix", 0)

    def handle_error(self, request, client_address):
        # Peers that hang up mid-request are routine; keep real faults loud.
        exc = sys.exc_info()[1]
        if isinstance(exc, (ConnectionError, TimeoutError)):
            sys.stderr.write(f"client hung up: {type(exc).__name__}\n")
        else:
            super().handle_error(request, client_address)


def main():
    try:
        os.unlink(LISTEN_SOCKET)
    except FileNotFoundError:
        pass
    old = os.umask(0o077)
    srv = Server(LISTEN_SOCKET, Handler)
    os.umask(old)
    os.chmod(LISTEN_SOCKET, 0o600)
    sys.stderr.write(f"identity sidecar listening on {LISTEN_SOCKET}\n")
    sys.stderr.flush()
    def stop(signum, frame):
        raise SystemExit(0)

    # PID 1 in a container gets no default SIGTERM action: without this
    # handler `docker stop` waits out its timeout and kills us.
    signal.signal(signal.SIGTERM, stop)
    try:
        srv.serve_forever()
    except (KeyboardInterrupt, SystemExit):
        pass
    finally:
        srv.server_close()
        try:
            os.unlink(LISTEN_SOCKET)
        except FileNotFoundError:
            pass


if __name__ == "__main__":
    main()
