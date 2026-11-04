import gzip
import hashlib
import io
import json
import struct
import tarfile
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

PACKAGE_ROOT = Path(__file__).resolve().parents[1]
GOLDEN = Path(__file__).resolve().parent / "golden"


def make_crate(name, version, manifest_extra="", files=None, package_extra=""):
    manifest = (
        f'[package]\nname = "{name}"\nversion = "{version}"\nedition = "2021"\n{package_extra}'
        f"{manifest_extra}"
    )
    entries = {f"{name}-{version}/Cargo.toml": manifest.encode(), f"{name}-{version}/src/lib.rs": b"pub fn f() {}\n"}
    for relative, data in (files or {}).items():
        entries[f"{name}-{version}/{relative}"] = data
    return build_archive(entries)


def build_archive(entries):
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.GNU_FORMAT) as archive:
        for path, data in entries.items():
            info = tarfile.TarInfo(path)
            info.size = len(data)
            info.mode = 0o644
            info.mtime = 0
            archive.addfile(info, io.BytesIO(data))
    return gzip_bytes(raw.getvalue())


def gzip_bytes(data):
    out = io.BytesIO()
    with gzip.GzipFile(fileobj=out, mode="wb", mtime=0) as handle:
        handle.write(data)
    return out.getvalue()


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def wire_payload(metadata, crate_bytes):
    body = json.dumps(metadata).encode()
    return struct.pack("<I", len(body)) + body + struct.pack("<I", len(crate_bytes)) + crate_bytes


def publish_metadata(name, vers, deps=(), features=None, **extra):
    metadata = {"name": name, "vers": vers, "deps": list(deps), "features": features or {}}
    metadata.update(extra)
    return metadata


def dependency(name, req="^1.0", **fields):
    out = {
        "name": name,
        "version_req": req,
        "features": [],
        "optional": False,
        "default_features": True,
        "target": None,
        "kind": "normal",
        "registry": None,
        "explicit_name_in_toml": None,
        "package": None,
    }
    out.update(fields)
    return out


def http(method, url, data=None, headers=None):
    request = urllib.request.Request(url, data=data, method=method, headers=headers or {})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, dict(response.headers), response.read()
    except urllib.error.HTTPError as error:
        with error:
            return error.code, dict(error.headers), error.read()


class ScratchTestCase(unittest.TestCase):
    def setUp(self):
        self._scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self._scratch.cleanup)
        self.tmp = Path(self._scratch.name)


class RunningServer:
    def __init__(self, httpd, thread):
        self.httpd = httpd
        self.thread = thread
        self.url = httpd.base_url

    def close(self):
        self.httpd.shutdown()
        self.httpd.server_close()
        self.thread.join(timeout=10)


def start_server(state, site, token="secret", auth_required=False, public_base_url=None, **overrides):
    from mycrates import server
    from mycrates.config import Config
    from mycrates.publish import Publisher
    from mycrates.store import Store

    config = Config.load(Path(state) / "config.json")
    settings = dict(
        bind_host="127.0.0.1",
        bind_port=0,
        token=token,
        auth_required=auth_required,
        state_dir=Path(state),
        site_dir=Path(site),
        public_base_url=public_base_url or config.public_base_url,
        limits=config.limits,
        log=lambda message: None,
    )
    settings.update(overrides)
    httpd = server.PublishServer(server.ServeConfig(**settings))
    if public_base_url == "local":
        httpd.config.public_base_url = httpd.base_url
        store = Store(state)
        with store.lock_exclusive():
            Publisher(store, site, httpd.base_url, auth_required).publish()
    return RunningServer(httpd, server.start_in_thread(httpd))


def cli(*args):
    import contextlib

    from mycrates.cli import main

    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        code = main([str(a) for a in args])
    return code, out.getvalue(), err.getvalue()


class FakeUpstream:
    def __init__(self):
        self.crates = {}
        self.index_overrides = {}
        self.download_overrides = {}
        self.requests = []
        self.download_hook = None
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                outer.requests.append(self.path)
                body = outer.respond(self.path)
                if body is None:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *args):
                pass

        self.httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.port = self.httpd.server_address[1]
        self.url = f"http://127.0.0.1:{self.port}/"
        threading.Thread(target=self.httpd.serve_forever, kwargs={"poll_interval": 0.02}, daemon=True).start()

    def add(self, name, version, **kwargs):
        data = make_crate(name, version, **kwargs)
        self.crates[(name, version)] = data
        return sha256(data)

    def entry_line(self, name, version):
        checksum = self.index_overrides.get((name, version), sha256(self.crates[(name, version)]))
        entry = {"name": name, "vers": version, "deps": [], "cksum": checksum, "features": {}, "yanked": False}
        return json.dumps(entry, separators=(",", ":"))

    def respond(self, path):
        from mycrates.names import shard_for_name

        if path == "/config.json":
            return json.dumps({"dl": f"{self.url}dl/{{crate}}/{{version}}"}).encode()
        for (name, version), data in self.crates.items():
            if path == f"/dl/{name}/{version}":
                if self.download_hook:
                    self.download_hook(name, version)
                return self.download_overrides.get((name, version), data)
        for name in {n for n, _ in self.crates}:
            if path == "/" + shard_for_name(name.lower()):
                lines = [self.entry_line(n, v) for (n, v) in sorted(self.crates) if n == name]
                return ("\n".join(lines) + "\n").encode()
        return None

    def close(self):
        self.httpd.shutdown()
        self.httpd.server_close()


def lockfile_text(packages):
    out = "version = 4\n"
    for name, version, checksum in packages:
        out += (
            f'\n[[package]]\nname = "{name}"\nversion = "{version}"\n'
            f'source = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "{checksum}"\n'
        )
    return out


def init_state(tmp, public_base_url=None, **config_changes):
    from mycrates.config import Config

    state, site = Path(tmp) / "state", Path(tmp) / "site"
    code, _, err = cli("init", "--state-dir", state, "--site-dir", site)
    assert code == 0, err
    config = Config.load(state / "config.json")
    if public_base_url:
        config.public_base_url = public_base_url
    for key, value in config_changes.items():
        setattr(config.limits if hasattr(config.limits, key) else config, key, value)
    config.save(state / "config.json")
    return state, site
