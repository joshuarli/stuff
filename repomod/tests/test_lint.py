"""Tests for actionlint validation of fix output."""

import shutil
import subprocess
from pathlib import Path
from unittest.mock import patch

import pytest

from repomod import lint
from repomod.types import LintFinding

CI = ".github/workflows/ci.yml"
WORKFLOW = (
    "on: push\njobs:\n  build:\n    runs-on: macos-14\n"
    "    steps:\n      - run: echo ${{ %s }}\n"
)


def _git(repo: Path, *args: str) -> None:
    subprocess.run(["git", *args], cwd=repo, check=True, capture_output=True)


def _repo_with_workflow(tmp_path: Path, expr: str = "github.sha") -> Path:
    repo = tmp_path / "acme-backend"
    (repo / ".github" / "workflows").mkdir(parents=True)
    (repo / CI).write_text(WORKFLOW % expr)
    (repo / "README.md").write_text("hello")
    _git(repo, "init", "--initial-branch=main")
    _git(repo, "config", "user.email", "test@test.com")
    _git(repo, "config", "user.name", "Test")
    _git(repo, "add", ".")
    _git(repo, "commit", "-m", "init")
    return repo


def test_only_errors_introduced_by_the_fix_are_reported(tmp_path: Path) -> None:
    repo = _repo_with_workflow(tmp_path)
    (repo / CI).write_text(WORKFLOW.replace("macos-14", "macos-15") % "github.sha")
    _git(repo, "add", "--all")
    existing = LintFinding(CI, "shellcheck: pre-existing")
    introduced = LintFinding(CI, "unknown runner label")

    with patch.object(
        lint, "run_actionlint", side_effect=[{existing, introduced}, {existing}]
    ):
        assert lint.new_workflow_findings(repo) == {introduced}


def test_baseline_is_linted_on_the_original_and_changes_are_restored(
    tmp_path: Path,
) -> None:
    repo = _repo_with_workflow(tmp_path)
    changed = WORKFLOW.replace("macos-14", "macos-15") % "github.sha"
    (repo / CI).write_text(changed)
    _git(repo, "add", "--all")
    linted: list[str] = []

    def fake_actionlint(repo_dir: Path, files: list[str]) -> set[LintFinding]:
        linted.append((repo_dir / CI).read_text())
        return {LintFinding(CI, "boom")}

    with patch.object(lint, "run_actionlint", side_effect=fake_actionlint):
        lint.new_workflow_findings(repo)

    assert linted == [changed, WORKFLOW % "github.sha"]
    assert (repo / CI).read_text() == changed


def test_a_new_workflow_is_reported_without_linting_a_missing_baseline(
    tmp_path: Path,
) -> None:
    """A workflow the fix adds has no previous copy. actionlint must not be asked
    to open that path while the addition is stashed."""
    repo = _repo_with_workflow(tmp_path)
    new = ".github/workflows/deploy.yml"
    (repo / new).write_text("name: deploy\n")
    _git(repo, "add", "--all")
    introduced = LintFinding(new, "unknown action")
    seen: list[list[str]] = []

    def fake(repo_dir: Path, files: list[str]) -> set[LintFinding]:
        seen.append(files)
        if not (repo_dir / new).exists():
            msg = "baseline linted a workflow the fix added"
            raise AssertionError(msg)
        return {introduced}

    with patch.object(lint, "run_actionlint", side_effect=fake):
        assert lint.new_workflow_findings(repo) == {introduced}

    assert seen == [[new]]
    assert (repo / new).read_text() == "name: deploy\n"
    staged = subprocess.run(
        ["git", "diff", "--cached", "--name-only"],
        cwd=repo,
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    assert staged.splitlines() == [new]


def test_non_workflow_changes_are_not_linted(tmp_path: Path) -> None:
    repo = _repo_with_workflow(tmp_path)
    (repo / "README.md").write_text("changed")

    with patch.object(lint, "run_actionlint") as run_actionlint:
        assert lint.new_workflow_findings(repo) == set()

    run_actionlint.assert_not_called()


@pytest.mark.skipif(shutil.which("actionlint") is None, reason="needs actionlint")
def test_real_actionlint_flags_only_new_errors(tmp_path: Path) -> None:
    repo = _repo_with_workflow(tmp_path, expr="foo.bar")
    (repo / CI).write_text(WORKFLOW.replace("macos-14", "macos-15") % "foo.bar")
    _git(repo, "add", "--all")
    assert lint.new_workflow_findings(repo) == set()

    broken = WORKFLOW.replace("macos-14", "macos-15") % "foo.bar"
    (repo / CI).write_text(broken + "      - run: echo ${{ baz.qux }}\n")
    _git(repo, "add", "--all")
    findings = lint.new_workflow_findings(repo)
    assert [f.path for f in findings] == [CI]
    assert any("baz" in f.message for f in findings)
