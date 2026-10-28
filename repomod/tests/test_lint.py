from __future__ import annotations

from pathlib import Path

from repomod.policy import validate_ignored_rules
from repomod.rules import rules
from repomod.rules.lint import (
    _gha_lint_workflow_findings,
    _lint_workflow_fix,
    _make_lint_target_findings,
)
from repomod.types import RepoId, Repository


def repository(*tags: str) -> Repository:
    return Repository(RepoId("acme", "demo"), tuple(tags))


def apply_plan(root: Path, plan: dict[str, str]) -> None:
    for relative, contents in plan.items():
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents, encoding="utf-8")


def test_missing_lint_workflow_is_fixable_and_template_runs_make_lint(
    tmp_path: Path,
) -> None:
    project = repository("tool")
    findings = _gha_lint_workflow_findings(tmp_path, project)

    assert [(item.rule, item.file, item.fixable) for item in findings] == [
        ("gha-lint-workflow", ".github/workflows/lint.yml", True)
    ]

    plan = _lint_workflow_fix(tmp_path, project)
    assert set(plan) == {".github/workflows/lint.yml"}
    apply_plan(tmp_path, plan)

    assert _gha_lint_workflow_findings(tmp_path, project) == []
    assert _lint_workflow_fix(tmp_path, project) == {}

    rendered = (tmp_path / ".github" / "workflows" / "lint.yml").read_text(encoding="utf-8")
    assert "run: make lint" in rendered
    assert "actions/checkout@" in rendered


def test_lint_workflow_without_make_lint_needs_manual_repair(tmp_path: Path) -> None:
    workflow = tmp_path / ".github" / "workflows" / "lint.yml"
    workflow.parent.mkdir(parents=True)
    workflow.write_text(
        "name: lint\non:\n  push:\n    branches: [main]\njobs:\n  lint:\n    runs-on: ubuntu-latest\n"
        "    steps:\n      - run: cargo clippy\n",
        encoding="utf-8",
    )
    project = repository("tool")

    findings = _gha_lint_workflow_findings(tmp_path, project)

    assert len(findings) == 1
    assert findings[0].fixable is False
    assert "must run make lint" in findings[0].message
    assert _lint_workflow_fix(tmp_path, project) == {}


def test_lint_workflow_symlink_is_operational(tmp_path: Path) -> None:
    workflows = tmp_path / ".github" / "workflows"
    workflows.mkdir(parents=True)
    outside = tmp_path.parent / "outside-lint.yml"
    outside.write_text("name: lint\n", encoding="utf-8")
    (workflows / "lint.yml").symlink_to(outside)

    findings = _gha_lint_workflow_findings(tmp_path, repository("tool"))

    assert len(findings) == 1
    assert findings[0].kind == "operational"


def test_missing_makefile_needs_a_manual_lint_target(tmp_path: Path) -> None:
    findings = _make_lint_target_findings(tmp_path, repository("tool"))

    assert [(item.rule, item.file, item.fixable) for item in findings] == [
        ("make-lint-target", "Makefile", False)
    ]


def test_makefile_without_lint_target_is_reported(tmp_path: Path) -> None:
    (tmp_path / "Makefile").write_text(".PHONY: test\ntest:\n\t@echo ok\n", encoding="utf-8")

    findings = _make_lint_target_findings(tmp_path, repository("tool"))

    assert len(findings) == 1
    assert "does not define a lint target" in findings[0].message
    assert findings[0].fixable is False


def test_makefile_with_lint_target_passes(tmp_path: Path) -> None:
    (tmp_path / "Makefile").write_text(
        ".PHONY: lint\nlint:\n\tcargo clippy -- --deny warnings\n", encoding="utf-8"
    )

    assert _make_lint_target_findings(tmp_path, repository("tool")) == []


def test_lint_rules_are_registered_and_blacklistable() -> None:
    rule_ids = {rule.id for rule in rules()}

    assert {"gha-lint-workflow", "make-lint-target"} <= rule_ids

    ignored = Repository(
        RepoId("acme", "demo"),
        ("tool",),
        ignored_rules=frozenset({"gha-lint-workflow", "make-lint-target"}),
    )
    validate_ignored_rules((ignored,))
