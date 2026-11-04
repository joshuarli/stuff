import json
import os
import shutil
import subprocess
import textwrap
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from mycrates.config import Config
from mycrates.store import Store

from .support import ScratchTestCase, cli, make_crate, sha256, start_server

CARGO = shutil.which("cargo")
MIRRORED = ("mirrordep", "1.0.0")
SOURCES = {
    "leaf": "pub fn answer() -> u32 { 40 }\n",
    "app": "pub fn answer() -> u32 { leaf::answer() + mirrordep::two() }\n",
}


class FakeCratesIo:
    def __init__(self):
        name, version = MIRRORED
        self.crate = make_crate(name, version, files={"src/lib.rs": b"pub fn two() -> u32 { 2 }\n"})
        self.checksum = sha256(self.crate)
        entry = {
            "name": name, "vers": version, "deps": [], "cksum": self.checksum, "features": {}, "yanked": False,
        }
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                if self.path == "/config.json":
                    body = json.dumps({"dl": f"http://127.0.0.1:{outer.port}/dl/{{crate}}/{{version}}"}).encode()
                elif self.path == "/mi/rr/mirrordep":
                    body = (json.dumps(entry, separators=(",", ":")) + "\n").encode()
                elif self.path == f"/dl/{name}/{version}":
                    body = outer.crate
                else:
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
        threading.Thread(target=self.httpd.serve_forever, kwargs={"poll_interval": 0.02}, daemon=True).start()

    def close(self):
        self.httpd.shutdown()
        self.httpd.server_close()


@unittest.skipUnless(CARGO, "cargo is not installed")
class CargoNativeTest(ScratchTestCase):
    def setUp(self):
        super().setUp()
        self.upstream = FakeCratesIo()
        self.addCleanup(self.upstream.close)
        self.state, self.site = self.tmp / "state", self.tmp / "site"
        cli("init", "--state-dir", self.state, "--site-dir", self.site)
        config = Config.load(self.state / "config.json")
        config.upstream.index_url = f"http://127.0.0.1:{self.upstream.port}/"
        config.save(self.state / "config.json")
        lock = self.tmp / "Cargo.lock"
        lock.write_text(
            textwrap.dedent(
                f"""\
                version = 4

                [[package]]
                name = "mirrordep"
                version = "1.0.0"
                source = "registry+https://github.com/rust-lang/crates.io-index"
                checksum = "{self.upstream.checksum}"
                """
            )
        )
        self.assertEqual(cli("import-lock", lock, "--project", "mirror", "--state-dir", self.state)[0], 0)
        self.assertEqual(cli("fetch", "--state-dir", self.state)[0], 0)
        self.running = start_server(self.state, self.site, token="cargo-token", public_base_url="local")
        self.addCleanup(self.running.close)
        self.cargo_home = self.tmp / "cargo-home"
        self.cargo_home.mkdir()
        (self.cargo_home / "config.toml").write_text(
            textwrap.dedent(
                f"""\
                [registries.mycrates]
                index = "sparse+{self.running.url}/current/registry/"

                [source.crates-io]
                replace-with = "mycrates-mirror"

                [source.mycrates-mirror]
                registry = "sparse+{self.running.url}/current/crates-io/"
                """
            )
        )

    def cargo(self, cwd, *args):
        environment = dict(os.environ)
        for key in [k for k in environment if k.startswith("CARGO_") or k == "RUSTC_WRAPPER"]:
            del environment[key]
        environment.update(
            CARGO_HOME=str(self.cargo_home),
            CARGO_REGISTRIES_MYCRATES_TOKEN="cargo-token",
            CARGO_TERM_COLOR="never",
            CARGO_TARGET_DIR=str(self.tmp / "target"),
            CARGO_HTTP_PROXY="http://127.0.0.1:1",
            NO_PROXY="127.0.0.1,localhost",
        )
        result = subprocess.run(["cargo", *args], cwd=cwd, env=environment, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, f"cargo {' '.join(args)}\n{result.stdout}\n{result.stderr}")
        return result

    def project(self, name, version, dependencies=""):
        directory = self.tmp / "src" / name
        (directory / "src").mkdir(parents=True)
        (directory / "Cargo.toml").write_text(
            f'[package]\nname = "{name}"\nversion = "{version}"\nedition = "2021"\n'
            f'publish = ["mycrates"]\ndescription = "{name}"\nlicense = "Apache-2.0"\n\n[dependencies]\n{dependencies}'
        )
        (directory / "src" / "lib.rs").write_text(SOURCES[name])
        return directory

    def test_publish_and_resolve_native_and_mirrored_dependencies(self):
        leaf = self.project("leaf", "0.1.0")
        self.cargo(leaf, "publish", "--registry", "mycrates")

        app = self.project(
            "app", "0.2.0", 'leaf = { version = "0.1", registry = "mycrates" }\nmirrordep = "1"\n'
        )
        self.cargo(app, "publish", "--registry", "mycrates")

        app_line = json.loads((self.site / "current/registry/3/a/app").read_text().strip())
        self.assertEqual({d["name"] for d in app_line["deps"]}, {"leaf", "mirrordep"})

        consumer = self.tmp / "consumer"
        (consumer / "src").mkdir(parents=True)
        (consumer / "Cargo.toml").write_text(
            '[package]\nname = "consumer"\nversion = "0.0.1"\nedition = "2021"\n\n'
            '[dependencies]\napp = { version = "0.2", registry = "mycrates" }\n'
        )
        (consumer / "src" / "main.rs").write_text('fn main() { println!("{}", app::answer()); }\n')
        self.cargo(consumer, "build")
        run = subprocess.run([str(self.tmp / "target/debug/consumer")], capture_output=True, text=True)
        self.assertEqual(run.stdout.strip(), "42")
        lock = (consumer / "Cargo.lock").read_text()
        for name in ("app", "leaf", "mirrordep"):
            self.assertIn(f'name = "{name}"', lock)
        Store(self.state).verify()
