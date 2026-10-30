from __future__ import annotations

import argparse
import os
import shutil
import subprocess
from pathlib import Path

import pytest

import repomod.cli as cli_module
import repomod.sync as sync_module
from repomod.catalog import Catalog, load_catalog, repository_path
from repomod.git import _porcelain_counts, expected_origin, head, status
from repomod.sync import SyncResult, sync_one, untrack_archived_repositories
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



def make_parent(tmp_path: Path) -> tuple[Path, str]:
    bare = tmp_path / "upstream.git"
    seed = tmp_path / "upstream-seed"
    git("init", "--bare", "--initial-branch=main", str(bare))
    git("init", "--initial-branch=main", str(seed))
    (seed / "UPSTREAM.md").write_text("upstream\n", encoding="utf-8")
    git("add", "UPSTREAM.md", cwd=seed)
    git("commit", "-m", "upstream initial", cwd=seed)
    git("push", str(bare), "main", cwd=seed)
    return bare, git("rev-parse", "HEAD", cwd=seed)


def parent_remote(repository: Repository, parent_sha: str, branch: str = "main") -> RemoteRepository:
    return RemoteRepository(
        str(repository.id), "main", "unused", False,
        parent="acme/upstream", parent_branch=branch, parent_sha=parent_sha,
    )


def upstream_refs(checkout: Path) -> list[str]:
    return git("for-each-ref", "--format=%(refname)", "refs/remotes/upstream/", cwd=checkout).splitlines()


def test_sync_clone_adds_and_fetches_upstream_remote(
    tmp_path: Path, git_environment: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    make_remote(tmp_path)
    upstream_bare, upstream_sha = make_parent(tmp_path)
    monkeypatch.setattr(sync_module, "upstream_url", lambda parent: f"file://{upstream_bare}")
    catalog, repository = make_catalog(tmp_path)

    result = sync_one(catalog, repository, parent_remote(repository, upstream_sha))

    checkout = repository_path(catalog, repository)
    assert result.action == "cloned"
    assert "upstream acme/upstream added and fetched" in result.message
    assert git("config", "--get", "remote.upstream.url", cwd=checkout) == f"file://{upstream_bare}"
    assert "refs/remotes/upstream/main" in upstream_refs(checkout)


def test_sync_existing_checkout_gains_upstream_remote(
    tmp_path: Path, git_environment: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    make_remote(tmp_path)
    upstream_bare, upstream_sha = make_parent(tmp_path)
    monkeypatch.setattr(sync_module, "upstream_url", lambda parent: f"file://{upstream_bare}")
    catalog, repository = make_catalog(tmp_path)
    plain = RemoteRepository(str(repository.id), "main", "unused", False)
    sync_one(catalog, repository, plain)
    checkout = repository_path(catalog, repository)
    assert upstream_refs(checkout) == []

    result = sync_one(catalog, repository, parent_remote(repository, upstream_sha))

    assert result.action in {"updated", "current", "fetched"}
    assert "upstream acme/upstream added and fetched" in result.message
    assert "refs/remotes/upstream/main" in upstream_refs(checkout)


def test_sync_repoints_wrong_upstream_url(
    tmp_path: Path, git_environment: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    make_remote(tmp_path)
    upstream_bare, upstream_sha = make_parent(tmp_path)
    monkeypatch.setattr(sync_module, "upstream_url", lambda parent: f"file://{upstream_bare}")
    catalog, repository = make_catalog(tmp_path)
    sync_one(catalog, repository, RemoteRepository(str(repository.id), "main", "unused", False))
    checkout = repository_path(catalog, repository)
    git("remote", "add", "upstream", "file:///elsewhere.git", cwd=checkout)

    result = sync_one(catalog, repository, parent_remote(repository, upstream_sha))

    assert "upstream acme/upstream repointed and fetched" in result.message
    assert git("config", "--get", "remote.upstream.url", cwd=checkout) == f"file://{upstream_bare}"


def test_sync_leaves_non_forks_without_upstream_remote(
    tmp_path: Path, git_environment: None
) -> None:
    make_remote(tmp_path)
    catalog, repository = make_catalog(tmp_path)
    result = sync_one(catalog, repository, RemoteRepository(str(repository.id), "main", "unused", False))

    assert result.action == "cloned"
    assert "upstream" not in result.message
    assert upstream_refs(repository_path(catalog, repository)) == []


def test_sync_upstream_failure_is_non_fatal(
    tmp_path: Path, git_environment: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    _, seed, _ = make_remote(tmp_path)
    monkeypatch.setattr(sync_module, "upstream_url", lambda parent: "file:///does/not/exist.git")
    catalog, repository = make_catalog(tmp_path)
    sha = git("rev-parse", "HEAD", cwd=seed)
    remote = RemoteRepository(
        str(repository.id), "main", sha, False,
        parent="acme/upstream", parent_branch="main", parent_sha="b" * 40,
    )
    sync_one(catalog, repository, remote)

    result = sync_one(catalog, repository, remote)

    assert result.action == "current"
    assert result.failed is False
    assert "upstream acme/upstream unavailable" in result.message


def test_sync_dry_run_reports_upstream_plan_without_mutating(
    tmp_path: Path, git_environment: None
) -> None:
    make_remote(tmp_path)
    catalog, repository = make_catalog(tmp_path)
    checkout = repository_path(catalog, repository)

    missing = sync_one(catalog, repository, parent_remote(repository, "b" * 40), dry_run=True)
    assert missing.action == "planned"
    assert "would clone into" in missing.message
    assert "would ensure the upstream acme/upstream remote and fetch main" in missing.message
    assert not checkout.exists()

    sync_one(catalog, repository, RemoteRepository(str(repository.id), "main", "unused", False))
    assert upstream_refs(checkout) == []
    planned = sync_one(catalog, repository, parent_remote(repository, "b" * 40), dry_run=True)
    assert planned.action == "planned"
    assert "would ensure the upstream acme/upstream remote and fetch main" in planned.message
    assert upstream_refs(checkout) == []


def test_ensure_upstream_remote_states(tmp_path: Path, git_environment: None) -> None:
    from repomod.git import ensure_upstream_remote

    work = tmp_path / "work"
    git("init", "--initial-branch=main", str(work))
    assert ensure_upstream_remote(work, "file:///one.git") == "added"
    assert ensure_upstream_remote(work, "file:///one.git") == "current"
    assert ensure_upstream_remote(work, "file:///two.git") == "updated"
    assert git("config", "--get", "remote.upstream.url", cwd=work) == "file:///two.git"


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
        cli_module,
        "_catalog",
        lambda: load_catalog(
            catalog.path, {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path / "fleet")}
        ),
    )
    monkeypatch.setattr(
        cli_module, "remote_metadata", lambda repositories, **kwargs: {str(repository.id): archived}
    )

    assert cli_module._sync(argparse.Namespace(dry_run=False, workers=4)) == 0

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
        cli_module,
        "_catalog",
        lambda: load_catalog(
            catalog.path, {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path / "fleet")}
        ),
    )
    monkeypatch.setattr(
        cli_module, "remote_metadata", lambda repositories, **kwargs: {str(repository.id): archived}
    )

    assert cli_module._sync(argparse.Namespace(dry_run=True, workers=4)) == 0

    out = capsys.readouterr().out
    assert "would remove archived checkout" in out
    assert "would untrack archived repository from catalog" in out
    assert repository_path(catalog, repository).is_dir()
    reloaded = load_catalog(
        catalog.path, {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path / "fleet")}
    )
    assert str(repository.id) in {str(repo.id) for repo in reloaded.repositories}
