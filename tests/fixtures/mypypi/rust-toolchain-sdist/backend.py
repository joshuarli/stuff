import base64
import hashlib
import subprocess
import zipfile
from pathlib import Path

NAME = "rust_toolchain_smoke"
VERSION = "0.1.0"
DIST_INFO = f"{NAME}-{VERSION}.dist-info"


def run(*args):
    done = subprocess.run(args, capture_output=True, text=True)
    if done.returncode != 0:
        raise SystemExit(f"{' '.join(args)} failed: {done.stderr.strip()}")
    return done.stdout.strip()


def build_wheel(wheel_directory, config_settings=None, metadata_directory=None):
    cargo = run("cargo", "--version")
    rustc = run("rustc", "--version")
    files = {
        f"{NAME}/__init__.py": f"CARGO = {cargo!r}\nRUSTC = {rustc!r}\n",
        f"{DIST_INFO}/METADATA": (
            f"Metadata-Version: 2.1\nName: {NAME}\nVersion: {VERSION}\n"
        ),
        f"{DIST_INFO}/WHEEL": (
            "Wheel-Version: 1.0\n"
            "Generator: rust-toolchain-smoke\n"
            "Root-Is-Purelib: true\n"
            "Tag: py3-none-any\n"
        ),
    }
    records = []
    path = Path(wheel_directory) / f"{NAME}-{VERSION}-py3-none-any.whl"
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as wheel:
        for name, text in files.items():
            data = text.encode()
            wheel.writestr(name, data)
            digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
            records.append(f"{name},sha256={digest},{len(data)}")
        records.append(f"{DIST_INFO}/RECORD,,")
        wheel.writestr(f"{DIST_INFO}/RECORD", "\n".join(records) + "\n")
    return path.name
