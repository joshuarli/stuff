"""Minimal SigV4 client over TCP or a unix socket (standard library only).

Used by `bin/s3-conformance` (S3 probes), `bin/oo` (store admin API, which is
S3-style signed) and the backup/restore checks.
"""
import datetime
import hashlib
import hmac
import http.client
import socket
import urllib.parse


def _h(key: bytes, msg: str) -> bytes:
    return hmac.new(key, msg.encode(), hashlib.sha256).digest()


class _UnixConn(http.client.HTTPConnection):
    def __init__(self, path: str, timeout: float):
        super().__init__("localhost", timeout=timeout)
        self._path = path

    def connect(self):
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(self.timeout)
        s.connect(self._path)
        self.sock = s


class S3:
    def __init__(self, endpoint: str, region: str, access_key: str, secret_key: str, timeout: float = 60):
        """`endpoint` is http(s)://host:port or unix:/path/to.sock."""
        self.unix = endpoint[len("unix:"):] if endpoint.startswith("unix:") else None
        if self.unix:
            self.scheme, self.host, self.port = "http", "localhost", 0
        else:
            u = urllib.parse.urlparse(endpoint)
            self.scheme, self.host = u.scheme, u.hostname
            self.port = u.port or (443 if u.scheme == "https" else 80)
        self.region, self.ak, self.sk, self.timeout = region, access_key, secret_key, timeout

    def request(self, method, path, query=None, headers=None, body=b""):
        query = query or {}
        now = datetime.datetime.now(datetime.timezone.utc)
        amz, day = now.strftime("%Y%m%dT%H%M%SZ"), now.strftime("%Y%m%d")
        payload_hash = hashlib.sha256(body).hexdigest()
        host = "localhost" if self.unix else f"{self.host}:{self.port}"
        h = {k.lower(): str(v).strip() for k, v in (headers or {}).items()}
        h.update({"host": host, "x-amz-date": amz, "x-amz-content-sha256": payload_hash})
        signed = ";".join(sorted(h))
        canon_q = "&".join(
            f"{urllib.parse.quote(k, safe='-_.~')}={urllib.parse.quote(str(v), safe='-_.~')}"
            for k, v in sorted(query.items())
        )
        canon_path = urllib.parse.quote(path, safe="/-_.~")
        canon = "\n".join([method, canon_path, canon_q, "".join(f"{k}:{h[k]}\n" for k in sorted(h)), signed, payload_hash])
        scope = f"{day}/{self.region}/s3/aws4_request"
        sts = "\n".join(["AWS4-HMAC-SHA256", amz, scope, hashlib.sha256(canon.encode()).hexdigest()])
        k = _h(_h(_h(_h(("AWS4" + self.sk).encode(), day), self.region), "s3"), "aws4_request")
        h["authorization"] = (f"AWS4-HMAC-SHA256 Credential={self.ak}/{scope}, SignedHeaders={signed}, "
                              f"Signature={hmac.new(k, sts.encode(), hashlib.sha256).hexdigest()}")
        if self.unix:
            conn = _UnixConn(self.unix, self.timeout)
        elif self.scheme == "https":
            conn = http.client.HTTPSConnection(self.host, self.port, timeout=self.timeout)
        else:
            conn = http.client.HTTPConnection(self.host, self.port, timeout=self.timeout)
        url = canon_path + (("?" + canon_q) if canon_q else "")
        try:
            conn.request(method, url, body=body if method != "GET" else None, headers={k: v for k, v in h.items() if k != "host"})
            r = conn.getresponse()
            data = r.read()
            return r.status, {k.lower(): v for k, v in r.getheaders()}, data
        finally:
            conn.close()
