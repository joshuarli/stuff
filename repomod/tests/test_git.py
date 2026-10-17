"""Tests for PR handling in git.py."""

import json
import subprocess
from pathlib import Path
from typing import Any
from unittest.mock import patch

import pytest

from repomod import git
from repomod.types import PRInfo, PRSpec, RolloutPR
from repomod.utils import ShutdownError, interrupt

PR = PRSpec(
    repo_name="acme/backend",
    branch="macos-15-runners",
    title="ci: upgrade runners",
    body="body",
    reviewers=[],
)
URL = "https://github.com/acme/backend/pull/7"


def _repo_with_change(tmp_path: Path) -> Path:
    repo = tmp_path / "backend"
    repo.mkdir()
    for args in (
        ["init", "--initial-branch=main"],
        ["config", "user.email", "test@test.com"],
        ["config", "user.name", "Test"],
    ):
        subprocess.run(["git", *args], cwd=repo, check=True, capture_output=True)
    (repo / "ci.yml").write_text("runs-on: macos-14\n")
    subprocess.run(["git", "add", "."], cwd=repo, check=True, capture_output=True)
    subprocess.run(
        ["git", "commit", "-m", "init"], cwd=repo, check=True, capture_output=True
    )
    (repo / "ci.yml").write_text("runs-on: macos-15\n")
    return repo


def _known(state: str) -> RolloutPR:
    return RolloutPR("acme/backend", URL, state, False)


def test_merged_or_closed_pr_is_returned_without_touching_the_repo(
    tmp_path: Path,
) -> None:
    repo = _repo_with_change(tmp_path)

    for state in ("MERGED", "CLOSED"):
        with patch.object(git, "push_and_open_pr") as push:
            assert git.commit_and_push(repo, PR, _known(state)) == PRInfo(URL, state)
        push.assert_not_called()

    branch = subprocess.run(
        ["git", "branch", "--show-current"], cwd=repo, capture_output=True, text=True
    ).stdout.strip()
    assert branch == "main"


def test_git_and_gh_start_in_a_new_session(tmp_path: Path) -> None:
    """SIGINT hits the foreground process group, so git and gh must leave it."""
    interrupt.event.clear()
    completed = subprocess.CompletedProcess(["git"], 0, stdout="", stderr="")
    with patch("repomod.git.subprocess.run", return_value=completed) as run:
        git.git_run(["status"], cwd=tmp_path)
        git.gh_run(["auth", "status"])

    assert run.call_args_list[0].args[0][0] == "git"
    assert run.call_args_list[0].kwargs.get("start_new_session") is True
    assert run.call_args_list[1].args[0][0] == "gh"
    assert run.call_args_list[1].kwargs.get("start_new_session") is True


def test_git_run_child_error_during_interrupt_is_shutdown(tmp_path: Path) -> None:
    interrupt.event.clear()

    def killed(*_args: object, **_kwargs: object) -> subprocess.CompletedProcess[str]:
        interrupt.event.set()
        raise subprocess.CalledProcessError(-2, ["git", "add"])

    try:
        with (
            patch("repomod.git.subprocess.run", side_effect=killed),
            pytest.raises(ShutdownError),
        ):
            git.git_run(["add", "--all"], cwd=tmp_path)
    finally:
        interrupt.event.clear()


def test_commit_and_push_stops_before_the_next_git_command_when_interrupted(
    tmp_path: Path,
) -> None:
    """An in-flight git command may finish; ^C must win before the next one."""
    repo = _repo_with_change(tmp_path)
    interrupt.event.clear()
    commands: list[list[str]] = []
    original = subprocess.run

    def side_effect(
        cmd: list[str], **kwargs: Any
    ) -> subprocess.CompletedProcess[Any]:
        commands.append(list(cmd))
        result = original(cmd, **kwargs)
        if list(cmd)[:2] == ["git", "add"]:
            interrupt.event.set()
        return result

    try:
        with (
            patch("repomod.git.subprocess.run", side_effect=side_effect),
            patch.object(git, "push_and_open_pr") as push,
            pytest.raises(ShutdownError),
        ):
            git.commit_and_push(repo, PR, None)
    finally:
        interrupt.event.clear()

    push.assert_not_called()
    assert ["git", "add", "--all"] in commands
    assert ["git", "commit", "--quiet", "--message", PR.title] not in commands


def test_pr_by_repo_prefers_the_open_pr_over_an_old_closed_one() -> None:
    old = RolloutPR(
        "acme/backend", "https://github.com/acme/backend/pull/1", "CLOSED", False
    )
    current = RolloutPR("acme/backend", URL, "OPEN", False)

    assert git.pr_by_repo([old, current])["acme/backend"] == current
    assert git.pr_by_repo([current, old])["acme/backend"] == current


def _search_result(*items: dict[str, Any]) -> subprocess.CompletedProcess[str]:
    return subprocess.CompletedProcess(
        [], 0, stdout="\n".join(json.dumps(i) for i in items), stderr=""
    )


def _item(
    repo: str, n: int, state: str, merged_at: str | None, draft: bool
) -> dict[str, Any]:
    return {
        "node_id": f"PR_kwDO{repo}{n}",
        "html_url": f"https://github.com/acme/{repo}/pull/{n}",
        "repository_url": f"https://api.github.com/repos/acme/{repo}",
        "state": state,
        "draft": draft,
        "pull_request": {"merged_at": merged_at},
    }


def test_search_prs_classifies_open_merged_and_closed() -> None:
    result = _search_result(
        _item("backend", 1, "open", None, draft=True),
        _item("frontend", 2, "closed", "2026-09-25T08:47:48Z", draft=False),
        _item("worker", 3, "closed", None, draft=False),
    )

    with patch.object(git, "gh_run", return_value=result):
        prs = git.search_prs("macos-15-runners")

    assert [(p.repo, p.state, p.draft, p.node_id) for p in prs] == [
        ("acme/backend", "OPEN", True, "PR_kwDObackend1"),
        ("acme/frontend", "MERGED", False, "PR_kwDOfrontend2"),
        ("acme/worker", "CLOSED", False, "PR_kwDOworker3"),
    ]


def test_search_prs_with_no_results() -> None:
    with patch.object(git, "gh_run", return_value=_search_result()):
        assert git.search_prs("macos-15-runners") == []
