from __future__ import annotations

from pathlib import Path

import pytest

from repomod.rules.binary_release import _binary_release_findings
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
        "        target: [x86_64-unknown-linux-gnu, aarch64-apple-darwin]\n"
        "    steps:\n"
        f"      - run: {command}\n"
        "      - run: zip dist/demo.zip demo\n"
        "  release:\n"
        "    env:\n"
        "      RELEASE_TAG: release-${{ github.sha }}\n"
        "    steps:\n"
        "      - run: gh release create \"$RELEASE_TAG\" dist/demo.zip --prerelease --target \"$GITHUB_SHA\"\n"
    )


@pytest.mark.parametrize(
    ("language", "manifest", "command"),
    [
        ("rust", "Cargo.toml", "cargo build --release --target x86_64-unknown-linux-gnu"),
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

    multi_tagged = Repository(tool.id, ("tool", language))
    findings = _binary_release_findings(tmp_path, multi_tagged)

    assert len(findings) == 1
    assert findings[0].rule == "binary-release-tags"
    assert "require exactly [tool]" in findings[0].message
    assert "remove " + language in findings[0].message


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
    assert "exactly [tool]" not in findings[0].message
