from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import tempfile
from pathlib import Path

from repomod.rules.musl_toolchain import (
    image_build_arguments,
    load_musl_toolchain,
    validate_image_definition,
)

_ROOT = Path(__file__).resolve().parents[3]
_DIGEST = re.compile(r"sha256:[0-9a-f]{64}\Z")


def _dockerfile_path() -> Path:
    return _ROOT / "docker" / "musl" / "Dockerfile"


def _toolchain_path() -> Path:
    return _ROOT / "docker" / "musl" / "rust-toolchain.toml"


def _scratch_directory() -> Path:
    configured = os.environ.get("TMPDIR")
    root = (
        Path(configured)
        if configured is not None
        else _ROOT / ".work" / "tmp"
    )
    root.mkdir(parents=True, exist_ok=True)
    if root.is_symlink() or not root.is_dir():
        raise ValueError(f"musl build scratch path is not a regular directory: {root}")
    return root


def _pinned_source(path: Path) -> str:
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"canonical image source is missing or not a regular file: {path}")
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        raise ValueError(f"cannot read canonical image source {path}: {error}") from error


def _layout_digest(layout: Path, platform: str) -> str:
    path = layout / "index.json"
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"build did not write an OCI index at {path}")
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read OCI index at {path}: {error}") from error
    manifests = document.get("manifests") if isinstance(document, dict) else None
    if not isinstance(manifests, list):
        raise ValueError(f"OCI index at {path} has no manifests")
    architecture = platform.rsplit("/", 1)[-1]
    digests = [
        entry.get("digest")
        for entry in manifests
        if isinstance(entry, dict)
        and isinstance(entry.get("digest"), str)
        and (
            not isinstance(entry.get("platform"), dict)
            or entry["platform"].get("architecture") == architecture
        )
    ]
    if not digests or not isinstance(digests[0], str) or _DIGEST.fullmatch(digests[0]) is None:
        raise ValueError(f"OCI index at {path} has no manifest digest for {platform}")
    return digests[0]


def _replace_output(layout: Path, output: Path) -> None:
    if output.is_symlink():
        raise ValueError(f"output path is a symlink: {output}")
    if output.exists():
        if output.is_dir():
            shutil.rmtree(output)
        else:
            output.unlink()
    output.parent.mkdir(parents=True, exist_ok=True)
    os.replace(layout, output)


def _musl(args: argparse.Namespace) -> int:
    if getattr(args, "operation", None) != "build":
        raise ValueError("musl requires a build operation")
    output = Path(args.output)
    lock = load_musl_toolchain()
    dockerfile = _pinned_source(_dockerfile_path())
    toolchain = _pinned_source(_toolchain_path())
    validate_image_definition(lock, dockerfile, toolchain)
    executable = shutil.which("docker")
    if executable is None:
        raise ValueError("docker is required to build the canonical musl image")
    with tempfile.TemporaryDirectory(prefix="musl-build-", dir=_scratch_directory()) as temporary:
        layout = Path(temporary) / "layout"
        command = [
            executable,
            "buildx",
            "build",
            "--platform",
            args.platform,
            "--output",
            f"type=oci,dest={layout},tar=false",
            "-f",
            str(_dockerfile_path()),
        ]
        for name, value in image_build_arguments(lock).items():
            command.extend(["--build-arg", f"{name}={value}"])
        command.append(str(_ROOT / "docker" / "musl"))
        try:
            result = subprocess.run(
                command,
                capture_output=True,
                text=True,
                timeout=600,
                check=False,
            )
        except subprocess.TimeoutExpired as error:
            raise ValueError("docker build timed out after ten minutes") from error
        except OSError as error:
            raise ValueError(f"cannot run docker build: {error}") from error
        if result.returncode != 0:
            detail = result.stderr.strip() or result.stdout.strip() or f"docker exited {result.returncode}"
            raise ValueError(f"docker build failed: {detail}")
        digest = _layout_digest(layout, args.platform)
        _replace_output(layout, output)
    print(f"{output}: {digest}")
    return 0
