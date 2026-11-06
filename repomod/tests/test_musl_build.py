from __future__ import annotations

import argparse
import json
import shutil
import subprocess
from dataclasses import replace
from pathlib import Path

import pytest

from repomod.cli import _parser
from repomod.cmd import musl as musl_cmd
from repomod.rules import musl_toolchain as musl

ROOT = Path(__file__).resolve().parents[1]
DIGEST = "sha256:" + "9" * 64


def _args(platform: str = "linux/amd64", output: Path | None = None) -> argparse.Namespace:
    return argparse.Namespace(
        operation="build",
        platform=platform,
        output=output if output is not None else Path(".work/oci/musl"),
    )


def _workspace(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    root = tmp_path / "repo"
    shutil.copytree(ROOT / "docker", root / "docker")
    monkeypatch.setattr(musl_cmd, "_ROOT", root)
    scratch = tmp_path / "scratch"
    scratch.mkdir()
    monkeypatch.setattr(musl_cmd, "_scratch_directory", lambda: scratch)
    monkeypatch.setattr(musl_cmd.shutil, "which", lambda name: "/usr/bin/docker")
    return root


def _fake_run(
    monkeypatch: pytest.MonkeyPatch,
    *,
    returncode: int = 0,
    digest: str = DIGEST,
    platform: str = "linux/amd64",
    write: bool = True,
) -> list[list[str]]:
    calls: list[list[str]] = []

    def run(command: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
        calls.append(command)
        if write:
            output = next(arg for arg in command if arg.startswith("type=oci,dest="))
            layout = Path(output.removeprefix("type=oci,dest=").removesuffix(",tar=false"))
            layout.mkdir(parents=True)
            index = {
                "schemaVersion": 2,
                "manifests": [
                    {
                        "mediaType": "application/vnd.oci.image.manifest.v1+json",
                        "digest": digest,
                        "size": 1,
                        "platform": {
                            "architecture": platform.rsplit("/", 1)[-1],
                            "os": "linux",
                        },
                    }
                ],
            }
            (layout / "index.json").write_text(json.dumps(index), encoding="utf-8")
        return subprocess.CompletedProcess(command, returncode, stdout="", stderr="failed" if returncode else "")

    monkeypatch.setattr(musl_cmd.subprocess, "run", run)
    return calls


def test_build_writes_an_oci_layout_and_prints_the_digest(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    _workspace(tmp_path, monkeypatch)
    calls = _fake_run(monkeypatch)
    output = tmp_path / "oci" / "musl"

    assert musl_cmd._musl(_args(output=output)) == 0

    assert (output / "index.json").is_file()
    assert DIGEST in capsys.readouterr().out
    command = calls[0]
    assert command[:3] == ["/usr/bin/docker", "buildx", "build"]
    assert "--platform" in command
    assert "linux/amd64" in command
    assert not any("--push" in value for value in command)
    assert any(value.startswith("BASE_IMAGE=alpine:3.24.2@sha256:") for value in command)


def test_build_rejects_a_dockerfile_lock_mismatch_before_docker(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = _workspace(tmp_path, monkeypatch)
    dockerfile = root / "docker" / "musl" / "Dockerfile"
    dockerfile.write_text(
        dockerfile.read_text(encoding="utf-8").replace("bash=5.3.9-r1", "bash=0"),
        encoding="utf-8",
    )
    calls = _fake_run(monkeypatch)
    output = tmp_path / "oci"

    with pytest.raises(ValueError, match=r"bash must be bash=5\.3\.9-r1"):
        musl_cmd._musl(_args(output=output))

    assert calls == []
    assert not output.exists()


def test_failed_build_leaves_no_partial_layout(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _workspace(tmp_path, monkeypatch)
    _fake_run(monkeypatch, returncode=1)
    output = tmp_path / "oci"

    with pytest.raises(ValueError, match="docker build failed"):
        musl_cmd._musl(_args(output=output))

    assert not output.exists()


def test_missing_docker_fails_without_writing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _workspace(tmp_path, monkeypatch)
    monkeypatch.setattr(musl_cmd.shutil, "which", lambda name: None)
    output = tmp_path / "oci"

    with pytest.raises(ValueError, match="docker is required"):
        musl_cmd._musl(_args(output=output))

    assert not output.exists()


def test_build_replaces_a_previous_layout(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _workspace(tmp_path, monkeypatch)
    _fake_run(monkeypatch)
    output = tmp_path / "oci"
    (output / "old").mkdir(parents=True)
    (output / "old" / "stale").write_text("old", encoding="utf-8")

    assert musl_cmd._musl(_args(output=output)) == 0

    assert (output / "index.json").is_file()
    assert not (output / "old").exists()


def test_checked_in_definition_matches_the_lock() -> None:
    lock = musl.load_musl_toolchain()
    dockerfile = (ROOT / "docker" / "musl" / "Dockerfile").read_text(encoding="utf-8")
    toolchain = (ROOT / "docker" / "musl" / "rust-toolchain.toml").read_text(encoding="utf-8")

    musl.validate_image_definition(lock, dockerfile, toolchain)


@pytest.mark.parametrize(
    ("old", "new", "message"),
    [
        ("FROM ${BASE_IMAGE}", "FROM alpine:3.24.1", "BASE_IMAGE"),
        ("bash=5.3.9-r1", "bash=5.3.9-r0", "bash must be bash=5.3.9-r1"),
        ("musl-cargo", "cargo-wrapper", "musl-cargo"),
        ("sha256sum -c", "true", "sha256"),
    ],
)
def test_definition_rejects_mismatches(old: str, new: str, message: str) -> None:
    lock = musl.load_musl_toolchain()
    dockerfile = (ROOT / "docker" / "musl" / "Dockerfile").read_text(encoding="utf-8")
    toolchain = (ROOT / "docker" / "musl" / "rust-toolchain.toml").read_text(encoding="utf-8")

    with pytest.raises(ValueError, match=message):
        musl.validate_image_definition(lock, dockerfile.replace(old, new), toolchain)


def test_definition_rejects_a_floating_rust_channel() -> None:
    lock = musl.load_musl_toolchain()
    dockerfile = (ROOT / "docker" / "musl" / "Dockerfile").read_text(encoding="utf-8")

    with pytest.raises(ValueError, match="pin a nightly channel"):
        musl.validate_image_definition(
            lock,
            dockerfile,
            '[toolchain]\nchannel = "stable"\ncomponents = ["rust-src"]\n',
        )


def test_image_package_versions_reject_architecture_drift() -> None:
    lock = musl.load_musl_toolchain()
    stable = lock.channels["stable"]
    packages = dict(stable.packages["aarch64"])
    packages["bash"] = "0.0.0-r0"
    drifted = replace(
        lock,
        channels={
            **lock.channels,
            "stable": replace(stable, packages={**stable.packages, "aarch64": packages}),
        },
    )

    with pytest.raises(ValueError, match="one approved stable version per architecture: bash"):
        musl.image_package_versions(drifted)


def test_parser_builds_the_musl_image_and_has_no_push_path() -> None:
    args = _parser().parse_args(["musl", "build", "--platform", "linux/arm64"])

    assert args.command == "musl"
    assert args.operation == "build"
    assert args.platform == "linux/arm64"
    assert args.output == Path(".work/oci/musl")
    with pytest.raises(SystemExit):
        _parser().parse_args(["musl", "build", "--platform", "linux/arm64", "--push"])


def test_build_rejects_gcc_in_the_image_before_docker(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = _workspace(tmp_path, monkeypatch)
    dockerfile = root / "docker" / "musl" / "Dockerfile"
    dockerfile.write_text(
        dockerfile.read_text(encoding="utf-8").replace(
            "    git=2.54.0-r0 \\\n", "    gcc=15.2.0-r5 \\\n    git=2.54.0-r0 \\\n"
        ),
        encoding="utf-8",
    )
    calls = _fake_run(monkeypatch)

    with pytest.raises(ValueError, match="puts GCC objects in the build image"):
        musl_cmd._musl(_args(output=tmp_path / "oci"))

    assert calls == []
