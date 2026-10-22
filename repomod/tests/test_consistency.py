from __future__ import annotations

import shutil
from pathlib import Path

import repomod.git as git_module
from repomod.git import run_git
from repomod.rules import rules
from repomod.rules.consistency import (
    _make_target_findings,
    _markdown_link_findings,
    _tracked_artifact_findings,
)
from repomod.rules.languages import _language_tag_findings
from repomod.types import RepoId, Repository


def repository(*tags: str) -> Repository:
    return Repository(RepoId("acme", "demo"), tuple(tags))


def initialize_repository(root: Path) -> None:
    root.mkdir(parents=True, exist_ok=True)
    run_git(["init", "--quiet"], cwd=root)


def test_markdown_links_reports_missing_local_targets_and_ignores_external_links(
    tmp_path: Path,
) -> None:
    (tmp_path / "README.md").write_text(
        "[existing](docs/guide.md) [missing](docs/missing.md) [web](https://example.com)\n",
        encoding="utf-8",
    )
    (tmp_path / "docs").mkdir()
    (tmp_path / "docs" / "guide.md").write_text("# Guide\n", encoding="utf-8")

    findings = _markdown_link_findings(tmp_path, repository("tool"))

    assert [(finding.file, finding.message) for finding in findings] == [
        ("README.md", "local Markdown link target does not exist: docs/missing.md")
    ]


def test_markdown_links_reports_paths_that_escape_the_repository(tmp_path: Path) -> None:
    root = tmp_path / "repo"
    root.mkdir()
    (root / "README.md").write_text("[outside](../secret.md)\n", encoding="utf-8")

    findings = _markdown_link_findings(root, repository("tool"))

    assert len(findings) == 1
    assert "escapes the repository" in findings[0].message


def test_markdown_links_checks_reference_links_and_skips_code(tmp_path: Path) -> None:
    (tmp_path / "README.md").write_text(
        "[reference][guide] `[inline](missing.md)`\n\n"
        "```md\n[example](also-missing.md)\n```\n\n"
        "<!-- [comment](comment-missing.md) -->\n\n"
        "[guide]: docs/missing.md\n",
        encoding="utf-8",
    )

    findings = _markdown_link_findings(tmp_path, repository("tool"))

    assert len(findings) == 1
    assert findings[0].message == "local Markdown link target does not exist: docs/missing.md"


def test_language_tag_lint_reports_clear_source_without_tag(tmp_path: Path) -> None:
    manifest = tmp_path / "Cargo.toml"
    manifest.write_text('[package]\nname = "demo"\nversion = "1.0.0"\n', encoding="utf-8")
    source = tmp_path / "src" / "lib.rs"
    source.parent.mkdir()
    source.write_text("pub fn run() {}\n", encoding="utf-8")

    findings = _language_tag_findings(tmp_path, repository("library"))

    assert [(finding.rule, finding.message, finding.severity) for finding in findings] == [
        ("language-tags", "Rust source is present but the catalog has no rust tag", "warning")
    ]


def test_language_tag_lint_reports_catalog_language_without_detected_source(
    tmp_path: Path,
) -> None:
    (tmp_path / "package.json").write_text('{"name":"demo"}\n', encoding="utf-8")

    findings = _language_tag_findings(tmp_path, repository("extension", "typescript"))

    assert [(finding.rule, finding.message, finding.severity) for finding in findings] == [
        (
            "language-tags",
            "TypeScript tag is present but no matching source was detected",
            "warning",
        )
    ]


def test_language_tag_lint_detects_python_source_in_standard_src_package_layout(
    tmp_path: Path,
) -> None:
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "demo"\nversion = "1.0.0"\n', encoding="utf-8"
    )
    source = tmp_path / "src" / "demo" / "module.py"
    source.parent.mkdir(parents=True)
    source.write_text("def run(): pass\n", encoding="utf-8")

    findings = _language_tag_findings(tmp_path, repository("library"))

    assert [finding.message for finding in findings] == [
        "Python source is present but the catalog has no python tag"
    ]


def test_language_tag_lint_exempts_recognized_tool_only_binary_release(tmp_path: Path) -> None:
    fixture = Path(__file__).parent / "fixtures" / "binary_release" / "laputa-systems_srv"
    shutil.copytree(fixture, tmp_path, dirs_exist_ok=True)

    assert _language_tag_findings(tmp_path, repository("tool")) == []


def test_language_tag_lint_accepts_nested_python_build_sources(tmp_path: Path) -> None:
    fixture = Path(__file__).parent / "fixtures" / "languages" / "python_build"
    shutil.copytree(fixture, tmp_path, dirs_exist_ok=True)
    build_repository = Repository(
        RepoId("joshuarli", "python-build"), ("build", "python", "rust")
    )

    assert _language_tag_findings(tmp_path, build_repository) == []


def test_workflow_make_target_lint_checks_literal_root_targets(tmp_path: Path) -> None:
    workflow = tmp_path / ".github" / "workflows" / "test.yml"
    workflow.parent.mkdir(parents=True)
    workflow.write_text(
        "jobs:\n  test:\n    steps:\n      - run: make test\n      - run: make missing\n",
        encoding="utf-8",
    )
    (tmp_path / "Makefile").write_text(".PHONY: test\ntest:\n\t@echo ok\n", encoding="utf-8")

    findings = _make_target_findings(tmp_path, repository("tool"))

    assert [(finding.file, finding.message) for finding in findings] == [
        (".github/workflows/test.yml", "workflow runs make target 'missing', but the root Makefile does not define it")
    ]


def test_workflow_make_target_lint_reads_quoted_yaml_run_values(tmp_path: Path) -> None:
    workflow = tmp_path / ".github" / "workflows" / "test.yml"
    workflow.parent.mkdir(parents=True)
    workflow.write_text("jobs:\n  test:\n    steps:\n      - run: \"make missing\"\n", encoding="utf-8")
    (tmp_path / "Makefile").write_text("test:\n\t@echo ok\n", encoding="utf-8")

    findings = _make_target_findings(tmp_path, repository("tool"))

    assert len(findings) == 1
    assert "make target 'missing'" in findings[0].message


def test_workflow_make_target_lint_reports_missing_root_makefile(tmp_path: Path) -> None:
    workflow = tmp_path / ".github" / "workflows" / "test.yml"
    workflow.parent.mkdir(parents=True)
    workflow.write_text("jobs:\n  test:\n    steps:\n      - run: make test\n", encoding="utf-8")

    findings = _make_target_findings(tmp_path, repository("tool"))

    assert len(findings) == 1
    assert "no root Makefile" in findings[0].message


def test_workflow_make_target_lint_ignores_dynamic_targets_and_included_makefiles(
    tmp_path: Path,
) -> None:
    workflow = tmp_path / ".github" / "workflows" / "test.yml"
    workflow.parent.mkdir(parents=True)
    workflow.write_text("jobs:\n  test:\n    steps:\n      - run: make \"${TARGET}\"\n", encoding="utf-8")
    (tmp_path / "Makefile").write_text("include targets.mk\n", encoding="utf-8")

    assert _make_target_findings(tmp_path, repository("tool")) == []


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


def test_generated_artifact_ignore_probes_use_one_git_process(
    tmp_path: Path, monkeypatch
) -> None:
    initialize_repository(tmp_path)
    (tmp_path / ".gitignore").write_text(".DS_Store\nThumbs.db\n", encoding="utf-8")
    original_run_git = git_module.run_git
    check_ignore_calls = []

    def record_run_git(*args, **kwargs):
        if args[0][0] == "check-ignore":
            check_ignore_calls.append(kwargs["input_text"])
        return original_run_git(*args, **kwargs)

    monkeypatch.setattr(git_module, "run_git", record_run_git)

    findings = _tracked_artifact_findings(tmp_path, repository("tool"))

    assert findings == []
    assert check_ignore_calls == [".DS_Store\0Thumbs.db\0"]


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
    outside.write_text(".DS_Store\nThumbs.db\n", encoding="utf-8")
    (tmp_path / ".gitignore").symlink_to(outside)

    findings = _tracked_artifact_findings(tmp_path, repository("tool"))

    assert len(findings) == 1
    assert ".DS_Store" in findings[0].message
    assert "Thumbs.db" in findings[0].message


def test_consistency_and_language_lints_are_registered() -> None:
    rule_ids = {rule.id for rule in rules()}

    assert {"markdown-links", "generated-artifacts", "language-tags", "workflow-make-targets"} <= rule_ids
