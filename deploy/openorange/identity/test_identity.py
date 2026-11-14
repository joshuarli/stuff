"""Unit tests for the identity sidecar with a fake tailscaled LocalAPI.

Run: python3 -m unittest -v test_identity   (from this directory)
The tagged-node cases use canned whois documents: no tagged device exists on
the tailnet yet, so they prove the mapping logic, not a real tagged peer.
"""
import json
import os
import socketserver
import tempfile
import threading
import unittest
import http.client
import socket
import urllib.parse
from http.server import BaseHTTPRequestHandler

import identity

DOCS = {
    "100.64.0.5:1111": {"Node": {"Tags": None}, "UserProfile": {"LoginName": "alice@example.com"}},
    "100.64.0.6:2222": {"Node": {"Tags": ["tag:ci", "tag:build"]}, "UserProfile": {"LoginName": "tagged-devices"}},
    "100.64.0.7:3333": {"Node": {"Tags": None}, "UserProfile": {"LoginName": "bad login\r\nX: y"}},
    "100.64.0.8:4444": {"Node": {"Tags": ["tag:a,b"]}, "UserProfile": {}},
    "[fd7a:115c:a1e0::1]:5555": {"Node": {}, "UserProfile": {"LoginName": "v6@example.com"}},
}


class FakeLocalAPI(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_GET(self):
        q = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query)
        addr = q.get("addr", [""])[0]
        if addr == "100.64.0.99:1":
            self.send_response(500)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        doc = DOCS.get(addr)
        body = json.dumps(doc if doc is not None else {"error": "no match"}).encode()
        self.send_response(200 if doc is not None else 404)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class UnixServer(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True

    def get_request(self):
        r, _ = super().get_request()
        return r, ("u", 0)


class Conn(http.client.HTTPConnection):
    def __init__(self, path):
        super().__init__("x")
        self.p = path

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.connect(self.p)


class IdentityTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.mkdtemp()
        cls.ts = os.path.join(cls.tmp, "ts.sock")
        cls.ids = os.path.join(cls.tmp, "id.sock")
        cls.fake = UnixServer(cls.ts, FakeLocalAPI)
        threading.Thread(target=cls.fake.serve_forever, daemon=True).start()
        identity.LOCALAPI_SOCKET = cls.ts
        cls.srv = identity.Server(cls.ids, identity.Handler)
        threading.Thread(target=cls.srv.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.srv.shutdown()
        cls.fake.shutdown()
        cls.srv.server_close()
        cls.fake.server_close()

    def ask(self, peer, extra=None):
        c = Conn(self.ids)
        h = dict(extra or {})
        if peer is not None:
            h["X-OO-Peer"] = peer
        c.request("GET", "/auth", headers=h)
        r = c.getresponse()
        r.read()
        out = (r.status, r.getheader("Stats-Tailnet-Identity"))
        c.close()
        return out

    def test_user(self):
        self.assertEqual(self.ask("100.64.0.5:1111"), (200, "user:alice@example.com"))

    def test_user_v6(self):
        self.assertEqual(self.ask("[fd7a:115c:a1e0::1]:5555"), (200, "user:v6@example.com"))

    def test_tagged_node_canned(self):
        self.assertEqual(self.ask("100.64.0.6:2222"), (200, "tags:tag:build,tag:ci"))

    def test_unknown_peer_forbidden(self):
        self.assertEqual(self.ask("100.64.0.50:1"), (403, None))

    def test_non_tailnet_address_forbidden_without_lookup(self):
        self.assertEqual(self.ask("10.0.1.6:5000"), (403, None))
        self.assertEqual(self.ask("127.0.0.1:80"), (403, None))

    def test_missing_or_malformed_peer(self):
        self.assertEqual(self.ask(None), (403, None))
        self.assertEqual(self.ask("not-an-ip"), (403, None))
        self.assertEqual(self.ask("100.64.0.5"), (403, None))

    def test_header_injection_in_login_rejected(self):
        self.assertEqual(self.ask("100.64.0.7:3333"), (403, None))

    def test_comma_in_tag_rejected(self):
        self.assertEqual(self.ask("100.64.0.8:4444"), (403, None))

    def test_localapi_error_fails_closed(self):
        self.assertEqual(self.ask("100.64.0.99:1"), (503, None))

    def test_client_supplied_identity_headers_are_ignored(self):
        st, ident = self.ask("100.64.0.50:1", {"Stats-Tailnet-Identity": "user:evil", "Tailscale-User-Login": "evil"})
        self.assertEqual((st, ident), (403, None))

    def test_localapi_down(self):
        saved = identity.LOCALAPI_SOCKET
        identity.LOCALAPI_SOCKET = os.path.join(self.tmp, "absent.sock")
        try:
            self.assertEqual(self.ask("100.64.0.5:1111"), (503, None))
        finally:
            identity.LOCALAPI_SOCKET = saved


if __name__ == "__main__":
    unittest.main()
