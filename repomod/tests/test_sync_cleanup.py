from __future__ import annotations

import argparse
from pathlib import Path

import pytest
from git_helpers import make_catalog, make_remote

import repomod.cmd.sync as sync_cmd
from repomod.catalog import load_catalog, repository_path
from repomod.cmd import common as cmd_common
from repomod.sync import SyncResult, sync_one, untrack_archived_repositories
from repomod.types import RemoteRepository


def test_sync_removes_clean_checkout_when_github_reports_archived(
    tmp_path: Path,
    git_environment: None,
) -> None:
    _, _, _ = make_remote(tmp_path)
    catalog, repository = make_catalog(tmp_path)
    active = RemoteRepository(str(repository.id), "main", "unused", False)
    sync_one(catalog, repository, active)
    checkout = repository_path(catalog, repository)
    assert checkout.is_dir()

    result = sync_one(catalog, repository, RemoteRepository(str(repository.id), "main", "unused", True))

    assert result.action == "removed"
    assert result.failed is False
    assert not checkout.exists()


def test_sync_archived_without_checkout_is_skipped(
    tmp_path: Path,
    git_environment: None,
) -> None:
    catalog, repository = make_catalog(tmp_path)
    result = sync_one(catalog, repository, RemoteRepository(str(repository.id), "main", "unused", True))

    assert result.action == "skipped"
    assert result.failed is False
    assert "no checkout" in result.message
    assert not repository_path(catalog, repository).exists()


def test_sync_refuses_to_remove_dirty_archived_checkout(
    tmp_path: Path,
    git_environment: None,
) -> None:
    _, _, _ = make_remote(tmp_path)
    catalog, repository = make_catalog(tmp_path)
    sync_one(catalog, repository, RemoteRepository(str(repository.id), "main", "unused", False))
    checkout = repository_path(catalog, repository)
    (checkout / "README.md").write_text("local edit\n", encoding="utf-8")

    result = sync_one(catalog, repository, RemoteRepository(str(repository.id), "main", "unused", True))

    assert result.action == "skipped"
    assert result.failed is True
    assert "refusing to remove" in result.message
    assert checkout.is_dir()
    assert (checkout / "README.md").read_text(encoding="utf-8") == "local edit\n"


def test_sync_archived_dry_run_plans_removal_without_deleting(
    tmp_path: Path,
    git_environment: None,
) -> None:
    _, _, _ = make_remote(tmp_path)
    catalog, repository = make_catalog(tmp_path)
    sync_one(catalog, repository, RemoteRepository(str(repository.id), "main", "unused", False))
    checkout = repository_path(catalog, repository)

    result = sync_one(
        catalog, repository, RemoteRepository(str(repository.id), "main", "unused", True), dry_run=True
    )

    assert result.action == "planned"
    assert "would remove" in result.message
    assert checkout.is_dir()


def test_sync_archived_dry_run_without_checkout_is_skipped(
    tmp_path: Path, git_environment: None
) -> None:
    catalog, repository = make_catalog(tmp_path)
    result = sync_one(
        catalog, repository, RemoteRepository(str(repository.id), "main", "unused", True), dry_run=True
    )

    assert result.action == "skipped"
    assert result.failed is False
    assert "no checkout" in result.message

def test_untrack_archived_repositories_comments_out_catalog(tmp_path: Path) -> None:
    catalog, repository = make_catalog(tmp_path)
    name = str(repository.id)
    archived = RemoteRepository(name, "main", "unused", True)

    untracked = untrack_archived_repositories(
        catalog, {name: archived}, [SyncResult(name, "removed", "removed archived checkout")]
    )

    assert [(result.repository, result.action) for result in untracked] == [(name, "untracked")]
    assert f"# {repository.id.name} =" in catalog.path.read_text(encoding="utf-8")
    reloaded = load_catalog(
        catalog.path, {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path / "fleet")}
    )
    assert name not in {str(repo.id) for repo in reloaded.repositories}


def test_untrack_archived_repositories_skips_blocked_and_active(tmp_path: Path) -> None:
    catalog, repository = make_catalog(tmp_path)
    name = str(repository.id)
    before = catalog.path.read_text(encoding="utf-8")
    remotes = {
        name: RemoteRepository(name, "main", "unused", True),
        "acme/other": RemoteRepository("acme/other", "main", "unused", False),
    }
    results = [
        SyncResult(name, "skipped", "checkout has uncommitted changes; refusing", True),
        SyncResult("acme/other", "current", "already current"),
    ]

    assert untrack_archived_repositories(catalog, remotes, results) == []
    assert catalog.path.read_text(encoding="utf-8") == before


def test_untrack_archived_repositories_dry_run_plans_without_editing(tmp_path: Path) -> None:
    catalog, repository = make_catalog(tmp_path)
    name = str(repository.id)
    before = catalog.path.read_text(encoding="utf-8")
    archived = RemoteRepository(name, "main", "unused", True)

    untracked = untrack_archived_repositories(
        catalog,
        {name: archived},
        [SyncResult(name, "planned", "would remove archived checkout")],
        dry_run=True,
    )

    assert [(result.repository, result.action) for result in untracked] == [(name, "planned")]
    assert "would untrack" in untracked[0].message
    assert catalog.path.read_text(encoding="utf-8") == before


def test_sync_command_untracks_archived_repo_from_catalog(
    tmp_path: Path, git_environment: None, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    make_remote(tmp_path)
    catalog, repository = make_catalog(tmp_path)
    sync_one(catalog, repository, RemoteRepository(str(repository.id), "main", "unused", False))
    assert repository_path(catalog, repository).is_dir()
    archived = RemoteRepository(str(repository.id), "main", "unused", True)
    monkeypatch.setattr(
        cmd_common,
        "_catalog",
        lambda: load_catalog(
            catalog.path, {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path / "fleet")}
        ),
    )
    monkeypatch.setattr(
        "repomod.github.remote_metadata", lambda repositories, **kwargs: {str(repository.id): archived}
    )

    assert sync_cmd._sync(argparse.Namespace(dry_run=False, workers=4)) == 0

    out = capsys.readouterr().out
    assert f"{repository.id}: removed:" in out
    assert f"{repository.id}: untracked:" in out
    assert not repository_path(catalog, repository).exists()
    reloaded = load_catalog(
        catalog.path, {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path / "fleet")}
    )
    assert str(repository.id) not in {str(repo.id) for repo in reloaded.repositories}


def test_sync_command_dry_run_plans_untrack_without_mutating(
    tmp_path: Path, git_environment: None, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    make_remote(tmp_path)
    catalog, repository = make_catalog(tmp_path)
    sync_one(catalog, repository, RemoteRepository(str(repository.id), "main", "unused", False))
    archived = RemoteRepository(str(repository.id), "main", "unused", True)
    monkeypatch.setattr(
        cmd_common,
        "_catalog",
        lambda: load_catalog(
            catalog.path, {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path / "fleet")}
        ),
    )
    monkeypatch.setattr(
        "repomod.github.remote_metadata", lambda repositories, **kwargs: {str(repository.id): archived}
    )

    assert sync_cmd._sync(argparse.Namespace(dry_run=True, workers=4)) == 0

    out = capsys.readouterr().out
    assert "would remove archived checkout" in out
    assert "would untrack archived repository from catalog" in out
    assert repository_path(catalog, repository).is_dir()
    reloaded = load_catalog(
        catalog.path, {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path / "fleet")}
    )
    assert str(repository.id) in {str(repo.id) for repo in reloaded.repositories}
