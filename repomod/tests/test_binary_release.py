from __future__ import annotations

import shutil
from pathlib import Path

import pytest

from repomod.rules.binary_release import (
    _binary_release_findings,
    _binary_release_standard_findings,
    _binary_release_static_findings,
    _binary_release_static_fix,
    _rust_musl_image_findings,
)
from repomod.types import RepoId, Repository


def write_workflow(root: Path, source: str) -> None:
    workflows = root / ".github" / "workflows"
    workflows.mkdir(parents=True, exist_ok=True)
    (workflows / "release.yml").write_text(source, encoding="utf-8")


def binary_workflow(command: str) -> str:
    return (
        "name: release\n"
        "on:\n"
        "  workflow_dispatch:\n"
        "jobs:\n"
        "  build:\n"
        "    strategy:\n"
        "      matrix:\n"
        "        target: [x86_64-unknown-linux-musl, aarch64-apple-darwin]\n"
        "    steps:\n"
        f"      - run: {command}\n"
        "      - uses: actions/upload-artifact@v7\n"
        "        with:\n          path: demo\n"
        "  release:\n"
        "    env:\n"
        "      RELEASE_TAG: release-${{ github.sha }}\n"
        "    steps:\n"
        "      - uses: actions/download-artifact@v7\n"
        "        with:\n          path: dist\n"
        "      - run: zip dist/demo.zip demo\n"
        "      - run: gh release create \"$RELEASE_TAG\" dist/demo.zip --prerelease --target \"$GITHUB_SHA\"\n"
    )


def standard_workflow(targets: list[str]) -> str:
    matrix = "\n".join(f"            target: {target}" for target in targets)
    return (
        "name: release\n"
        "on:\n"
        "  workflow_dispatch:\n"
        "jobs:\n"
        "  preflight:\n"
        "    steps:\n"
        "      - run: |\n"
        "          default_branch=$(gh api \"repos/$REPOSITORY\" --jq '.default_branch')\n"
        "          if [[ \"$REF\" != \"refs/heads/$default_branch\" ]]; then exit 1; fi\n"
        "  build:\n"
        "    strategy:\n"
        "      matrix:\n"
        "        include:\n"
        f"{matrix}\n"
        "    steps:\n"
        "      - run: cargo build --locked --release --target \"${{ matrix.target }}\"\n"
        "      - uses: actions/upload-artifact@v7\n"
        "  release:\n"
        "    env:\n"
        "      RELEASE_TAG: release-${{ github.sha }}\n"
        "    steps:\n"
        "      - uses: actions/download-artifact@v7\n"
        "        with:\n          path: dist\n"
        "      - run: |\n"
        "          if response=$(gh api \"repos/$REPOSITORY/git/refs\" "
        "-f \"ref=refs/tags/$RELEASE_TAG\" -f \"sha=$GITHUB_SHA\" 2>&1); then\n"
        "            echo ok\n"
        "          fi\n"
        "      - run: |\n"
        "          zip -9 dist/demo.zip demo\n"
        "          sha256sum dist/demo.zip > dist/SHA256SUMS\n"
        "          gh release create \"$RELEASE_TAG\" dist/demo.zip dist/SHA256SUMS "
        "--prerelease --verify-tag --title \"$RELEASE_TAG\"\n"
    )


def write_rust_executable(root: Path) -> None:
    (root / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "1.0.0"\nedition = "2024"\n',
        encoding="utf-8",
    )
    source = root / "src" / "main.rs"
    source.parent.mkdir(exist_ok=True)
    source.write_text("fn main() {}\n", encoding="utf-8")


def write_musl_dockerfile(root: Path) -> None:
    (root / "Dockerfile").write_text(
        "FROM alpine:3.24.1\n"
        "RUN wget -O /tmp/rustup-init "
        "https://static.rust-lang.org/rustup/archive/1.29.1/x86_64-unknown-linux-musl/rustup-init "
        "&& echo abc | sha256sum -c -\n"
        "RUN wget -O /tmp/llvm.tar.xz https://github.com/laputa-systems/llvm-prebuilt-musl/releases/download/x.tar.xz "
        "&& echo def | sha256sum -c -\n"
        "RUN rustup toolchain install nightly --target x86_64-unknown-linux-musl "
        "--target aarch64-unknown-linux-musl --component rust-src\n"
        "RUN cat > /usr/local/bin/musl-cargo <<'EOF'\n"
        "RUN for obj in crtbegin.o crtend.o; do echo $obj; done\n"
        "RUN ar rcs /usr/lib/libutil.a\n",
        encoding="utf-8",
    )


@pytest.mark.parametrize(
    ("language", "manifest", "command"),
    [
        ("rust", "Cargo.toml", "cargo build --release --target x86_64-unknown-linux-musl"),
        ("go", "go.mod", "go build -o demo ./cmd/demo"),
        ("swift", "Package.swift", "swift build -c release"),
    ],
)
def test_standard_native_prerelease_requires_tool_only(
    tmp_path: Path, language: str, manifest: str, command: str
) -> None:
    if language == "rust":
        (tmp_path / manifest).write_text(
            '[package]\nname = "demo"\nversion = "1.0.0"\nedition = "2024"\n',
            encoding="utf-8",
        )
        source = tmp_path / "src" / "main.rs"
        source.parent.mkdir()
        source.write_text("fn main() {}\n", encoding="utf-8")
    elif language == "go":
        (tmp_path / manifest).write_text("module example.com/demo\ngo 1.23\n", encoding="utf-8")
        (tmp_path / "main.go").write_text("package main\nfunc main() {}\n", encoding="utf-8")
    else:
        (tmp_path / manifest).write_text(
            '// swift-tools-version: 6.0\nimport PackageDescription\n'
            'let package = Package(name: "Demo", targets: [.executableTarget(name: "Demo")])\n',
            encoding="utf-8",
        )
    write_workflow(tmp_path, binary_workflow(command))
    tool = Repository(RepoId("laputa-systems", "srv"), ("tool",))

    assert _binary_release_findings(tmp_path, tool) == []
    vacuous = Repository(tool.id, ("library",))
    vacuous_findings = _binary_release_findings(tmp_path, vacuous)

    assert len(vacuous_findings) == 1
    assert vacuous_findings[0].rule == "binary-release-tags"
    assert "require the tool role" in vacuous_findings[0].message


_TOOL_FIXTURES = [
    ("laputa-systems_srv", "laputa-systems/srv"),
    ("joshuarli_ish", "joshuarli/ish"),
    ("laputa-systems_diff-pretty", "laputa-systems/diff-pretty"),
    ("laputa-systems_xsh", "laputa-systems/xsh"),
    ("joshuarli_mactop", "joshuarli/mactop"),
    ("joshuarli_gh", "joshuarli/gh"),
    ("laputa-systems_zzz", "laputa-systems/zzz"),
    ("laputa-systems_git-build", "laputa-systems/git-build"),
]


def test_captured_fleet_workflows_require_exactly_the_tool_role(tmp_path: Path) -> None:
    for fixture_name, repository_id in _TOOL_FIXTURES:
        fixture = Path(__file__).parent / "fixtures" / "binary_release" / fixture_name
        root = tmp_path / fixture_name
        shutil.copytree(fixture, root)
        owner, name = repository_id.split("/")

        assert _binary_release_findings(
            root, Repository(RepoId(owner, name), ("tool",))
        ) == [], fixture_name


def test_captured_binary_releases_reject_extra_roles(tmp_path: Path) -> None:
    for fixture_name, repository_id in _TOOL_FIXTURES:
        fixture = Path(__file__).parent / "fixtures" / "binary_release" / fixture_name
        for extra_tag in ("library", "app"):
            root = tmp_path / f"{fixture_name}-{extra_tag}"
            shutil.copytree(fixture, root)
            owner, name = repository_id.split("/")

            findings = _binary_release_findings(
                root, Repository(RepoId(owner, name), ("tool", extra_tag))
            )

            assert len(findings) == 1, (fixture_name, extra_tag)
            assert findings[0].severity == "error"
            assert "remove " + extra_tag in findings[0].message


def test_captured_binary_releases_allow_fork_platform_and_language_tags(
    tmp_path: Path,
) -> None:
    allowed = [
        ("tool", "fork"),
        ("tool", "linux"),
        ("tool", "macos", "swift"),
        ("tool", "rust"),
    ]
    for fixture_name, repository_id in _TOOL_FIXTURES:
        fixture = Path(__file__).parent / "fixtures" / "binary_release" / fixture_name
        owner, name = repository_id.split("/")
        for tags in allowed:
            root = tmp_path / f"{fixture_name}-{'-'.join(tags)}"
            shutil.copytree(fixture, root)

            assert _binary_release_findings(
                root, Repository(RepoId(owner, name), tags)
            ) == [], (fixture_name, tags)


def write_c_executable(root: Path) -> None:
    (root / "Makefile").write_text(
        "CC ?= clang\nCFLAGS ?= -O3\n\ndemo: src/demo/demo.c\n"
        "\t$(CC) $(CFLAGS) src/demo/demo.c -o demo\n",
        encoding="utf-8",
    )
    source = root / "src" / "demo" / "demo.c"
    source.parent.mkdir(parents=True, exist_ok=True)
    source.write_text("int main(void) { return 0; }\n", encoding="utf-8")


def test_make_c_build_recognizes_c_makefile_layout(tmp_path: Path) -> None:
    write_c_executable(tmp_path)
    write_workflow(tmp_path, binary_workflow("make git"))
    tool = Repository(RepoId("laputa-systems", "git-build"), ("tool",))

    assert _binary_release_findings(tmp_path, tool) == []
    assert _binary_release_findings(tmp_path, Repository(tool.id, ("tool", "c"))) == []

    bare = Repository(tool.id, ("library",))
    bare_findings = _binary_release_findings(tmp_path, bare)

    assert len(bare_findings) == 1
    assert bare_findings[0].rule == "binary-release-tags"


def test_make_without_c_sources_is_not_a_native_build(tmp_path: Path) -> None:
    (tmp_path / "Makefile").write_text("help:\n\techo help\n", encoding="utf-8")
    write_workflow(tmp_path, binary_workflow("make git"))
    repository = Repository(RepoId("laputa-systems", "git-build"), ("tool",))

    findings = _binary_release_findings(tmp_path, repository)

    assert len(findings) == 1
    assert findings[0].severity == "warning"
    assert "does not establish" in findings[0].message


def write_c_dockerfile(root: Path) -> None:
    (root / "Dockerfile").write_text(
        "FROM alpine:3.24.1\n"
        "ADD --checksum=sha256:abc --unpack=true "
        "https://www.kernel.org/pub/software/scm/git/git-2.56.0.tar.xz /src/\n"
        "WORKDIR /src/git-2.56.0\n"
        'RUN make CC=clang CFLAGS="-O3" git git-shell\n',
        encoding="utf-8",
    )


def test_docker_c_build_recognizes_tarball_layout_without_sources(
    tmp_path: Path,
) -> None:
    write_c_dockerfile(tmp_path)
    write_workflow(tmp_path, binary_workflow("make git"))
    repository = Repository(RepoId("laputa-systems", "git-build"), ("tool", "c"))

    assert _binary_release_findings(tmp_path, repository) == []

    (tmp_path / "Dockerfile").write_text("FROM alpine:3.24.1\n", encoding="utf-8")

    findings = _binary_release_findings(tmp_path, repository)

    assert len(findings) == 1
    assert findings[0].severity == "warning"


def test_captured_c_makefile_fixture_passes_binary_policy(tmp_path: Path) -> None:
    fixture = (
        Path(__file__).parent
        / "fixtures"
        / "binary_release"
        / "laputa-systems_git-build"
    )
    root = tmp_path / "git-build"
    shutil.copytree(fixture, root)
    repository = Repository(RepoId("laputa-systems", "git-build"), ("tool", "c"))

    assert _binary_release_findings(root, repository) == []
    assert _binary_release_standard_findings(root, repository) == []

    plan = _binary_release_static_fix(root, repository)
    assert set(plan) == {".github/workflows/release.yml"}
    (root / ".github" / "workflows" / "release.yml").write_text(
        plan[".github/workflows/release.yml"], encoding="utf-8"
    )

    assert _binary_release_static_findings(root, repository) == []


def test_static_smoke_permits_multiple_elf_artifacts_per_arch(tmp_path: Path) -> None:
    write_rust_executable(tmp_path)
    write_workflow(
        tmp_path,
        binary_workflow("cargo build --release --target x86_64-unknown-linux-musl"),
    )
    tool = Repository(RepoId("acme", "demo"), ("tool",))
    workflow = _binary_release_static_fix(tmp_path, tool)[
        ".github/workflows/release.yml"
    ]

    assert "-lt 1" in workflow
    assert "at least one x86_64 and one aarch64" in workflow


def test_captured_python_wheel_prerelease_is_not_a_binary_tool_policy(
    tmp_path: Path,
) -> None:
    fixture = Path(__file__).parent / "fixtures" / "binary_release" / "joshuarli_pytest-parallel"
    root = tmp_path / "pytest-parallel"
    shutil.copytree(fixture, root)

    findings = _binary_release_findings(
        root,
        Repository(RepoId("joshuarli", "pytest-parallel"), ("library", "pypi", "python")),
    )

    assert findings == []


def test_binary_static_fixer_checks_downloaded_linux_artifacts_before_prerelease(
    tmp_path: Path,
) -> None:
    write_rust_executable(tmp_path)
    write_workflow(
        tmp_path,
        binary_workflow("cargo build --release --target x86_64-unknown-linux-musl"),
    )
    tool = Repository(RepoId("acme", "demo"), ("tool", "fork"))

    finding = _binary_release_static_findings(tmp_path, tool)
    plan = _binary_release_static_fix(tmp_path, tool)

    assert len(finding) == 1
    assert finding[0].rule == "binary-release-static"
    assert finding[0].fixable
    assert set(plan) == {".github/workflows/release.yml"}
    workflow = plan[".github/workflows/release.yml"]
    release_job = workflow.split("  release:\n", 1)[1]
    assert release_job.index("actions/download-artifact") < release_job.index(
        "Verify Linux ELF artifacts are fully static"
    ) < release_job.index("gh release create")
    assert "readelf -lW" in release_job
    assert "readelf -dW" in release_job
    assert "INTERP" in release_job
    assert "NEEDED" in release_job
    assert "shopt -s globstar nullglob" in release_job
    assert "Advanced Micro Devices X86-64" in release_job
    assert "AArch64" in release_job

    workflow_path = tmp_path / ".github" / "workflows" / "release.yml"
    workflow_path.write_text(workflow, encoding="utf-8")

    assert _binary_release_static_findings(tmp_path, tool) == []
    assert _binary_release_static_fix(tmp_path, tool) == {}


def test_binary_static_fixer_repositions_a_misplaced_smoke(tmp_path: Path) -> None:
    write_rust_executable(tmp_path)
    write_workflow(
        tmp_path,
        binary_workflow("cargo build --release --target x86_64-unknown-linux-musl"),
    )
    tool = Repository(RepoId("acme", "demo"), ("tool",))
    workflow_path = tmp_path / ".github" / "workflows" / "release.yml"
    workflow = _binary_release_static_fix(tmp_path, tool)[".github/workflows/release.yml"]
    smoke_start = workflow.index("      - name: Verify Linux ELF artifacts are fully static")
    smoke_end = workflow.index("      - run: zip dist/demo.zip demo", smoke_start)
    smoke = workflow[smoke_start:smoke_end]
    misplaced = workflow[:smoke_start] + workflow[smoke_end:] + smoke
    workflow_path.write_text(misplaced, encoding="utf-8")

    finding = _binary_release_static_findings(tmp_path, tool)
    plan = _binary_release_static_fix(tmp_path, tool)

    assert len(finding) == 1
    assert finding[0].fixable
    fixed = plan[".github/workflows/release.yml"]
    release_job = fixed.split("  release:\n", 1)[1]
    assert release_job.index("actions/download-artifact") < release_job.index(
        "Verify Linux ELF artifacts are fully static"
    ) < release_job.index("gh release create")
    assert fixed.count("Verify Linux ELF artifacts are fully static") == 1


def test_binary_static_rule_skips_macos_only_tools(tmp_path: Path) -> None:
    write_rust_executable(tmp_path)
    write_workflow(
        tmp_path,
        binary_workflow("cargo build --release --target x86_64-unknown-linux-musl"),
    )

    assert _binary_release_static_findings(
        tmp_path,
        Repository(RepoId("acme", "demo"), ("tool", "macos")),
    ) == []

def test_shared_library_prerelease_does_not_require_tool_only(tmp_path: Path) -> None:
    (tmp_path / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "1.0.0"\nedition = "2024"\n'
        '[lib]\ncrate-type = ["cdylib"]\n',
        encoding="utf-8",
    )
    workflow = binary_workflow("cargo build --release --lib").replace(
        "zip dist/demo.zip demo", "tar -czf dist/libdemo.tar.gz target/release/libdemo.so"
    )
    write_workflow(tmp_path, workflow)
    library = Repository(RepoId("acme", "demo"), ("library", "rust", "crates-io"))

    assert _binary_release_findings(tmp_path, library) == []


def test_python_wheel_prerelease_does_not_activate_binary_release_policy(
    tmp_path: Path,
) -> None:
    (tmp_path / "pyproject.toml").write_text("[project]\nname = 'demo'\n", encoding="utf-8")
    write_workflow(
        tmp_path,
        "on:\n  workflow_dispatch:\njobs:\n  package:\n    steps:\n"
        "      - run: uv build\n"
        "      - uses: actions/upload-artifact@v7\n"
        "        with:\n          path: dist/*.whl\n"
        "      - run: gh release create \"release-${GITHUB_SHA}\" dist/*.whl --prerelease --target \"$GITHUB_SHA\"\n",
    )
    repository = Repository(RepoId("joshuarli", "pytest-parallel"), ("library", "pypi", "python"))

    assert _binary_release_findings(tmp_path, repository) == []


def test_library_ci_without_release_is_not_classified(tmp_path: Path) -> None:
    (tmp_path / "Cargo.toml").write_text("", encoding="utf-8")
    write_workflow(tmp_path, "on:\n  push:\njobs:\n  test:\n    steps:\n      - run: cargo test\n")
    repository = Repository(RepoId("laputa-systems", "h12tiny"), ("library", "rust", "crates-io"))

    assert _binary_release_findings(tmp_path, repository) == []


def test_unconventional_prerelease_is_reported_without_guessing_role(tmp_path: Path) -> None:
    write_workflow(
        tmp_path,
        "on:\n  workflow_dispatch:\njobs:\n  release:\n    steps:\n"
        "      - run: gh release create \"release-${GITHUB_SHA}\" --prerelease --target \"$GITHUB_SHA\"\n",
    )
    repository = Repository(RepoId("acme", "unknown"), ("library",))

    findings = _binary_release_findings(tmp_path, repository)

    assert len(findings) == 1
    assert findings[0].severity == "warning"
    assert "does not establish" in findings[0].message
    assert "tool role" not in findings[0].message


def test_api_tag_creation_establishes_binary_contract(tmp_path: Path) -> None:
    write_rust_executable(tmp_path)
    write_workflow(
        tmp_path,
        "on:\n  workflow_dispatch:\njobs:\n  build:\n    steps:\n"
        "      - run: cargo build --locked --release --target x86_64-unknown-linux-musl\n"
        "      - uses: actions/upload-artifact@v7\n"
        "      - uses: actions/download-artifact@v7\n"
        "      - run: zip dist/demo.zip demo\n"
        "  release:\n    steps:\n"
        "      - run: |\n"
        "          if response=$(gh api \"repos/$REPOSITORY/git/refs\" "
        "-f \"ref=refs/tags/$RELEASE_TAG\" -f \"sha=$GITHUB_SHA\" 2>&1); then\n"
        "            echo ok\n"
        "          fi\n"
        "      - run: gh release create \"release-${{ github.sha }}\" dist/demo.zip --prerelease --verify-tag\n",
    )

    assert _binary_release_findings(tmp_path, Repository(RepoId("acme", "demo"), ("tool",))) == []


def test_standard_rule_accepts_portable_musl_matrix(tmp_path: Path) -> None:
    write_workflow(
        tmp_path,
        standard_workflow(
            [
                "aarch64-apple-darwin",
                "x86_64-unknown-linux-musl",
                "aarch64-unknown-linux-musl",
            ]
        ),
    )
    repository = Repository(RepoId("acme", "demo"), ("tool",))

    assert _binary_release_standard_findings(tmp_path, repository) == []


def test_standard_rule_enforces_scope_matrix(tmp_path: Path) -> None:
    portable = Repository(RepoId("acme", "demo"), ("tool",))
    write_workflow(tmp_path, standard_workflow(["aarch64-apple-darwin"]))

    findings = _binary_release_standard_findings(tmp_path, portable)

    assert any("must build exactly" in finding.message for finding in findings)

    macos = Repository(RepoId("acme", "demo"), ("tool", "macos", "swift"))
    assert _binary_release_standard_findings(tmp_path, macos) == []

    linux = Repository(RepoId("acme", "demo"), ("tool", "linux"))
    write_workflow(tmp_path, standard_workflow(["x86_64-linux-musl", "aarch64-linux-musl"]))

    assert _binary_release_standard_findings(tmp_path, linux) == []
    assert any(
        "must build exactly" in finding.message
        for finding in _binary_release_standard_findings(tmp_path, portable)
    )


def test_standard_rule_rejects_glibc_targets(tmp_path: Path) -> None:
    write_workflow(
        tmp_path,
        standard_workflow(
            [
                "aarch64-apple-darwin",
                "x86_64-unknown-linux-gnu",
                "aarch64-unknown-linux-gnu",
            ]
        ),
    )
    repository = Repository(RepoId("acme", "demo"), ("tool",))

    findings = _binary_release_standard_findings(tmp_path, repository)

    assert any("glibc" in finding.message for finding in findings)


def test_standard_rule_requires_ish_format(tmp_path: Path) -> None:
    write_workflow(tmp_path, binary_workflow("cargo build --release"))
    repository = Repository(RepoId("acme", "demo"), ("tool",))

    findings = _binary_release_standard_findings(tmp_path, repository)
    messages = [finding.message for finding in findings]

    assert any("SHA256SUMS" in message for message in messages)
    assert any("--verify-tag" in message for message in messages)
    assert any("preflight" in message for message in messages)


def test_standard_rule_skips_forks_and_private_tools(tmp_path: Path) -> None:
    write_workflow(
        tmp_path,
        standard_workflow(
            [
                "aarch64-apple-darwin",
                "x86_64-unknown-linux-gnu",
                "aarch64-unknown-linux-gnu",
            ]
        ),
    )
    fork = Repository(RepoId("acme", "demo"), ("tool", "fork"))
    private = Repository(RepoId("acme", "demo"), ("private-tool",))

    assert _binary_release_standard_findings(tmp_path, fork) == []
    assert _binary_release_standard_findings(tmp_path, private) == []


def test_musl_image_accepts_ish_contract(tmp_path: Path) -> None:
    write_rust_executable(tmp_path)
    write_musl_dockerfile(tmp_path)
    repository = Repository(RepoId("acme", "demo"), ("tool",))

    assert _rust_musl_image_findings(tmp_path, repository) == []


def test_musl_image_rejects_glibc_builder(tmp_path: Path) -> None:
    write_rust_executable(tmp_path)
    (tmp_path / "Dockerfile").write_text(
        "FROM debian:12\n"
        "RUN apt-get install -y gcc\n"
        "RUN cargo build --release --target x86_64-unknown-linux-gnu\n",
        encoding="utf-8",
    )
    repository = Repository(RepoId("acme", "demo"), ("tool",))

    findings = _rust_musl_image_findings(tmp_path, repository)
    messages = [finding.message for finding in findings]

    assert any("Alpine" in message for message in messages)
    assert any("glibc" in message for message in messages)
    assert any("musl-cargo" in message for message in messages)


def test_musl_image_skips_missing_dockerfile_and_non_rust(tmp_path: Path) -> None:
    repository = Repository(RepoId("acme", "demo"), ("tool",))

    assert _rust_musl_image_findings(tmp_path, repository) == []

    (tmp_path / "Dockerfile").write_text("FROM alpine:3.24.1\n", encoding="utf-8")

    assert _rust_musl_image_findings(tmp_path, repository) == []
