from __future__ import annotations

import hashlib
import shutil
import socket
import subprocess
import time
import zipfile
from base64 import urlsafe_b64encode
from pathlib import Path
from urllib.request import urlopen

import pytest
from packaging.version import Version

import mypypi.store as store
from mypypi.models import Target, ValidatedArtifact, WheelIdentity
from mypypi.validate import _local_platform

ROOT = Path(__file__).resolve().parents[4]
SERVER_BIN = ROOT / "target" / "debug" / "stuff-server"

HOST_PLATFORM = _local_platform()
assert HOST_PLATFORM is not None


def _artifact(tmp_path: Path) -> ValidatedArtifact:
    filename = "candidate_fixture-1.0-py3-none-any.whl"
    identity = WheelIdentity.parse(filename)
    dist_info = f"{identity.package.name.replace('-', '_')}-{identity.package.version}.dist-info"
    path = tmp_path / filename
    members = {
        f"{dist_info}/METADATA": (
            "Metadata-Version: 2.4\n"
            f"Name: {identity.package.name}\n"
            f"Version: {identity.package.version}\n"
            "Requires-Python: >=3.15\n\n"
        ).encode(),
        f"{dist_info}/WHEEL": b"Wheel-Version: 1.0\nTag: py3-none-any\n\n",
        "candidate_fixture.py": b"value = 1\n",
    }
    record = []
    for name, content in members.items():
        digest = urlsafe_b64encode(hashlib.sha256(content).digest()).decode().rstrip("=")
        record.append(f"{name},sha256={digest},{len(content)}")
    record.append(f"{dist_info}/RECORD,,")
    members[f"{dist_info}/RECORD"] = "\n".join(record).encode() + b"\n"
    with zipfile.ZipFile(path, "w") as archive:
        for name, content in members.items():
            archive.writestr(name, content)
    return ValidatedArtifact(
        identity,
        path,
        hashlib.sha256(path.read_bytes()).hexdigest(),
        frozenset({Target(Version("3.15"), HOST_PLATFORM)}),
    )


def _free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


@pytest.mark.network
def test_uv_installs_from_stuff_server(tmp_path: Path) -> None:
    if shutil.which("cargo") is None:
        pytest.skip("cargo is required to build stuff-server")
    if shutil.which("uv") is None:
        pytest.skip("uv is required to install from the index")

    build = subprocess.run(
        ["cargo", "build", "--bin", "stuff-server"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        timeout=600,
    )
    assert build.returncode == 0, build.stderr
    assert SERVER_BIN.exists()

    output = tmp_path / "index"
    store.publish((_artifact(tmp_path),), output)
    assert (output / "current").is_symlink()

    port = _free_port()
    server = subprocess.Popen(
        [str(SERVER_BIN), "--bind", f"127.0.0.1:{port}", "--route", f"/={output}"],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        assert server.stdout is not None
        banner = server.stdout.readline()
        assert banner.startswith("listening on "), banner
        for _ in range(50):
            try:
                with urlopen(f"http://127.0.0.1:{port}/current/simple/") as response:
                    assert response.status == 200
                    break
            except OSError:
                time.sleep(0.1)
        else:
            pytest.fail("stuff-server did not become ready")

        with urlopen(f"http://127.0.0.1:{port}/current/simple/candidate-fixture/") as response:
            assert response.status == 200
            assert "text/html" in (response.headers.get("Content-Type") or "")

        venv = tmp_path / ".consumer"
        uv_venv = subprocess.run(
            ["uv", "venv", "--python", "3.15", str(venv)],
            capture_output=True,
            text=True,
            timeout=300,
        )
        assert uv_venv.returncode == 0, uv_venv.stderr
        install = subprocess.run(
            [
                "uv",
                "pip",
                "install",
                "--python",
                str(venv / "bin" / "python"),
                "--only-binary=:all:",
                "--index-url",
                f"http://127.0.0.1:{port}/current/simple",
                "--index-strategy",
                "first-index",
                "candidate-fixture==1.0",
            ],
            capture_output=True,
            text=True,
            timeout=300,
        )
        assert install.returncode == 0, install.stderr
    finally:
        server.terminate()
        server.wait(timeout=10)
