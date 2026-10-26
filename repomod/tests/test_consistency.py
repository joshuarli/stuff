from __future__ import annotations

import shutil
from pathlib import Path

import pytest

from repomod.git import run_git
from repomod.rules import rules
from repomod.rules.consistency import (
    _make_target_findings,
    _tracked_artifact_findings,
)
from repomod.rules.languages import _language_tag_findings
from repomod.types import RepoId, Repository


def repository(*tags: str) -> Repository:
    return Repository(RepoId("acme", "demo"), tuple(tags))


def initialize_repository(root: Path) -> None:
    root.mkdir(parents=True, exist_ok=True)
    run_git(["init", "--quiet"], cwd=root)


def test_language_tag_lint_matches_detected_source_against_catalog_tags(tmp_path: Path) -> None:
    missing_tag = tmp_path / "missing-tag"
    missing_tag.mkdir()
    (missing_tag / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "1.0.0"\n', encoding="utf-8"
    )
    source = missing_tag / "src" / "lib.rs"
    source.parent.mkdir()
    source.write_text("pub fn run() {}\n", encoding="utf-8")

    assert [
        (finding.rule, finding.message, finding.severity)
        for finding in _language_tag_findings(missing_tag, repository("library"))
    ] == [
        ("language-tags", "Rust source is present but the catalog has no rust tag", "warning")
    ]

    unobserved = tmp_path / "unobserved"
    unobserved.mkdir()
    (unobserved / "package.json").write_text('{"name":"demo"}\n', encoding="utf-8")

    assert [
        (finding.rule, finding.message, finding.severity)
        for finding in _language_tag_findings(unobserved, repository("extension", "typescript"))
    ] == [
        (
            "language-tags",
            "TypeScript tag is present but no matching source was detected",
            "warning",
        )
    ]

    src_layout = tmp_path / "src-layout"
    src_layout.mkdir()
    (src_layout / "pyproject.toml").write_text(
        '[project]\nname = "demo"\nversion = "1.0.0"\n', encoding="utf-8"
    )
    nested = src_layout / "src" / "demo" / "module.py"
    nested.parent.mkdir(parents=True)
    nested.write_text("def run(): pass\n", encoding="utf-8")

    assert [
        finding.message
        for finding in _language_tag_findings(src_layout, repository("library"))
    ] == ["Python source is present but the catalog has no python tag"]

    recognized = tmp_path / "recognized"
    fixture = Path(__file__).parent / "fixtures" / "binary_release" / "laputa-systems_srv"
    shutil.copytree(fixture, recognized)

    assert _language_tag_findings(recognized, repository("tool")) == []

    nested_build = tmp_path / "nested-build"
    fixture = Path(__file__).parent / "fixtures" / "languages" / "python_build"
    shutil.copytree(fixture, nested_build)
    build_repository = Repository(
        RepoId("joshuarli", "python-build"), ("build", "python", "rust")
    )

    assert _language_tag_findings(nested_build, build_repository) == []


@pytest.mark.parametrize(
    ("run", "makefile", "expected"),
    [
        (
            "jobs:\n  test:\n    steps:\n      - run: make test\n      - run: make missing\n",
            ".PHONY: test\ntest:\n\t@echo ok\n",
            [
                (
                    ".github/workflows/test.yml",
                    "workflow runs make target 'missing', but the root Makefile does not define it",
                )
            ],
        ),
        (
            "jobs:\n  test:\n    steps:\n      - run: \"make missing\"\n",
            "test:\n\t@echo ok\n",
            [(".github/workflows/test.yml", "workflow runs make target 'missing'")],
        ),
        (
            "jobs:\n  test:\n    steps:\n      - run: make test\n",
            None,
            [(".github/workflows/test.yml", "no root Makefile")],
        ),
        (
            "jobs:\n  test:\n    steps:\n      - run: make \"${TARGET}\"\n",
            "include targets.mk\n",
            [],
        ),
        (
            "jobs:\n  test:\n    steps:\n      - run: make release TARGET=${{ matrix.target }}\n",
            ".PHONY: release\nrelease:\n\t@echo ok\n",
            [],
        ),
        (
            "jobs:\n  test:\n    steps:\n      - run: make ${{ matrix.target }}\n",
            ".PHONY: release\nrelease:\n\t@echo ok\n",
            [],
        ),
    ],
)
def test_workflow_make_target_lint_checks_literal_root_targets(
    tmp_path: Path, run: str, makefile: str | None, expected: list[tuple[str, str]]
) -> None:
    workflow = tmp_path / ".github" / "workflows" / "test.yml"
    workflow.parent.mkdir(parents=True)
    workflow.write_text(run, encoding="utf-8")
    if makefile is not None:
        (tmp_path / "Makefile").write_text(makefile, encoding="utf-8")

    findings = _make_target_findings(tmp_path, repository("tool"))

    assert len(findings) == len(expected)
    for finding, (expected_file, expected_message) in zip(findings, expected, strict=True):
        assert finding.file == expected_file
        assert expected_message in finding.message


def test_generated_artifact_lint_reports_tracked_outputs_and_missing_composed_ignores(
    tmp_path: Path,
) -> None:
    initialize_repository(tmp_path)
    (tmp_path / ".gitignore").write_text(".DS_Store\nThumbs.db\n", encoding="utf-8")
    (tmp_path / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "1.0.0"\n', encoding="utf-8"
    )
    source = tmp_path / "src" / "lib.rs"
    source.parent.mkdir()
    source.write_text("pub fn run() {}\n", encoding="utf-8")
    generated = tmp_path / "target" / "compiled"
    generated.parent.mkdir()
    generated.write_text("artifact\n", encoding="utf-8")
    run_git(["add", ".gitignore", "Cargo.toml", "src/lib.rs"], cwd=tmp_path)
    run_git(["add", "--force", "target/compiled"], cwd=tmp_path)

    findings = _tracked_artifact_findings(tmp_path, repository("tool"))

    assert [(finding.file, finding.severity) for finding in findings] == [
        ("target/compiled", "error"),
        (".gitignore", "warning"),
    ]
    assert "target/" in findings[1].message


def test_generated_artifact_lint_accepts_composed_profiles_for_untagged_rust(
    tmp_path: Path,
) -> None:
    initialize_repository(tmp_path)
    (tmp_path / ".gitignore").write_text(
        ".DS_Store\nThumbs.db\ntarget/\n", encoding="utf-8"
    )
    (tmp_path / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "1.0.0"\n', encoding="utf-8"
    )
    source = tmp_path / "src" / "main.rs"
    source.parent.mkdir()
    source.write_text("fn main() {}\n", encoding="utf-8")
    run_git(["add", ".gitignore", "Cargo.toml", "src/main.rs"], cwd=tmp_path)

    assert _tracked_artifact_findings(tmp_path, repository("tool")) == []


def test_generated_artifact_lint_does_not_treat_a_python_target_directory_as_rust_output(
    tmp_path: Path,
) -> None:
    initialize_repository(tmp_path)
    (tmp_path / ".gitignore").write_text(
        ".DS_Store\nThumbs.db\n.venv/\n__pycache__/\n.pytest_cache/\n.ruff_cache/\n.mypy_cache/\n.tox/\n",
        encoding="utf-8",
    )
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "demo"\nversion = "1.0.0"\n', encoding="utf-8"
    )
    source = tmp_path / "src" / "demo.py"
    source.parent.mkdir()
    source.write_text("def run(): pass\n", encoding="utf-8")
    target = tmp_path / "target" / "metadata.txt"
    target.parent.mkdir()
    target.write_text("maintained content\n", encoding="utf-8")
    run_git(["add", ".gitignore", "pyproject.toml", "src/demo.py"], cwd=tmp_path)
    run_git(["add", "--force", "target/metadata.txt"], cwd=tmp_path)

    assert _tracked_artifact_findings(tmp_path, repository("tool", "python")) == []


def test_generated_artifact_lint_requires_root_ignore_rules(tmp_path: Path) -> None:
    initialize_repository(tmp_path)
    (tmp_path / ".gitignore").write_text(
        ".DS_Store\nThumbs.db\n__pycache__/\n.pytest_cache/\n.ruff_cache/\n.mypy_cache/\n.tox/\n",
        encoding="utf-8",
    )
    nested_ignore = tmp_path / ".venv" / ".gitignore"
    nested_ignore.parent.mkdir()
    nested_ignore.write_text("*\n", encoding="utf-8")
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "demo"\nversion = "1.0.0"\n', encoding="utf-8"
    )
    source = tmp_path / "src" / "demo.py"
    source.parent.mkdir()
    source.write_text("def run(): pass\n", encoding="utf-8")
    run_git(["add", ".gitignore", "pyproject.toml", "src/demo.py"], cwd=tmp_path)

    findings = _tracked_artifact_findings(tmp_path, repository("tool", "python"))

    assert len(findings) == 1
    assert ".venv/" in findings[0].message


def test_generated_artifact_lint_does_not_follow_a_root_gitignore_symlink(
    tmp_path: Path,
) -> None:
    initialize_repository(tmp_path)
    outside = tmp_path.parent / "outside.gitignore"
    outside.write_text("target/\n", encoding="utf-8")
    (tmp_path / ".gitignore").symlink_to(outside)
    (tmp_path / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "1.0.0"\n', encoding="utf-8"
    )
    source = tmp_path / "src" / "main.rs"
    source.parent.mkdir()
    source.write_text("fn main() {}\n", encoding="utf-8")

    findings = _tracked_artifact_findings(tmp_path, repository("tool"))

    assert len(findings) == 1
    assert "target/" in findings[0].message


def test_consistency_and_language_lints_are_registered() -> None:
    rule_ids = {rule.id for rule in rules()}

    assert {"mdtool", "generated-artifacts", "language-tags", "workflow-make-targets"} <= rule_ids
