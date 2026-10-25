from __future__ import annotations

import os
import shutil
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
