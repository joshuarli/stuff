import base64
import hashlib
import os
import platform
import shlex
import subprocess
import sys
import sysconfig
import tempfile
import zipfile
from pathlib import Path

NAME = "native-probe"
VERSION = "0.1.0"
DIST_INFO = f"native_probe-{VERSION}.dist-info"
MODULES = (
    ("native_probe", "src/native_probe.c", "CC", "cc"),
    ("native_probe_cpp", "src/native_probe_cpp.cpp", "CXX", "c++"),
)


def extension_platform_tag():
    if sys.platform == "darwin":
        deployment = os.environ.get("MACOSX_DEPLOYMENT_TARGET", "15.0")
        major, minor = deployment.split(".")[:2]
        return f"macosx_{major}_{minor}_{platform.machine()}"
    return f"linux_{platform.machine()}"


def compile_module(name, source, env_name, default_compiler, output):
    compiler = shlex.split(os.environ.get(env_name, default_compiler))
    command = compiler + ["-shared", "-fPIC", "-O2"]
    if sys.platform == "darwin":
        deployment = os.environ.get("MACOSX_DEPLOYMENT_TARGET")
        command += ["-undefined", "dynamic_lookup"]
        if deployment:
            command += [f"-mmacosx-version-min={deployment}"]
    command += [
        f"-I{sysconfig.get_paths()['include']}",
        source,
        "-o",
        str(output / f"{name}{sysconfig.get_config_var('EXT_SUFFIX')}"),
    ]
    subprocess.run(command, check=True)


def record_row(name, data):
    digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()
    return f"{name},sha256={digest},{len(data)}"


def build_wheel(wheel_directory, config_settings=None, metadata_directory=None):
    platform_tag = extension_platform_tag()
    wheel_name = f"native_probe-{VERSION}-cp315-cp315-{platform_tag}.whl"
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        for name, source, env_name, default_compiler in MODULES:
            compile_module(name, source, env_name, default_compiler, root)
        files = {
            f"{DIST_INFO}/METADATA": (
                f"Metadata-Version: 2.1\nName: {NAME}\nVersion: {VERSION}\n"
                "Requires-Python: >=3.15\n"
            ).encode(),
            f"{DIST_INFO}/WHEEL": (
                "Wheel-Version: 1.0\n"
                "Generator: native-probe\n"
                "Root-Is-Purelib: false\n"
                f"Tag: cp315-cp315-{platform_tag}\n"
            ).encode(),
        }
        for path in sorted(root.iterdir()):
            files[path.name] = path.read_bytes()
        rows = [record_row(name, data) for name, data in sorted(files.items())]
        rows.append(f"{DIST_INFO}/RECORD,,")
        files[f"{DIST_INFO}/RECORD"] = ("\n".join(rows) + "\n").encode()
        destination = Path(wheel_directory) / wheel_name
        with zipfile.ZipFile(destination, "w", zipfile.ZIP_DEFLATED) as wheel:
            for name, data in files.items():
                wheel.writestr(name, data)
    return wheel_name
