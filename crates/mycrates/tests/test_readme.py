import pytest
import os
import shutil
import subprocess
import textwrap

from mycrates.config import Config

from .support import PACKAGE_ROOT, FakeUpstream, ScratchTestCase, start_server

FENCE = "```sh verified"


def verified_blocks(markdown):
    blocks, current = [], None
    for line in markdown.splitlines():
        if line.strip() == FENCE:
            current = []
        elif current is not None and line.strip() == "```":
            blocks.append(current)
            current = None
        elif current is not None:
            current.append(line.strip())
    return blocks


@pytest.mark.skipif(not (shutil.which("cargo")), reason="validate shells out to cargo")
class ReadmeTest(ScratchTestCase):
    def test_every_verified_command_in_the_readme_runs(self):
        blocks = verified_blocks((PACKAGE_ROOT / "README.md").read_text())
        self.assertTrue(blocks, "the README has no `sh verified` blocks")

        upstream = FakeUpstream()
        self.addCleanup(upstream.close)
        checksum = upstream.add("sample-pkg", "0.1.0")
        project = self.tmp / "project"
        (project / "src").mkdir(parents=True)
        (project / "src" / "lib.rs").write_text("")
        (project / "Cargo.toml").write_text(
            '[package]\nname = "validation-probe"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\nsample-pkg = "0.1.0"\n'
        )
        (project / "Cargo.lock").write_text(
            textwrap.dedent(
                f"""\
                version = 3

                [[package]]
                name = "sample-pkg"
                version = "0.1.0"
                source = "registry+https://github.com/rust-lang/crates.io-index"
                checksum = "{checksum}"

                [[package]]
                name = "validation-probe"
                version = "0.1.0"
                dependencies = [
                 "sample-pkg",
                ]
                """
            )
        )
        shim_dir = self.tmp / "bin"
        shim_dir.mkdir()
        (shim_dir / "mycrates").symlink_to(PACKAGE_ROOT / "bin" / "mycrates")
        environment = dict(os.environ, PATH=f"{shim_dir}:{os.environ['PATH']}")
        for key in [k for k in environment if k.startswith("CARGO_HTTP") or k == "RUSTC_WRAPPER"]:
            del environment[key]
        environment.update(CARGO_TARGET_DIR=str(self.tmp / "target"))

        server = None
        for block in blocks:
            for command in block:
                if not command or command.startswith("#"):
                    continue
                result = subprocess.run(
                    ["sh", "-c", command], cwd=project, env=environment, capture_output=True, text=True
                )
                self.assertEqual(result.returncode, 0, f"`{command}`\n{result.stdout}\n{result.stderr}")
                if command == "mycrates init":
                    state, site = project / ".work/state", project / ".work/site"
                    server = start_server(state, site, public_base_url="local")
                    self.addCleanup(server.close)
                    config = Config.load(state / "config.json")
                    config.upstream.index_url = upstream.url
                    config.public_base_url = server.url
                    config.save(state / "config.json")
        self.assertIsNotNone(server, "the README never runs `mycrates init`")


