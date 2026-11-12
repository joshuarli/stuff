from __future__ import annotations

from pathlib import Path

from repomod.rules.workflow_redundancy import _redundancy_findings
from repomod.types import RepoId, Repository

LINT = """name: lint
on:
  pull_request:
  push:
    branches: [main]
jobs:
  lint:
    runs-on: ubuntu-latest
    steps:
      - run: make lint
"""


def repository(*tags: str) -> Repository:
    return Repository(RepoId("acme", "demo"), tuple(tags))


def write(root: Path, name: str, source: str) -> None:
    path = root / ".github" / "workflows" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(source, encoding="utf-8")


def workflow(on: str, steps: str) -> str:
    return f"name: CI\n{on}\njobs:\n  checks:\n    runs-on: ubuntu-latest\n    steps:\n{steps}"


def messages(root: Path, *tags: str) -> list[tuple[str, str | None]]:
    return [(item.rule, item.file) for item in _redundancy_findings(root, repository(*tags))]


def test_duplicate_make_lint_on_shared_event_is_flagged(tmp_path: Path) -> None:
    write(tmp_path, "lint.yml", LINT)
    triggers = "on:\n  push:\n    branches: [main]\n  pull_request:\n"
    write(tmp_path, "ci.yml", workflow(triggers, "      - run: make lint\n      - run: make test\n"))

    findings = _redundancy_findings(tmp_path, repository())

    assert [(item.rule, item.file, item.fixable) for item in findings] == [
        ("workflow-redundancy", ".github/workflows/ci.yml", False)
    ]
    assert "pull_request, push" in findings[0].message


def test_flow_and_block_scalar_run_forms_are_detected(tmp_path: Path) -> None:
    write(tmp_path, "lint.yaml", LINT)
    block = "      - run: |\n          uv sync\n          make -j2 lint\n"
    write(tmp_path, "a.yml", workflow("on: [push, release]", block))
    write(tmp_path, "b.yml", workflow("on: pull_request", "      - run: uv sync && make lint # again\n"))

    assert messages(tmp_path) == [
        ("workflow-redundancy", ".github/workflows/a.yml"),
        ("workflow-redundancy", ".github/workflows/b.yml"),
    ]


def test_non_overlapping_events_do_not_trigger(tmp_path: Path) -> None:
    write(tmp_path, "lint.yml", LINT)
    triggers = "on:\n  schedule:\n    - cron: '0 0 * * *'\n  workflow_dispatch:\n"
    write(tmp_path, "nightly.yml", workflow(triggers, "      - run: make lint\n"))

    assert messages(tmp_path) == []


def test_lookalikes_are_not_make_lint_runs(tmp_path: Path) -> None:
    write(tmp_path, "lint.yml", LINT)
    steps = "      - name: make lint\n        run: make lint-fix\n      - run: echo make lint\n"
    write(tmp_path, "ci.yml", workflow("on: [push, pull_request]", steps))

    assert messages(tmp_path) == []


def test_without_a_standard_lint_workflow_nothing_is_flagged(tmp_path: Path) -> None:
    write(tmp_path, "ci.yml", workflow("on: [push]", "      - run: make lint\n"))
    write(tmp_path, "lint.yml", LINT.replace("make lint", "make check"))
    assert messages(tmp_path) == []
    assert messages(tmp_path / "empty") == []


def test_fork_repositories_only_inspect_fork_workflows(tmp_path: Path) -> None:
    write(tmp_path, "lint.yml", LINT)
    write(tmp_path, "ci.yml", workflow("on: [push]", "      - run: make lint\n"))

    assert messages(tmp_path, "fork") == []
