from __future__ import annotations

import subprocess
from pathlib import Path
from typing import Any

import pytest
from fix_helpers import git, initialize_repository

import repomod.fix as fix_module
from repomod.fix import push_fix_branch, push_repository
from repomod.git import head, status


def test_push_fix_branch_skips_when_remote_default_advanced(
    tmp_path: Path,
    git_environment: None,
) -> None:
    _catalog, repository, path, base = initialize_repository(tmp_path)
    update_worktree = tmp_path / "remote-update"
    git("worktree", "add", "-b", "remote-update", str(update_worktree), base, cwd=path)
    (update_worktree / "remote.txt").write_text("remote advanced\n", encoding="utf-8")
    git("add", "remote.txt", cwd=update_worktree)
    git("commit", "-m", "advance remote", cwd=update_worktree)
    git("push", "origin", "HEAD:refs/heads/main", cwd=update_worktree)
    git("worktree", "remove", "--force", str(update_worktree), cwd=path)
    branch = "repomod/fix/" + "a" * 32
    git("branch", branch, base, cwd=path)

    state, message = push_fix_branch(repository, path, branch, "main", base, base)

    assert state == "skipped"
    assert message == "remote default branch changed; sync before publishing fixes"

def test_push_rejection_keeps_local_commit_and_never_rewrites_remote(
    tmp_path: Path,
    git_environment: None,
) -> None:
    _catalog, repository, path, _ = initialize_repository(tmp_path)
    (path / "local.txt").write_text("local\n", encoding="utf-8")
    git("add", "local.txt", cwd=path)
    git("commit", "-m", "local", cwd=path)
    other = tmp_path / "other"
    url = "https://github.com/acme/demo.git"
    git("clone", url, str(other))
    (other / "remote.txt").write_text("remote\n", encoding="utf-8")
    git("add", "remote.txt", cwd=other)
    git("commit", "-m", "remote", cwd=other)
    git("push", "origin", "main", cwd=other)
    previous_head = head(path)

    state, message = push_repository(repository, path)

    assert state == "failed"
    assert "rejected" in (message or "")
    assert head(path) == previous_head
    assert status(path, repository.id).origin_valid

def test_push_uses_checked_commit_when_checkout_changes_before_push(
    tmp_path: Path,
    git_environment: None,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _catalog, repository, path, _ = initialize_repository(tmp_path)
    (path / "main.txt").write_text("main\n", encoding="utf-8")
    git("add", "main.txt", cwd=path)
    git("commit", "-m", "main change", cwd=path)
    main_head = head(path)
    original_run_git = fix_module.run_git
    switched: list[str] = []

    def switch_before_push(
        arguments: list[str], cwd: Path | None = None, **options: Any
    ) -> subprocess.CompletedProcess[str]:
        if arguments[0] == "push" and not switched:
            git("checkout", "-b", "feature", cwd=path)
            (path / "feature.txt").write_text("feature\n", encoding="utf-8")
            git("add", "feature.txt", cwd=path)
            git("commit", "-m", "feature change", cwd=path)
            switched.append(head(path))
        return original_run_git(arguments, cwd=cwd, **options)

    monkeypatch.setattr(fix_module, "run_git", switch_before_push)

    state, message = push_repository(repository, path)

    remote_head = git(
        "--git-dir", str(tmp_path / "remote.git"), "rev-parse", "refs/heads/main"
    ).stdout.strip()
    assert state == "pushed", message
    assert remote_head == main_head
    assert head(path) == switched[0]
