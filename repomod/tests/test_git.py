"""Tests for PR handling in git.py."""

import json
import subprocess
from collections.abc import Callable, Generator
from contextlib import contextmanager
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
    subprocess.run(["git", "add", "--all"], cwd=repo, check=True, capture_output=True)
    interrupt.event.clear()
    commands: list[list[str]] = []
    original = subprocess.run

    def side_effect(
        cmd: list[str], **kwargs: Any
    ) -> subprocess.CompletedProcess[Any]:
        commands.append(list(cmd))
        result = original(cmd, **kwargs)
        if list(cmd)[:4] == ["git", "diff", "--cached", "--quiet"]:
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
    assert ["git", "diff", "--cached", "--quiet"] in commands
    assert ["git", "commit", "--quiet", "--message", PR.title] not in commands


def test_get_diff_returns_the_tracked_diff_when_git_succeeds(tmp_path: Path) -> None:
    """A non-empty staged diff is success.

    git diff --cached without --exit-code exits 0. An unstaged edit is not it.
    """
    repo = _repo_with_change(tmp_path)
    assert git.get_diff(repo) == ""

    subprocess.run(["git", "add", "--all"], cwd=repo, check=True, capture_output=True)
    diff = git.get_diff(repo)

    assert "diff --git" in diff
    assert "macos-15" in diff


def test_get_diff_raises_when_git_exits_nonzero(tmp_path: Path) -> None:
    """A failed git diff must not come back as patch text, even if stdout is empty."""
    repo = _repo_with_change(tmp_path)
    (repo / ".git" / "index").write_bytes(b"not an index")

    with pytest.raises(subprocess.CalledProcessError):
        git.get_diff(repo)


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


REPO = "acme/backend"
BRANCH = "main"
WORKFLOW = ".github/workflows/ci.yml"
_GITHUB = f"https://github.com/{REPO}.git"


class _CloneSpy:
    def __init__(self) -> None:
        self.commands: list[list[str]] = []
        self.dest_existed_before_retry: list[bool] = []


def _run_git(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args], cwd=cwd, check=True, capture_output=True, text=True
    )


def _bare_repo(tmp_path: Path) -> Path:
    """Local origin with a workflow, a root file, and a file outside both."""
    seed = tmp_path / "seed"
    (seed / ".github" / "workflows").mkdir(parents=True)
    (seed / ".github" / "workflows" / "ci.yml").write_text("name: ci\n")
    (seed / "README.md").write_text("hello\n")
    (seed / "src").mkdir()
    (seed / "src" / "main.py").write_text("print('hi')\n")
    for args in (
        ["init", "--initial-branch=main"],
        ["config", "user.email", "test@test.com"],
        ["config", "user.name", "Test"],
        ["add", "."],
        ["commit", "-m", "init"],
    ):
        _run_git(seed, *args)
    bare = tmp_path / "origin.git"
    _run_git(tmp_path, "clone", "--bare", str(seed), str(bare))
    return bare


def _clone_local(
    bare: Path, dest: Path, *, sparse: bool, cone: list[str] | None = None
) -> None:
    cmd = ["clone"]
    if sparse:
        cmd.append("--sparse")
    _run_git(bare.parent, *cmd, str(bare), str(dest))
    if cone is not None:
        _run_git(dest, "sparse-checkout", "set", *cone)


@contextmanager
def _offline_clone(
    bare: Path,
    dest: Path,
    fail: Callable[[list[str]], bool] | None = None,
) -> Generator[_CloneSpy]:
    """Rewrite GitHub clone URLs to `bare` and skip retry sleeps."""
    original = subprocess.run
    spy = _CloneSpy()

    def side_effect(cmd: Any, **kwargs: Any) -> subprocess.CompletedProcess[Any]:
        argv = [str(part) for part in cmd]
        spy.commands.append(argv)
        if fail is not None and fail(argv):
            raise subprocess.CalledProcessError(1, argv, stderr="git failed")
        rewritten = [str(bare) if part == _GITHUB else part for part in argv]
        return original(rewritten, **kwargs)

    def wait(_seconds: float) -> None:
        spy.dest_existed_before_retry.append(dest.exists())

    with (
        patch("repomod.git.subprocess.run", side_effect=side_effect),
        patch("repomod.utils.interrupt.wait", side_effect=wait),
    ):
        yield spy


def _git_subcommand(argv: list[str], name: str) -> bool:
    return len(argv) >= 2 and argv[0] == "git" and argv[1] == name


def test_clone_checks_out_the_workflow_when_the_sparse_cone_was_never_set(
    tmp_path: Path,
) -> None:
    """A sparse clone with no cone still has the workflow in the index only."""
    bare = _bare_repo(tmp_path)
    dest = tmp_path / "backend"
    _clone_local(bare, dest, sparse=True)
    assert WORKFLOW in _run_git(dest, "ls-files").stdout.splitlines()
    assert not (dest / WORKFLOW).exists()

    with _offline_clone(bare, dest):
        git.clone(REPO, BRANCH, dest, [".github"])

    assert WORKFLOW in _run_git(dest, "ls-files").stdout.splitlines()
    assert (dest / WORKFLOW).is_file()


def test_clone_does_not_keep_a_dest_whose_cone_does_not_match(
    tmp_path: Path,
) -> None:
    bare = _bare_repo(tmp_path)
    dest = tmp_path / "backend"
    _clone_local(bare, dest, sparse=True, cone=["src"])
    assert (dest / "src/main.py").is_file()
    assert not (dest / WORKFLOW).exists()

    with _offline_clone(bare, dest):
        git.clone(REPO, BRANCH, dest, [".github"])

    assert (dest / WORKFLOW).is_file()
    assert not (dest / "src/main.py").exists()


def test_clone_without_sparse_paths_materializes_the_full_worktree(
    tmp_path: Path,
) -> None:
    bare = _bare_repo(tmp_path)
    dest = tmp_path / "backend"
    _clone_local(bare, dest, sparse=True, cone=[".github"])
    assert not (dest / "src/main.py").exists()

    with _offline_clone(bare, dest):
        git.clone(REPO, BRANCH, dest, None)

    for relative in (WORKFLOW, "README.md", "src/main.py"):
        assert (dest / relative).is_file()


def test_clone_sets_the_cone_when_reusing_a_matching_dest(tmp_path: Path) -> None:
    bare = _bare_repo(tmp_path)
    dest = tmp_path / "backend"
    _clone_local(bare, dest, sparse=True, cone=[".github"])

    with _offline_clone(bare, dest) as spy:
        git.clone(REPO, BRANCH, dest, [".github"])

    sets = [cmd for cmd in spy.commands if cmd[:3] == ["git", "sparse-checkout", "set"]]
    clones = [cmd for cmd in spy.commands if _git_subcommand(cmd, "clone")]
    assert sets == [["git", "sparse-checkout", "set", ".github"]]
    assert clones == []
    assert (dest / WORKFLOW).is_file()


def test_clone_replaces_a_dest_whose_head_does_not_resolve(tmp_path: Path) -> None:
    """A broken .git must not bind a parent repository and check that out."""
    bare = _bare_repo(tmp_path)
    parent = tmp_path / "parent"
    parent.mkdir()
    _run_git(parent, "init", "--initial-branch=main")
    _run_git(parent, "config", "user.email", "test@test.com")
    _run_git(parent, "config", "user.name", "Test")
    (parent / "PARENT_OK").write_text("keep\n")
    _run_git(parent, "add", ".")
    _run_git(parent, "commit", "-m", "init")
    head = _run_git(parent, "rev-parse", "HEAD").stdout
    dest = parent / "backend"
    (dest / ".git").mkdir(parents=True)

    with _offline_clone(bare, dest):
        git.clone(REPO, BRANCH, dest, [".github"])

    assert (dest / WORKFLOW).is_file()
    assert (parent / "PARENT_OK").read_text() == "keep\n"
    assert _run_git(parent, "rev-parse", "HEAD").stdout == head


def test_failed_fetch_removes_the_dest_before_the_next_attempt(
    tmp_path: Path,
) -> None:
    bare = _bare_repo(tmp_path)
    dest = tmp_path / "backend"
    _clone_local(bare, dest, sparse=False)

    def fail(argv: list[str]) -> bool:
        return _git_subcommand(argv, "fetch") or _git_subcommand(argv, "clone")

    with (
        _offline_clone(bare, dest, fail=fail) as spy,
        pytest.raises(subprocess.CalledProcessError),
    ):
        git.clone(REPO, BRANCH, dest, None)

    assert spy.dest_existed_before_retry == [False, False]
    assert not dest.exists()


def test_failed_sparse_checkout_set_removes_the_dest_before_the_next_attempt(
    tmp_path: Path,
) -> None:
    bare = _bare_repo(tmp_path)
    dest = tmp_path / "backend"

    def fail(argv: list[str]) -> bool:
        return argv[:3] == ["git", "sparse-checkout", "set"]

    with (
        _offline_clone(bare, dest, fail=fail) as spy,
        pytest.raises(subprocess.CalledProcessError),
    ):
        git.clone(REPO, BRANCH, dest, [".github"])

    assert spy.dest_existed_before_retry == [False, False]
    assert not dest.exists()
