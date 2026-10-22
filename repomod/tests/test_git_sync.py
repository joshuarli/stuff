from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest

import repomod.sync as sync_module
from repomod.catalog import Catalog, load_catalog, repository_path
from repomod.git import _porcelain_counts, expected_origin, head, status
from repomod.sync import sync_one
from repomod.types import RemoteRepository, RepoId, Repository


def git(*arguments: str, cwd: Path | None = None) -> str:
    result = subprocess.run(
        ["git", *arguments],
        cwd=cwd,
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout.strip()


@pytest.fixture
def git_environment(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    global_config = tmp_path / "gitconfig"
    global_config.write_text(
        '[user]\n\tname = Test User\n\temail = test@example.invalid\n',
        encoding="utf-8",
    )
    monkeypatch.setenv("GIT_CONFIG_GLOBAL", str(global_config))
    monkeypatch.setenv("GIT_CONFIG_NOSYSTEM", "1")
    monkeypatch.setenv("GIT_TERMINAL_PROMPT", "0")


def make_remote(tmp_path: Path, name: str = "tool") -> tuple[Path, Path, str]:
    bare = tmp_path / f"{name}.git"
    seed = tmp_path / f"{name}-seed"
    git("init", "--bare", "--initial-branch=main", str(bare))
    git("init", "--initial-branch=main", str(seed))
    (seed / "README.md").write_text("first\n", encoding="utf-8")
    git("add", "README.md", cwd=seed)
    git("commit", "-m", "initial", cwd=seed)
    url = f"https://github.com/acme/{name}.git"
    git("remote", "add", "origin", url, cwd=seed)
    global_config = Path(os.environ["GIT_CONFIG_GLOBAL"])
    with global_config.open("a", encoding="utf-8") as config:
        config.write(f'\n[url "file://{bare}"]\n\tinsteadOf = {url}\n')
    git("push", "-u", "origin", "main", cwd=seed)
    return bare, seed, url


def make_catalog(tmp_path: Path) -> tuple[Catalog, Repository]:
    path = tmp_path / "catalog.toml"
    path.write_text(
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "joshuarli"\n\n'
        '[repos."acme"]\ntool = ["tool"]\n',
        encoding="utf-8",
    )
    catalog = load_catalog(
        path,
        {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path / "fleet")},
    )
    return catalog, catalog.repositories[0]


def test_github_origins_support_https_ssh_and_git_suffix() -> None:
    repo = RepoId("acme", "tool")

    assert expected_origin(repo, "https://github.com/acme/tool")
    assert expected_origin(repo, "https://github.com/ACME/TOOL.git")
    assert expected_origin(repo, "git@github.com:acme/tool.git")
    assert expected_origin(repo, "ssh://git@github.com/acme/tool")
    assert not expected_origin(repo, "https://github.com/acme/other.git")
    assert not expected_origin(repo, "https://github.com.evil.invalid/acme/tool")
    assert not expected_origin(repo, "https://github.com/acme/tool?fork=1")


def test_porcelain_v2_counts_header_and_rename_records() -> None:
    output = (
        "# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0"
        "# branch.ab +2 -3\0"
        "2 .M N... 100644 100644 100644 abc def R100 new name\0old name\0"
        "1 A. N... 000000 100644 100644 . abc staged\0"
        "? untracked\0"
    )

    assert _porcelain_counts(output) == (1, 1, 1)


def test_status_reads_remote_tracking_counts_from_nul_headers(
    tmp_path: Path,
    git_environment: None,
) -> None:
    _, seed, url = make_remote(tmp_path)
    checkout = tmp_path / "checkout"
    git("clone", "--branch", "main", url, str(checkout))
    (checkout / "ahead.txt").write_text("ahead\n", encoding="utf-8")
    git("add", "ahead.txt", cwd=checkout)
    git("commit", "-m", "ahead", cwd=checkout)
    (seed / "behind.txt").write_text("behind\n", encoding="utf-8")
    git("add", "behind.txt", cwd=seed)
    git("commit", "-m", "behind", cwd=seed)
    git("push", "origin", "main", cwd=seed)
    git("fetch", "origin", cwd=checkout)

    state = status(checkout, RepoId("acme", "tool"))

    assert state.origin_valid
    assert state.branch == "main"
    assert state.upstream == "origin/main"
    assert state.ahead == 1
    assert state.behind == 1


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

