from __future__ import annotations

import shutil
from dataclasses import replace
from pathlib import Path

import pytest
from git_helpers import git, make_catalog, make_remote

from repomod.catalog import repository_path
from repomod.clean import gc_one
from repomod.git import safe_fast_forward, status
from repomod.sync import sync_one
from repomod.types import RemoteRepository


@pytest.mark.parametrize("operation", ["sync", "gc"])
@pytest.mark.parametrize("dry_run", [False, True])
@pytest.mark.parametrize("state", ["branch", "tag", "recovery", "stash", "worktree", "dirty-worktree",
                                   "missing-worktree", "hidden-local", "hidden-global", "remote-missing"])
def test_removal_preserves_repository_state(
    tmp_path: Path, git_environment: None, operation: str, dry_run: bool, state: str,
) -> None:
    bare, _, _ = make_remote(tmp_path)
    catalog, repository = make_catalog(tmp_path)
    active = RemoteRepository(str(repository.id), "main", "unused", False)
    sync_one(catalog, repository, active)
    checkout = repository_path(catalog, repository)
    original = git("rev-parse", "HEAD", cwd=checkout)
    linked = tmp_path / "linked"
    if state == "branch":
        git("checkout", "-b", "unpublished", cwd=checkout)
        (checkout / "feature").write_text("unpublished")
        git("add", "feature", cwd=checkout)
        git("commit", "-m", "unpublished", cwd=checkout)
        original = git("rev-parse", "HEAD", cwd=checkout)
        git("checkout", "main", cwd=checkout)
    elif state == "tag":
        git("tag", "unpublished", cwd=checkout)
    elif state == "recovery":
        git("update-ref", "refs/repomod/recovery/test", original, cwd=checkout)
    elif state == "stash":
        (checkout / "README.md").write_text("saved")
        git("stash", cwd=checkout)
    elif "worktree" in state:
        git("worktree", "add", "--detach", str(linked), cwd=checkout)
        if state == "dirty-worktree":
            (linked / "local").write_text("preserve")
        elif state == "missing-worktree":
            shutil.rmtree(linked)
    elif state.startswith("hidden-"):
        scope = "--local" if state == "hidden-local" else "--global"
        git("config", scope, "status.showUntrackedFiles", "no", cwd=checkout)
        (checkout / "local").write_text("preserve")
        assert status(checkout, repository.id).dirty
    elif state == "remote-missing":
        git("--git-dir", str(bare), "update-ref", "-d", "refs/heads/main")
    before_refs = git("show-ref", cwd=checkout)
    if operation == "sync":
        result = sync_one(catalog, repository, replace(active, archived=True), dry_run=dry_run)
    else:
        result = gc_one(replace(catalog, repositories=()), checkout, dry_run=dry_run, force=False)
    assert result.action == "skipped", result
    assert checkout.is_dir()
    assert git("show-ref", cwd=checkout) == before_refs
    assert git("cat-file", "-t", original, cwd=checkout) == "commit"
    if linked.exists():
        git("status", "--porcelain", cwd=linked)
        if state == "dirty-worktree":
            assert (linked / "local").read_text() == "preserve"
    if state.startswith("hidden-"):
        assert (checkout / "local").read_text() == "preserve"


@pytest.mark.parametrize("state", ["recovery", "worktree"])
def test_gc_force_protects_recovery_and_shared_storage(
    tmp_path: Path, git_environment: None, state: str,
) -> None:
    make_remote(tmp_path)
    catalog, repository = make_catalog(tmp_path)
    sync_one(catalog, repository, RemoteRepository(str(repository.id), "main", "unused", False))
    checkout = repository_path(catalog, repository)
    if state == "recovery":
        git("update-ref", "refs/repomod/recovery/test", "HEAD", cwd=checkout)
    else:
        git("worktree", "add", "--detach", str(tmp_path / "linked"), cwd=checkout)
    result = gc_one(replace(catalog, repositories=()), checkout, dry_run=False, force=True)
    assert result.action == "skipped"
    assert checkout.exists()


def test_removal_refuses_unavailable_remote(tmp_path: Path, git_environment: None) -> None:
    bare, _, _ = make_remote(tmp_path)
    catalog, repository = make_catalog(tmp_path)
    active = RemoteRepository(str(repository.id), "main", "unused", False)
    sync_one(catalog, repository, active)
    bare.rename(tmp_path / "unavailable.git")
    result = sync_one(catalog, repository, replace(active, archived=True))
    assert result.failed
    assert repository_path(catalog, repository).exists()


def test_fast_forward_recheck_detects_hidden_untracked_files(tmp_path: Path, git_environment: None) -> None:
    _, seed, url = make_remote(tmp_path)
    checkout = tmp_path / "checkout"
    git("clone", url, str(checkout))
    git("config", "status.showUntrackedFiles", "no", cwd=checkout)
    (checkout / "local").write_text("preserve")
    (seed / "remote").write_text("remote")
    git("add", "remote", cwd=seed)
    git("commit", "-m", "advance", cwd=seed)
    git("push", "origin", "main", cwd=seed)
    git("fetch", "origin", cwd=checkout)
    original = git("rev-parse", "HEAD", cwd=checkout)
    assert safe_fast_forward(checkout, "main", original) == "checkout changed during sync"
    assert git("rev-parse", "HEAD", cwd=checkout) == original


@pytest.mark.parametrize("operation", ["sync", "gc"])
def test_removal_rechecks_refs_after_remote_inspection(
    tmp_path: Path, git_environment: None, monkeypatch: pytest.MonkeyPatch, operation: str,
) -> None:
    import repomod.git as git_module

    make_remote(tmp_path)
    catalog, repository = make_catalog(tmp_path)
    active = RemoteRepository(str(repository.id), "main", "unused", False)
    sync_one(catalog, repository, active)
    checkout = repository_path(catalog, repository)
    original_run_git = git_module.run_git

    def add_recovery_after_remote_read(arguments, cwd=None, **kwargs):
        result = original_run_git(arguments, cwd=cwd, **kwargs)
        if arguments[0] == "ls-remote":
            git("update-ref", "refs/repomod/recovery/concurrent", "HEAD", cwd=checkout)
        return result

    monkeypatch.setattr(git_module, "run_git", add_recovery_after_remote_read)
    if operation == "sync":
        result = sync_one(catalog, repository, replace(active, archived=True))
    else:
        result = gc_one(replace(catalog, repositories=()), checkout, dry_run=False, force=False)
    assert result.action == "skipped"
    assert "changed" in result.message
    assert git("rev-parse", "refs/repomod/recovery/concurrent", cwd=checkout)
