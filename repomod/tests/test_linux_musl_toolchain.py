from __future__ import annotations

import json
import shutil
from pathlib import Path

import pytest

from repomod.rules import musl_toolchain as musl
from repomod.types import Finding, RepoId, Repository

FIXTURES = Path(__file__).parent / "fixtures" / "musl_toolchain"
STABLE_INDEX = "sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6"
EDGE_INDEX = "sha256:020dfcbaaf4cc1078bf2d9c7ba31a8466e334061dcd2f248001d68f79e52c000"
EDGE_AMD64 = "sha256:266f29255458134745f2bf588cb23ed1ed1768b96ff2580a05d70a8aba59e145"
EDGE_ARM64 = "sha256:42d86a3173522de4786cfba0b5d631dbadb3d03a86d218dafb070dafd9809c7e"
CANONICAL_DIGEST = "sha256:" + "9" * 64


def approved_lock(image: bool = False) -> musl.MuslToolchain:
    source = (FIXTURES / "lock.toml").read_text(encoding="utf-8")
    if image:
        source += f'\n[image]\nreference = "ghcr.io/acme/musl-toolchain"\ndigest = "{CANONICAL_DIGEST}"\n'
    return musl._parse_musl_toolchain(source)


def install_lock(monkeypatch: pytest.MonkeyPatch, lock: musl.MuslToolchain) -> None:
    def load(path: Path | None = None) -> musl.MuslToolchain:
        return lock

    monkeypatch.setattr(musl, "load_musl_toolchain", load)


def repository(tags: tuple[str, ...] = ("tool",), name: str = "acme/demo") -> Repository:
    return Repository(RepoId.parse(name), tags)


def approved_repo(tmp_path: Path) -> Path:
    root = tmp_path / "repo"
    shutil.copytree(FIXTURES / "approved", root)
    return root


def bare_repo(tmp_path: Path, dockerfile: str, *, scoped: bool = True) -> Path:
    root = tmp_path / "repo"
    root.mkdir()
    source = f"# musl builder\n{dockerfile}" if scoped else dockerfile
    (root / "Dockerfile").write_text(source, encoding="utf-8")
    return root


def check(
    monkeypatch: pytest.MonkeyPatch, root: Path, tags: tuple[str, ...] = ("tool",), image: bool = False
) -> list[Finding]:
    install_lock(monkeypatch, approved_lock(image))
    return musl._linux_musl_toolchain_findings(root, repository(tags))


def messages(findings: list[Finding]) -> list[str]:
    return [finding.message for finding in findings]


def test_approved_builder_passes(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    assert check(monkeypatch, approved_repo(tmp_path)) == []


def test_floating_base_is_reported(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    root = bare_repo(tmp_path, "FROM alpine:edge\n")

    findings = check(monkeypatch, root)

    assert any("approved stable" in message or "approved edge" in message for message in messages(findings))
    assert findings[0].fixable is True


def test_missing_digest_is_reported(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    root = bare_repo(tmp_path, "FROM alpine:3.24.2\n")

    findings = check(monkeypatch, root)

    assert any("approved stable digest" in message for message in messages(findings))


def test_unversioned_package_is_reported(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    root = bare_repo(
        tmp_path,
        f"FROM alpine:3.24.2@{STABLE_INDEX}\nRUN apk add --no-cache bash\n",
    )

    findings = check(monkeypatch, root)

    assert messages(findings) == ["package bash must be pinned as bash=5.3.9-r1"]
    assert findings[0].fixable is True


def test_stale_package_is_reported(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    root = bare_repo(
        tmp_path,
        f"FROM alpine:3.24.2@{STABLE_INDEX}\nRUN apk add --no-cache musl-dev=1.2.6-r2\n",
    )

    findings = check(monkeypatch, root)

    assert messages(findings) == ["package musl-dev=1.2.6-r2 is stale; use musl-dev=1.2.6-r3"]


def test_unapproved_package_is_manual(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    source = (
        f"FROM alpine:3.24.2@{STABLE_INDEX}\n"
        "RUN apk add --no-cache bash clang\n"
    )
    root = bare_repo(tmp_path, source)

    findings = check(monkeypatch, root)

    assert any("clang is not in musl-toolchain.toml" in message for message in messages(findings))
    assert not any(finding.fixable for finding in findings)


def test_glibc_final_stage_is_reported(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    root = bare_repo(
        tmp_path,
        "FROM ubuntu:24.04\n"
        "RUN apt-get update && apt-get install -y gcc\n"
        "RUN cargo build --release --target x86_64-unknown-linux-gnu\n",
    )

    findings = check(monkeypatch, root)

    assert any("Debian or Ubuntu" in message for message in messages(findings))
    assert any("apt-get" in message for message in messages(findings))
    assert any("unknown-linux-gnu" in message for message in messages(findings))


def test_out_of_scope_ubuntu_dockerfile_is_not_flagged(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = bare_repo(
        tmp_path,
        "FROM ubuntu:24.04\nRUN apt-get update && apt-get install -y curl\n",
        scoped=False,
    )

    assert check(monkeypatch, root) == []


@pytest.mark.parametrize("tag", ["fork", "private-tool", "macos"])
def test_skipped_repository_is_not_flagged(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, tag: str
) -> None:
    root = approved_repo(tmp_path)
    (root / "Dockerfile").write_text("FROM alpine:edge\n", encoding="utf-8")

    assert check(monkeypatch, root, (tag,)) == []


def test_ci_only_rust_builder_passes_with_base_and_packages(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = bare_repo(
        tmp_path,
        f"FROM alpine:3.24.2@{STABLE_INDEX}\n"
        "RUN apk add --no-cache bash=5.3.9-r1 musl-dev=1.2.6-r3\n",
    )
    workflows = root / ".github" / "workflows"
    workflows.mkdir(parents=True)
    (workflows / "ci.yml").write_text(
        "name: ci\non: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n"
        "    steps:\n      - run: cargo test --target x86_64-unknown-linux-musl\n",
        encoding="utf-8",
    )

    assert check(monkeypatch, root) == []


def test_canonical_image_stage_is_exempt_and_checked(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = bare_repo(
        tmp_path,
        f"FROM ghcr.io/acme/musl-toolchain@{CANONICAL_DIGEST}\nRUN apk add --no-cache unpinned\n",
    )

    assert check(monkeypatch, root, image=True) == []

    (root / "Dockerfile").write_text(
        f"FROM ghcr.io/acme/musl-toolchain@{STABLE_INDEX}\nRUN apk add --no-cache unpinned\n",
        encoding="utf-8",
    )
    findings = check(monkeypatch, root, image=True)

    assert messages(findings) == [f"canonical builder image must use the approved digest {CANONICAL_DIGEST}"]
    assert findings[0].fixable is True


def test_host_musl_build_is_reported(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    root = approved_repo(tmp_path)
    workflows = root / ".github" / "workflows"
    source = (workflows / "release.yml").read_text(encoding="utf-8")
    source = source.replace("      - uses: docker/setup-buildx-action@v4\n", "")
    source = source.replace(
        "      - uses: docker/build-push-action@v7\n"
        "        with:\n"
        "          context: .\n"
        "          file: Dockerfile\n",
        "",
    )
    (workflows / "release.yml").write_text(source, encoding="utf-8")

    findings = check(monkeypatch, root)

    assert any("in a container or through Docker" in message for message in messages(findings))


def test_rust_builder_without_contract_skips_toolchain_requirements(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = bare_repo(
        tmp_path,
        f"FROM alpine:3.24.2@{STABLE_INDEX}\n"
        "RUN apk add --no-cache bash=5.3.9-r1 musl-dev=1.2.6-r3\n",
    )
    (root / "Cargo.toml").write_text('[package]\nname = "demo"\nversion = "1.0.0"\n', encoding="utf-8")

    assert check(monkeypatch, root) == []


def test_fixer_rewrites_base_and_packages(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    root = bare_repo(
        tmp_path,
        "FROM alpine:edge\nRUN apk add --no-cache bash musl-dev=1.2.6-r2\n",
    )
    install_lock(monkeypatch, approved_lock())

    plan = musl._linux_musl_toolchain_fix(root, repository())
    for relative, source in plan.items():
        (root / relative).write_text(source, encoding="utf-8")

    assert f"FROM alpine:edge@{EDGE_INDEX}" in plan["Dockerfile"]
    assert "bash=5.3.9-r2" in plan["Dockerfile"]
    assert "musl-dev=1.2.6-r6" in plan["Dockerfile"]
    assert musl._linux_musl_toolchain_findings(root, repository()) == []


def test_fixer_does_not_edit_a_file_with_an_unapproved_package(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = bare_repo(
        tmp_path,
        f"FROM alpine:3.24.2@{STABLE_INDEX}\nRUN apk add --no-cache bash clang\n",
    )
    install_lock(monkeypatch, approved_lock())

    assert musl._linux_musl_toolchain_fix(root, repository()) == {}
    findings = musl._linux_musl_toolchain_findings(root, repository())
    assert not any(finding.fixable for finding in findings)


def _python_lock(root: Path, musl_dev: str = "1.2.6-r6") -> None:
    document = {
        "linux_toolchain": {
            "release": "3.25.0_alpha20260805",
            "image_index_digest": EDGE_INDEX,
            "images": {
                "amd64": {"digest": EDGE_AMD64},
                "arm64": {"digest": EDGE_ARM64},
            },
            "apk_closure": {
                "x86_64": [
                    {"name": "musl", "version": "1.2.6-r6"},
                    {"name": "musl-dev", "version": musl_dev},
                ],
                "aarch64": [
                    {"name": "musl", "version": "1.2.6-r6"},
                    {"name": "musl-dev", "version": musl_dev},
                ],
            },
        }
    }
    (root / "bootstrap.lock.json").write_text(json.dumps(document), encoding="utf-8")


def test_python_build_adapter_checks_the_bootstrap_closure(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = tmp_path / "repo"
    root.mkdir()
    _python_lock(root)
    tags = ("build", "python")
    install_lock(monkeypatch, approved_lock())
    name = repository(tags, "laputa-systems/python-build")

    assert musl._linux_musl_toolchain_findings(root, name) == []

    _python_lock(root, "1.2.6-r5")
    findings = musl._linux_musl_toolchain_findings(root, name)

    assert any("bootstrap refresh" in message for message in messages(findings))


def _chromium(root: Path, release: str = "3.24.2", musl_dev: str = "1.2.6-r3") -> None:
    (root / "config").mkdir(parents=True, exist_ok=True)
    document = {"base_release": release, "world": [f"musl-dev={musl_dev}"]}
    (root / "config" / "packages.amd64.lock").write_text(json.dumps(document), encoding="utf-8")
    (root / "Dockerfile").write_text(
        f"ARG BASE_IMAGE=alpine@{STABLE_INDEX}\n"
        "ARG GLIBC_IMAGE=debian@sha256:" + "a" * 64 + "\n"
        "FROM ${GLIBC_IMAGE} AS compiler-host-runtime\n"
        "FROM ${BASE_IMAGE}\n"
        f"RUN apk add --no-cache musl-dev={musl_dev}\n"
        "RUN ln -s x86_64-unknown-linux-gnu /opt/compiler\n",
        encoding="utf-8",
    )


def test_chromium_build_adapter_checks_lock_and_base_image(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = tmp_path / "repo"
    root.mkdir()
    _chromium(root)
    name = repository(("build", "python"), "laputa-systems/chromium-build")
    install_lock(monkeypatch, approved_lock())

    assert musl._linux_musl_toolchain_findings(root, name) == []

    _chromium(root, release="3.24.1", musl_dev="1.2.6-r2")
    findings = musl._linux_musl_toolchain_findings(root, name)

    assert any("base_release" in message for message in messages(findings))
    assert any("package lock musl-dev" in message for message in messages(findings))
    assert any("Dockerfile musl-dev" in message for message in messages(findings))


def _node_workflow(root: Path, packages: str) -> None:
    workflows = root / ".github" / "workflows"
    workflows.mkdir(parents=True, exist_ok=True)
    (workflows / "build.yml").write_text(
        "name: build\n"
        "on: push\n"
        "env:\n"
        f"  ALPINE_AMD64: alpine@{EDGE_AMD64}\n"
        "jobs:\n"
        "  build:\n"
        "    runs-on: ubuntu-latest\n"
        "    steps:\n"
        "      - run: |\n"
        "          docker run --rm alpine sh -ec '\n"
        f"            apk add --no-cache {packages}\n"
        "          '\n",
        encoding="utf-8",
    )


def test_node_build_adapter_checks_and_fixes_the_workflow(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = tmp_path / "repo"
    root.mkdir()
    _node_workflow(root, "bash musl-dev")
    name = repository(("build", "python"), "laputa-systems/node-build")
    install_lock(monkeypatch, approved_lock())

    findings = musl._linux_musl_toolchain_findings(root, name)
    assert any("bash must be pinned" in message for message in messages(findings))
    assert all(finding.fixable for finding in findings)

    plan = musl._linux_musl_toolchain_fix(root, name)
    for relative, source in plan.items():
        (root / relative).write_text(source, encoding="utf-8")
    assert musl._linux_musl_toolchain_findings(root, name) == []

    _node_workflow(root, "bash clang")
    findings = musl._linux_musl_toolchain_findings(root, name)
    assert any("clang is not in musl-toolchain.toml" in message for message in messages(findings))
    assert not any(finding.fixable for finding in findings)
    assert musl._linux_musl_toolchain_fix(root, name) == {}


def test_zzz_adapter_requires_and_adds_the_checksum(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = tmp_path / "repo"
    workflows = root / ".github" / "workflows"
    workflows.mkdir(parents=True)
    workflow = (
        "name: release\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n"
        "      - name: Download zig\n"
        "        run: curl -fsSL https://ziglang.org/download/0.16.0/zig-x86_64-linux-0.16.0.tar.xz | tar xJ\n"
    )
    (workflows / "release.yml").write_text(workflow, encoding="utf-8")
    name = repository(("tool", "linux"), "laputa-systems/zzz")
    install_lock(monkeypatch, approved_lock())

    findings = musl._linux_musl_toolchain_findings(root, name)
    assert messages(findings) == ["Zig archive download must verify the approved sha256 checksum"]
    assert findings[0].fixable is True

    plan = musl._linux_musl_toolchain_fix(root, name)
    for relative, source in plan.items():
        (root / relative).write_text(source, encoding="utf-8")
    assert musl._linux_musl_toolchain_findings(root, name) == []


def test_git_build_adapter_requires_exact_packages(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = bare_repo(
        tmp_path,
        f"FROM alpine:latest@{STABLE_INDEX}\nRUN apk add --no-cache bash\n",
    )
    name = repository(("tool", "c"), "laputa-systems/git-build")
    install_lock(monkeypatch, approved_lock())

    findings = musl._linux_musl_toolchain_findings(root, name)
    assert messages(findings) == ["package bash must be pinned as bash=5.3.9-r1"]
    assert findings[0].fixable is True

    plan = musl._linux_musl_toolchain_fix(root, name)
    for relative, source in plan.items():
        (root / relative).write_text(source, encoding="utf-8")
    assert musl._linux_musl_toolchain_findings(root, name) == []
