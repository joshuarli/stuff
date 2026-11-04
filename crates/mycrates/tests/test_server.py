import concurrent.futures
import json
import socket
import struct
import threading
import time
import unittest

from mycrates import wirejson
from mycrates.cli import main as cli_main
from mycrates.config import Config
from mycrates.errors import InputError
from mycrates.server import PublishServer, ServeConfig, split_bind
from mycrates.store import Store

from .support import (
    ScratchTestCase,
    cli,
    dependency,
    http,
    init_state,
    make_crate,
    publish_metadata,
    sha256,
    start_server,
    wire_payload,
)

TOKEN = "server-token"
AUTH = {"Authorization": TOKEN}


class ServerCase(ScratchTestCase):
    auth_required = False
    overrides = {}

    def setUp(self):
        super().setUp()
        self.state, self.site = init_state(self.tmp)
        self.running = start_server(
            self.state, self.site, token=TOKEN, auth_required=self.auth_required, public_base_url="local", **self.overrides
        )
        self.addCleanup(self.running.close)
        self.url = self.running.url

    def put(self, metadata, crate, headers=AUTH):
        return http("PUT", f"{self.url}/api/v1/crates/new", wire_payload(metadata, crate), headers)

    def publish(self, name, version="1.0.0", deps=(), **kwargs):
        crate = make_crate(name, version, **kwargs)
        status, _, body = self.put(publish_metadata(name, version, deps), crate)
        self.assertEqual(status, 200, body)
        return crate

    def raw(self, request, body=b"", read_timeout=10):
        with socket.create_connection(("127.0.0.1", self.running.httpd.server_address[1]), timeout=read_timeout) as sock:
            sock.sendall(request)
            if body:
                sock.sendall(body)
            chunks = []
            while True:
                try:
                    chunk = sock.recv(65536)
                except OSError:
                    break
                if not chunk:
                    break
                chunks.append(chunk)
        return b"".join(chunks)

    def put_head(self, length, extra=""):
        return (
            f"PUT /api/v1/crates/new HTTP/1.1\r\nHost: x\r\nAuthorization: {TOKEN}\r\nContent-Length: {length}\r\n{extra}\r\n"
        ).encode()

    def store_blobs(self):
        return sorted(Store(self.state).blobs_dir.glob("*.crate"))


class PublishApiTest(ServerCase):
    def test_a_published_crate_is_served_through_the_sparse_index_and_blobs(self):
        crate = self.publish("alpha")
        status, headers, body = http("GET", f"{self.url}/current/registry/al/ph/alpha")
        self.assertEqual(status, 200)
        entry = json.loads(body.decode().strip())
        self.assertEqual((entry["name"], entry["vers"], entry["cksum"]), ("alpha", "1.0.0", sha256(crate)))
        status, headers, blob = http("GET", f"{self.url}/blobs/{sha256(crate)}.crate")
        self.assertEqual((status, blob), (200, crate))
        self.assertEqual(headers["Cache-Control"], "public, max-age=31536000, immutable")
        self.assertEqual(headers["ETag"], f'"{sha256(crate)}"')
        self.assertEqual(http("GET", f"{self.url}/blobs/{sha256(crate)}.crate", headers={"If-None-Match": headers["ETag"]})[0], 304)
        status, headers, body = http("HEAD", f"{self.url}/blobs/{sha256(crate)}.crate")
        self.assertEqual((status, body, headers["Content-Length"]), (200, b"", str(len(crate))))
        config = json.loads(http("GET", f"{self.url}/current/registry/config.json")[2])
        self.assertEqual(config["dl"], f"{self.url}/blobs/{{sha256-checksum}}.crate")

    def test_a_native_dependency_on_a_published_crate_is_accepted(self):
        self.publish("alpha")
        crate = make_crate("beta", "1.0.0", '\n[dependencies]\nalpha = { version = "1.0", registry = "mycrates" }\n')
        metadata = publish_metadata("beta", "1.0.0", [dependency("alpha", "^1.0")])
        self.assertEqual(self.put(metadata, crate)[0], 200)

    def test_the_index_changes_etag_when_a_new_version_lands(self):
        self.publish("alpha", "1.0.0")
        _, headers, _ = http("GET", f"{self.url}/current/registry/al/ph/alpha")
        self.publish("alpha", "1.1.0")
        status, fresh, body = http("GET", f"{self.url}/current/registry/al/ph/alpha", headers={"If-None-Match": headers["ETag"]})
        self.assertEqual(status, 200)
        self.assertNotEqual(fresh["ETag"], headers["ETag"])
        self.assertEqual(len(body.decode().strip().splitlines()), 2)

    def test_republishing_identical_bytes_is_idempotent_and_different_bytes_conflict(self):
        crate = self.publish("alpha")
        self.assertEqual(self.put(publish_metadata("alpha", "1.0.0"), crate)[0], 200)
        changed = make_crate("alpha", "1.0.0", files={"extra": b"different"})
        status, _, body = self.put(publish_metadata("alpha", "1.0.0"), changed)
        self.assertEqual(status, 409)
        self.assertIn("conflict:", json.loads(body)["errors"][0]["detail"])
        self.assertEqual(len(self.store_blobs()), 2)

    def test_a_name_that_collides_with_an_existing_crate_conflicts(self):
        self.publish("Foo_bar")
        status, _, body = self.put(publish_metadata("foo-bar", "1.0.0"), make_crate("foo-bar", "1.0.0"))
        self.assertEqual(status, 409)
        self.assertIn("name collision", body.decode())

    def test_a_rejected_manifest_gets_a_json_error_and_leaves_no_blob_or_upload(self):
        crate = make_crate("alpha", "1.0.0")
        status, headers, body = self.put(publish_metadata("alpha", "1.0.0", [dependency("serde")]), crate)
        self.assertEqual(status, 422)
        self.assertEqual(headers["Content-Type"], "application/json")
        self.assertIn("dependency count mismatch", json.loads(body)["errors"][0]["detail"])
        self.assertEqual(self.store_blobs(), [])
        self.assertEqual([p.name for p in Store(self.state).uploads_dir.iterdir()], [])
        self.assertEqual(Store(self.state).load_native_records(), [])

    def test_path_dependencies_in_the_archive_cannot_be_published(self):
        crate = make_crate("alpha", "1.0.0", '\n[dependencies]\nlocal = { path = "../local", version = "1" }\n')
        status, _, body = self.put(publish_metadata("alpha", "1.0.0", [dependency("local", "^1")]), crate)
        self.assertEqual(status, 422)
        self.assertIn("git/path", body.decode())

    def test_malformed_uploads_get_precise_statuses_and_leave_no_residue(self):
        crate = make_crate("alpha", "1.0.0")
        good = wire_payload(publish_metadata("alpha", "1.0.0"), crate)
        meta = json.dumps(publish_metadata("alpha", "1.0.0")).encode()
        cases = {
            "empty json": (struct.pack("<II", 0, 10) + bytes(10), 400, "empty json"),
            "short payload": (b"1234", 400, "too short"),
            "json longer than body": (struct.pack("<I", 9999) + b"x" * 20, 413, "json metadata"),
            "invalid json": (struct.pack("<I", 5) + b"{nope" + struct.pack("<I", 3) + b"abc", 400, "invalid publish json"),
            "metadata of the wrong shape": (
                struct.pack("<I", 2) + b"[]" + struct.pack("<I", 3) + b"abc", 400, "invalid publish json"),
            "empty crate": (struct.pack("<I", len(meta)) + meta + struct.pack("<I", 0), 400, "empty crate"),
            "crate length lies": (good[:-1], 400, "does not match"),
            "not gzip": (wire_payload(publish_metadata("alpha", "1.0.0"), b"not-a-valid-gzip-stream"), 422, "integrity"),
            "other crate inside": (wire_payload(publish_metadata("alpha", "1.0.0"), make_crate("real-name", "1.0.0")), 422, "wrong prefix"),
            "version mismatch": (wire_payload(publish_metadata("alpha", "2.0.0"), crate), 422, "wrong prefix"),
            "invalid name": (wire_payload(publish_metadata("1bad", "1.0.0"), make_crate("1bad", "1.0.0")), 400, "crate name"),
        }
        for label, (payload, status, fragment) in cases.items():
            with self.subTest(label):
                code, _, body = http("PUT", f"{self.url}/api/v1/crates/new", payload, AUTH)
                self.assertEqual(code, status, body)
                self.assertIn(fragment, body.decode())
        self.assertEqual(self.store_blobs(), [])
        self.assertEqual([p.name for p in Store(self.state).uploads_dir.iterdir()], [])

    def test_size_limits_reject_before_reading_the_body(self):
        self.running.httpd.config.limits.max_compressed = 100
        response = self.raw(self.put_head(100 + 11 * 1024 * 1024))
        self.assertTrue(response.startswith(b"HTTP/1.1 413"), response[:80])
        crate = make_crate("alpha", "1.0.0", files={"pad": b"x" * 5000})
        status, _, body = http("PUT", f"{self.url}/api/v1/crates/new", wire_payload(publish_metadata("alpha", "1.0.0"), crate), AUTH)
        self.assertIn(status, (413,), body)

    def test_framing_requirements(self):
        no_length = b"PUT /api/v1/crates/new HTTP/1.1\r\nHost: x\r\nAuthorization: " + TOKEN.encode() + b"\r\n\r\n"
        self.assertTrue(self.raw(no_length).startswith(b"HTTP/1.1 411"))
        chunked = self.put_head(5, "Transfer-Encoding: chunked\r\n")
        self.assertTrue(self.raw(chunked, b"5\r\nhello\r\n0\r\n\r\n").startswith(b"HTTP/1.1 411"))
        self.assertTrue(self.raw(b"BREW /pot HTTP/1.1\r\nHost: x\r\n\r\n").startswith(b"HTTP/1.1 401"))
        unknown = self.raw(b"POST /anything HTTP/1.1\r\nHost: x\r\nAuthorization: " + TOKEN.encode() + b"\r\n\r\n")
        self.assertTrue(unknown.startswith(b"HTTP/1.1 404"))
        self.assertTrue(self.raw(b"garbage\r\n\r\n").startswith(b"HTTP/1.1 400"))

    def test_concurrent_writers_publish_every_version_into_one_index(self):
        def upload(version):
            crate = make_crate("alpha", version)
            return http("PUT", f"{self.url}/api/v1/crates/new", wire_payload(publish_metadata("alpha", version), crate), AUTH)[0]

        with concurrent.futures.ThreadPoolExecutor(8) as pool:
            statuses = list(pool.map(upload, [f"1.0.{i}" for i in range(8)]))
        self.assertEqual(statuses.count(200) + statuses.count(503), 8)
        accepted = statuses.count(200)
        lines = (self.site / "current/registry/al/ph/alpha").read_text().strip().splitlines()
        self.assertEqual(len(lines), accepted)
        Store(self.state).verify()


class AuthTest(ServerCase):
    def test_writes_need_the_token_in_either_header_form(self):
        crate = make_crate("alpha", "1.0.0")
        payload = wire_payload(publish_metadata("alpha", "1.0.0"), crate)
        for headers in [{}, {"Authorization": "wrong"}, {"Authorization": "Bearer wrong"}, {"Authorization": ""}]:
            with self.subTest(headers=headers):
                status, _, body = http("PUT", f"{self.url}/api/v1/crates/new", payload, headers)
                self.assertEqual((status, json.loads(body)["errors"][0]["detail"]), (401, "unauthorized"))
        self.assertEqual(http("PUT", f"{self.url}/api/v1/crates/new", payload, {"Authorization": f"Bearer {TOKEN}"})[0], 200)
        self.assertEqual(self.store_blobs() != [], True)

    def test_unauthenticated_writes_leave_state_untouched(self):
        crate = make_crate("alpha", "1.0.0")
        http("PUT", f"{self.url}/api/v1/crates/new", wire_payload(publish_metadata("alpha", "1.0.0"), crate))
        self.assertEqual(self.store_blobs(), [])

    def test_reads_are_open_when_auth_is_not_required(self):
        self.assertEqual(http("GET", f"{self.url}/current/registry/config.json")[0], 200)
        config = json.loads(http("GET", f"{self.url}/current/registry/config.json")[2])
        self.assertFalse(config["auth-required"])

    def test_yank_and_unyank_need_the_token_even_when_reads_are_open(self):
        self.publish("alpha")
        for method, action in (("DELETE", "yank"), ("PUT", "unyank")):
            status, _, _ = http(method, f"{self.url}/api/v1/crates/alpha/1.0.0/{action}")
            self.assertEqual(status, 401)
        self.assertFalse(json.loads((self.site / "current/registry/al/ph/alpha").read_text())["yanked"])


class ReadAuthTest(ServerCase):
    auth_required = True

    def test_reads_need_the_token_and_the_config_advertises_it(self):
        crate = self.publish("alpha")
        for path in ["/current/registry/config.json", "/current/registry/al/ph/alpha", f"/blobs/{sha256(crate)}.crate"]:
            with self.subTest(path=path):
                self.assertEqual(http("GET", self.url + path)[0], 401)
                self.assertEqual(http("GET", self.url + path, headers=AUTH)[0], 200)
        config = json.loads(http("GET", f"{self.url}/current/registry/config.json", headers=AUTH)[2])
        self.assertTrue(config["auth-required"])


class YankTest(ServerCase):
    def test_yank_and_unyank_update_index_and_catalog(self):
        self.publish("alpha")
        index = self.site / "current/registry/al/ph/alpha"
        status, _, body = http("DELETE", f"{self.url}/api/v1/crates/alpha/1.0.0/yank", headers=AUTH)
        self.assertEqual((status, json.loads(body)), (200, {"ok": True}))
        self.assertTrue(json.loads(index.read_text())["yanked"])
        catalog = json.loads((self.site / "current/catalog.json").read_text())
        self.assertTrue(catalog["packages"][0]["versions"][0]["yanked"])
        self.assertEqual(http("PUT", f"{self.url}/api/v1/crates/alpha/1.0.0/unyank", headers=AUTH)[0], 200)
        self.assertFalse(json.loads(index.read_text())["yanked"])

    def test_unknown_crates_versions_and_routes_are_404(self):
        self.publish("alpha")
        for method, path in [
            ("DELETE", "/api/v1/crates/alpha/9.9.9/yank"),
            ("DELETE", "/api/v1/crates/missing/1.0.0/yank"),
            ("PUT", "/api/v1/crates/alpha/1.0.0/yank"),
            ("DELETE", "/api/v1/crates/alpha/1.0.0/unyank"),
            ("DELETE", "/api/v1/crates/alpha/yank"),
        ]:
            with self.subTest(path=path):
                self.assertEqual(http(method, self.url + path, headers=AUTH)[0], 404)


class StaticFilesTest(ServerCase):
    def test_only_the_published_tree_is_served(self):
        crate = self.publish("alpha")
        generation = (self.site / "current").resolve().name
        for path, expected in [
            ("/current/catalog.json", 200),
            ("/current/index.html", 200),
            ("/current/llms.txt", 200),
            (f"/generations/{generation}/registry/config.json", 200),
            ("/current/crates-io/config.json", 200),
            ("/current/registry/missing", 404),
            ("/current/registry/", 404),
            ("/current/registry", 404),
            ("/", 404),
            ("/blobs/", 404),
            ("/blobs/nothex.crate", 404),
            (f"/blobs/{'0' * 64}.crate", 404),
            (f"/blobs/{sha256(crate)}.crate/extra", 404),
            ("/current/../state/config.json", 404),
            ("/%2e%2e/state/config.json", 404),
            ("/current/%2e%2e/%2e%2e/state/config.json", 404),
            ("/.publish.lock", 404),
            ("/state/config.json", 404),
        ]:
            with self.subTest(path=path):
                self.assertEqual(http("GET", self.url + path)[0], expected)

    def test_content_types(self):
        self.publish("alpha")
        self.assertEqual(http("GET", f"{self.url}/current/catalog.json")[1]["Content-Type"], "application/json")
        self.assertTrue(http("GET", f"{self.url}/current/index.html")[1]["Content-Type"].startswith("text/html"))
        self.assertTrue(http("GET", f"{self.url}/current/registry/al/ph/alpha")[1]["Content-Type"].startswith("text/plain"))

    def test_a_symlink_that_escapes_the_site_is_not_followed(self):
        secret = self.tmp / "secret.txt"
        secret.write_text("secret")
        (self.site / "generations").mkdir(exist_ok=True)
        (self.site / "generations" / "leak").symlink_to(secret)
        self.assertEqual(http("GET", f"{self.url}/generations/leak")[0], 404)


class HardeningTest(ServerCase):
    overrides = {"header_timeout": 0.5, "body_timeout": 1.0}

    def test_a_header_that_trickles_past_the_deadline_times_out(self):
        with socket.create_connection(("127.0.0.1", self.running.httpd.server_address[1]), timeout=10) as sock:
            for byte in b"GET /current/registry/config.json HTTP/1.1\r\nX-Slow: ":
                try:
                    sock.sendall(bytes([byte]))
                except OSError:
                    break
                time.sleep(0.05)
            data = sock.recv(4096)
        self.assertTrue(data.startswith(b"HTTP/1.1 408"), data[:60])

    def test_a_body_that_trickles_past_the_deadline_times_out_and_leaves_no_residue(self):
        crate = make_crate("alpha", "1.0.0")
        payload = wire_payload(publish_metadata("alpha", "1.0.0"), crate)
        with socket.create_connection(("127.0.0.1", self.running.httpd.server_address[1]), timeout=10) as sock:
            sock.sendall(self.put_head(len(payload)))
            sent = 0
            try:
                while sent < len(payload):
                    sock.sendall(payload[sent : sent + 20])
                    sent += 20
                    time.sleep(0.1)
            except OSError:
                pass
            data = b""
            try:
                data = sock.recv(4096)
            except OSError:
                pass
        self.assertTrue(data.startswith(b"HTTP/1.1 408") or data == b"", data[:60])
        self.assertEqual(self.store_blobs(), [])
        self.assertEqual([p.name for p in Store(self.state).uploads_dir.iterdir()], [])

    def test_a_body_sent_after_the_header_deadline_but_inside_the_body_deadline_is_accepted(self):
        crate = make_crate("alpha", "1.0.0")
        payload = wire_payload(publish_metadata("alpha", "1.0.0"), crate)
        with socket.create_connection(("127.0.0.1", self.running.httpd.server_address[1]), timeout=10) as sock:
            sock.sendall(self.put_head(len(payload)))
            time.sleep(0.7)
            sock.sendall(payload)
            data = sock.recv(4096)
        self.assertTrue(data.startswith(b"HTTP/1.1 200"), data[:60])

    def test_an_overlong_request_line_is_rejected(self):
        self.assertTrue(self.raw(b"GET /" + b"a" * 70000 + b" HTTP/1.1\r\n\r\n").startswith(b"HTTP/1.1 400"))


class AdmissionTest(ServerCase):
    overrides = {"header_timeout": 5}

    def setUp(self):
        super().setUp()
        self.running.httpd.writes.limit = 2
        self.running.httpd.connections.limit = 6

    def idle(self, count):
        sockets = [socket.create_connection(("127.0.0.1", self.running.httpd.server_address[1])) for _ in range(count)]
        self.addCleanup(lambda: [s.close() for s in sockets])
        time.sleep(0.2)
        return sockets

    def test_idle_connections_do_not_block_authenticated_writes(self):
        self.idle(4)
        self.publish("alpha")

    def test_unauthenticated_requests_do_not_take_write_slots(self):
        crate = make_crate("alpha", "1.0.0")
        payload = wire_payload(publish_metadata("alpha", "1.0.0"), crate)
        for _ in range(10):
            self.assertEqual(http("PUT", f"{self.url}/api/v1/crates/new", payload)[0], 401)
        self.assertEqual(self.running.httpd.writes.active, 0)
        self.assertEqual(http("PUT", f"{self.url}/api/v1/crates/new", payload, AUTH)[0], 200)

    def test_connections_beyond_the_cap_are_turned_away_with_503(self):
        self.idle(6)
        status, _, body = http("GET", f"{self.url}/current/registry/config.json")
        self.assertEqual(status, 503)
        self.assertIn("too many concurrent connections", body.decode())

    def test_writes_beyond_the_slot_count_are_turned_away_with_503(self):
        crate = make_crate("alpha", "1.0.0")
        payload = wire_payload(publish_metadata("alpha", "1.0.0"), crate)
        stalled = []
        for _ in range(2):
            sock = socket.create_connection(("127.0.0.1", self.running.httpd.server_address[1]))
            sock.sendall(self.put_head(len(payload)))
            stalled.append(sock)
            self.addCleanup(sock.close)
        time.sleep(0.3)
        status, _, body = http("PUT", f"{self.url}/api/v1/crates/new", payload, AUTH)
        self.assertEqual(status, 503)
        self.assertIn("too many concurrent writes", body.decode())


class StartupTest(ScratchTestCase):
    def config(self, **changes):
        state, site = init_state(self.tmp)
        settings = dict(
            bind_host="127.0.0.1", bind_port=0, token="t", auth_required=False, state_dir=state, site_dir=site,
            public_base_url="http://127.0.0.1:1", limits=Config.load(state / "config.json").limits,
        )
        settings.update(changes)
        return ServeConfig(**settings)

    def test_only_loopback_addresses_can_be_bound(self):
        for host in ["0.0.0.0", "192.0.2.1", "example.com", ""]:
            with self.subTest(host=host), self.assertRaisesRegex(InputError, "loopback"):
                PublishServer(self.config(bind_host=host))
        for bind in ["0.0.0.0:80", "[::]:80", "8080", "127.0.0.1:port"]:
            with self.subTest(bind=bind), self.assertRaises(InputError):
                split_bind(bind)
        self.assertEqual(split_bind("127.0.0.1:8080"), ("127.0.0.1", 8080))
        self.assertEqual(split_bind("[::1]:9"), ("::1", 9))

    def test_serving_without_a_token_is_refused(self):
        with self.assertRaisesRegex(InputError, "without a token"):
            PublishServer(self.config(token=""))

    def test_the_cli_refuses_a_missing_token_and_a_world_readable_token_file(self):
        state, site = init_state(self.tmp)
        base = ["serve-publish", "--bind", "127.0.0.1:0", "--state-dir", str(state), "--site-dir", str(site)]
        code, _, err = cli(*base)
        self.assertEqual(code, 2)
        self.assertIn("without a token", err)
        token = self.tmp / "token"
        token.write_text("secret\n")
        token.chmod(0o644)
        code, _, err = cli(*base, "--token-file", token)
        self.assertEqual(code, 2)
        self.assertIn("0600", err)
        code, _, err = cli(*base, "--token-file", self.tmp / "absent")
        self.assertEqual(code, 2)
        self.assertIn("not found", err)
        code, _, err = cli("serve-publish", "--bind", "0.0.0.0:1", "--state-dir", state)
        self.assertEqual(code, 2)

    def test_the_cli_server_serves_a_token_file_secret(self):
        state, site = init_state(self.tmp)
        token = self.tmp / "token"
        token.write_text("from-file\n")
        token.chmod(0o600)
        lines = []
        ready = threading.Event()
        import contextlib
        import io

        class Capture(io.StringIO):
            def write(inner, text):
                lines.append(text)
                if "listening on" in text:
                    ready.set()
                return len(text)

        def run():
            with contextlib.redirect_stdout(Capture()):
                cli_main(["serve-publish", "--bind", "127.0.0.1:0", "--token-file", str(token), "--state-dir", str(state), "--site-dir", str(site)])

        threading.Thread(target=run, daemon=True).start()
        self.assertTrue(ready.wait(10))
        address = next(t for t in lines if "listening on" in t).split("listening on ")[1].strip()
        crate = make_crate("alpha", "1.0.0")
        payload = wire_payload(publish_metadata("alpha", "1.0.0"), crate)
        self.assertEqual(http("PUT", f"http://{address}/api/v1/crates/new", payload, {"Authorization": "secret"})[0], 401)
        self.assertEqual(http("PUT", f"http://{address}/api/v1/crates/new", payload, {"Authorization": "from-file"})[0], 200)


if __name__ == "__main__":
    unittest.main()
