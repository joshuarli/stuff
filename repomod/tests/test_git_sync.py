from __future__ import annotations

import shutil
from pathlib import Path

import pytest
from git_helpers import git, make_catalog, make_remote

import repomod.sync as sync_module
from repomod.catalog import repository_path
from repomod.git import head, status
from repomod.sync import sync_one
from repomod.types import RemoteRepository


def test_sync_clones_missing_checkout_then_fast_forwards_safely(
    tmp_path: Path,
    git_environment: None,
) -> None:
    _, seed, _ = make_remote(tmp_path)
    catalog, repository = make_catalog(tmp_path)
    remote = RemoteRepository(str(repository.id), "main", "unused", False)

    first = sync_one(catalog, repository, remote)
    checkout = repository_path(catalog, repository)
    assert first.action == "cloned"
    assert status(checkout, repository.id).origin_valid
    assert status(checkout, repository.id).branch == "main"

    (seed / "README.md").write_text("second\n", encoding="utf-8")
    git("commit", "-am", "second", cwd=seed)
    git("push", "origin", "main", cwd=seed)
    expected = git("rev-parse", "HEAD", cwd=seed)
    result = sync_one(catalog, repository, remote)

    assert result.action == "updated"
    assert head(checkout) == expected


def test_sync_clone_sets_origin_head_for_default_branch_resolution(
    tmp_path: Path,
    git_environment: None,
) -> None:
    make_remote(tmp_path)
    catalog, repository = make_catalog(tmp_path)
    remote = RemoteRepository(str(repository.id), "main", "unused", False)

    sync_one(catalog, repository, remote)

    assert (
        git("symbolic-ref", "refs/remotes/origin/HEAD", cwd=repository_path(catalog, repository))
        == "refs/remotes/origin/main"
    )


def test_sync_noop_leaves_a_current_checkout_untouched(
    tmp_path: Path,
    git_environment: None,
) -> None:
    _, seed, _ = make_remote(tmp_path)
    catalog, repository = make_catalog(tmp_path)
    sha = git("rev-parse", "HEAD", cwd=seed)
    remote = RemoteRepository(str(repository.id), "main", sha, False)
    sync_one(catalog, repository, remote)
    before = git("rev-parse", "HEAD", cwd=repository_path(catalog, repository))

    result = sync_one(catalog, repository, remote)

    assert result.action == "current"
    assert head(repository_path(catalog, repository)) == before


def test_sync_fetches_but_does_not_change_dirty_checkout(
    tmp_path: Path,
    git_environment: None,
) -> None:
    _, seed, _ = make_remote(tmp_path)
    catalog, repository = make_catalog(tmp_path)
    remote = RemoteRepository(str(repository.id), "main", "unused", False)
    sync_one(catalog, repository, remote)
    checkout = repository_path(catalog, repository)
    (checkout / "README.md").write_text("local edit\n", encoding="utf-8")
    before_head = head(checkout)
    before_status = git("status", "--porcelain=v2", cwd=checkout)
    (seed / "README.md").write_text("remote edit\n", encoding="utf-8")
    git("commit", "-am", "remote change", cwd=seed)
    git("push", "origin", "main", cwd=seed)

    result = sync_one(catalog, repository, remote)

    assert result.action == "fetched"
    assert head(checkout) == before_head
    assert (checkout / "README.md").read_text(encoding="utf-8") == "local edit\n"
    assert git("status", "--porcelain=v2", cwd=checkout) == before_status


def test_sync_does_not_fast_forward_branch_checked_out_during_fetch(
    tmp_path: Path,
    git_environment: None,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _, seed, _ = make_remote(tmp_path)
    catalog, repository = make_catalog(tmp_path)
    remote = RemoteRepository(str(repository.id), "main", "unused", False)
    sync_one(catalog, repository, remote)
    checkout = repository_path(catalog, repository)
    initial_head = head(checkout)
    git("checkout", "-b", "feature", cwd=checkout)
    git("checkout", "main", cwd=checkout)
    (seed / "README.md").write_text("second\n", encoding="utf-8")
    git("commit", "-am", "second", cwd=seed)
    git("push", "origin", "main", cwd=seed)
    original_fetch = sync_module.fetch_default

    def fetch_and_switch(path: Path, branch: str, deepen: int | None = None) -> None:
        original_fetch(path, branch, deepen)
        git("checkout", "feature", cwd=path)

    monkeypatch.setattr(sync_module, "fetch_default", fetch_and_switch)

    result = sync_one(catalog, repository, remote)

    assert result.action == "fetched"
    assert head(checkout) == initial_head
    assert git("branch", "--show-current", cwd=checkout) == "feature"


def test_sync_dry_run_does_not_clone_or_fetch(
    tmp_path: Path, git_environment: None
) -> None:
    catalog, repository = make_catalog(tmp_path)
    result = sync_one(catalog, repository, None, dry_run=True)

    assert result.action == "planned"
    assert not repository_path(catalog, repository).exists()

def test_sync_clone_is_full_history_without_blobs(
    tmp_path: Path, git_environment: None
) -> None:
    _, seed, _ = make_remote(tmp_path)
    (seed / "SECOND.md").write_text("second\n", encoding="utf-8")
    git("add", "SECOND.md", cwd=seed)
    git("commit", "-m", "second", cwd=seed)
    git("push", "origin", "main", cwd=seed)
    catalog, repository = make_catalog(tmp_path)
    remote = RemoteRepository(str(repository.id), "main", "unused", False)

    result = sync_one(catalog, repository, remote)
    checkout = repository_path(catalog, repository)

    assert result.action == "cloned"
    assert git("rev-parse", "--is-shallow-repository", cwd=checkout) == "false"
    assert "second" in git("log", "--format=%s", cwd=checkout)
    assert "initial" in git("log", "--format=%s", cwd=checkout)
    assert git("config", "--get", "remote.origin.partialclonefilter", cwd=checkout) == "blob:none"


def test_sync_unshallows_existing_shallow_checkout(
    tmp_path: Path, git_environment: None
) -> None:
    _, seed, url = make_remote(tmp_path)
    (seed / "SECOND.md").write_text("second\n", encoding="utf-8")
    git("add", "SECOND.md", cwd=seed)
    git("commit", "-m", "second", cwd=seed)
    git("push", "origin", "main", cwd=seed)
    catalog, repository = make_catalog(tmp_path)
    checkout = repository_path(catalog, repository)
    checkout.parent.mkdir(parents=True)
    git("clone", "--depth", "1", url, str(checkout))
    assert git("rev-parse", "--is-shallow-repository", cwd=checkout) == "true"
    assert "initial" not in git("log", "--format=%s", cwd=checkout)

    result = sync_one(catalog, repository, RemoteRepository(str(repository.id), "main", "unused", False))

    assert "completed full history" in result.message
    assert git("rev-parse", "--is-shallow-repository", cwd=checkout) == "false"
    assert "initial" in git("log", "--format=%s", cwd=checkout)
    assert "second" in git("log", "--format=%s", cwd=checkout)


def test_sync_shallow_unshallow_failure_fails(
    tmp_path: Path, git_environment: None
) -> None:
    bare, _, url = make_remote(tmp_path)
    catalog, repository = make_catalog(tmp_path)
    checkout = repository_path(catalog, repository)
    checkout.parent.mkdir(parents=True)
    git("clone", "--depth", "1", url, str(checkout))
    assert git("rev-parse", "--is-shallow-repository", cwd=checkout) == "true"
    shutil.rmtree(bare)

    result = sync_one(catalog, repository, RemoteRepository(str(repository.id), "main", "unused", False))

    assert result.action == "failed"
    assert result.failed is True
    assert "could not complete history from shallow clone" in result.message
