from __future__ import annotations

import os
import subprocess
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from threading import Barrier, Event

import pytest

from repomod.catalog import load_catalog, repository_path
from repomod.fix import fix_repository, push_repository
from repomod.git import head, status
from repomod.types import Finding, RepoId, Repository, Rule


def git(*arguments: str, cwd: Path | None = None, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *arguments],
        cwd=cwd,
        capture_output=True,
        text=True,
        check=check,
    )


@pytest.fixture
def git_environment(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    global_config = tmp_path / "gitconfig"
    global_config.write_text("[user]\n\tname = Test User\n\temail = test@example.invalid\n", encoding="utf-8")
    monkeypatch.setenv("GIT_CONFIG_GLOBAL", str(global_config))
    monkeypatch.setenv("GIT_CONFIG_NOSYSTEM", "1")
    monkeypatch.setenv("GIT_TERMINAL_PROMPT", "0")


def initialize_repository(tmp_path: Path, *, push: bool = True) -> tuple[object, Repository, Path, str]:
    root = tmp_path / "fleet"
    root.mkdir()
    path = root / "demo"
    git("init", "--initial-branch=main", str(path))
    git("config", "user.name", "Test User", cwd=path)
    git("config", "user.email", "test@example.invalid", cwd=path)
    (path / "README.md").write_text("base\n", encoding="utf-8")
    git("add", "README.md", cwd=path)
    git("commit", "-m", "base", cwd=path)
    base = git("rev-parse", "HEAD", cwd=path).stdout.strip()
    catalog_path = tmp_path / "catalog.toml"
    catalog_path.write_text(
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n\n'
        '[repos."acme"]\ndemo = ["tool"]\n',
        encoding="utf-8",
    )
    catalog = load_catalog(catalog_path, {"HOME": str(tmp_path), "REPOMOD_ROOT": str(root)})
    repository = catalog.repositories[0]
    remote_url = "https://github.com/acme/demo.git"
    if push:
        bare = tmp_path / "remote.git"
        git("init", "--bare", "--initial-branch=main", str(bare))
        global_config = Path(os.environ["GIT_CONFIG_GLOBAL"])
        with global_config.open("a", encoding="utf-8") as config:
            config.write(f'\n[url "file://{bare}"]\n\tinsteadOf = {remote_url}\n')
        git("remote", "add", "origin", remote_url, cwd=path)
        git("push", "-u", "origin", "main", cwd=path)
    else:
        git("remote", "add", "origin", remote_url, cwd=path)
    return catalog, repository, path, base


def file_rule(rule_id: str, filename: str, value: str, *, validate=None, on_fix=None) -> Rule:
    def check(root: Path, repository: Repository) -> list[Finding]:
        path = root / filename
        if path.is_file() and path.read_text(encoding="utf-8") == value:
            return []
        return [Finding(str(repository.id), rule_id, filename, "file needs its policy content", fixable=True)]

    def fix(root: Path, repository: Repository) -> dict[str, str]:
        if on_fix is not None:
            on_fix(root, repository)
        return {filename: value}

    return Rule(
        rule_id,
        f"Repair {filename}",
        frozenset(),
        check,
        fix,
        f"chore(repomod): apply {rule_id} policy",
        (filename,),
        validate,
    )


def subjects(path: Path, base: str) -> list[str]:
    output = git("log", "--format=%s", f"{base}..HEAD", cwd=path).stdout
    return output.splitlines()


def test_successful_fixers_create_one_commit_each_and_reruns_are_empty(
    tmp_path: Path,
    git_environment: None,
) -> None:
    catalog, repository, path, base = initialize_repository(tmp_path)
    selected = (
        file_rule("first", "first.txt", "first\n"),
        file_rule("second", "second.txt", "second\n"),
        file_rule("third", "third.txt", "third\n"),
    )

    outcome = fix_repository(catalog, repository, selected, "main")

    assert outcome.failures == []
    assert [rule for rule, _ in outcome.commits] == ["first", "second", "third"]
    assert len(subjects(path, base)) == 3
    assert subjects(path, base) == [rule.commit_message for rule in reversed(selected)]
    for (rule_id, commit), filename in zip(outcome.commits, ("first.txt", "second.txt", "third.txt"), strict=True):
        committed_paths = git("show", "--format=", "--name-only", commit, cwd=path).stdout.splitlines()
        assert committed_paths == [filename]
        assert (path / filename).read_text(encoding="utf-8") == filename.removesuffix(".txt") + "\n"
    repeated = fix_repository(catalog, repository, selected, "main")
    assert repeated.commits == []
    assert repeated.failures == []
    assert subjects(path, base) == [rule.commit_message for rule in reversed(selected)]
    assert len(git("worktree", "list", "--porcelain", cwd=path).stdout.split("worktree ")) == 2


def test_failed_middle_fixer_is_discarded_while_later_success_commits(
    tmp_path: Path,
    git_environment: None,
) -> None:
    catalog, repository, path, base = initialize_repository(tmp_path)
    fail_validation = lambda root, repo, changed: ["fixture validation rejected B"]
    selected = (
        file_rule("first", "first.txt", "first\n"),
        file_rule("second", "second.txt", "second\n", validate=fail_validation),
        file_rule("third", "third.txt", "third\n"),
    )

    outcome = fix_repository(catalog, repository, selected, "main")

    assert [rule for rule, _ in outcome.commits] == ["first", "third"]
    assert outcome.failures == [("second", "fixture validation rejected B")]
    assert not (path / "second.txt").exists()
    assert (path / "first.txt").read_text(encoding="utf-8") == "first\n"
    assert (path / "third.txt").read_text(encoding="utf-8") == "third\n"
    assert len(subjects(path, base)) == 2


def test_canonical_change_during_fix_preserves_commits_under_recovery_ref(
    tmp_path: Path,
    git_environment: None,
) -> None:
    catalog, repository, path, _ = initialize_repository(tmp_path)

    def concurrent_edit(root: Path, repo: Repository) -> None:
        (path / "concurrent.txt").write_text("owned by user\n", encoding="utf-8")

    outcome = fix_repository(
        catalog,
        repository,
        (file_rule("first", "first.txt", "first\n", on_fix=concurrent_edit),),
        "main",
    )

    assert outcome.recovery_ref is not None
    assert outcome.commits and outcome.failures[0][0] == "publish"
    assert (path / "concurrent.txt").read_text(encoding="utf-8") == "owned by user\n"
    assert not (path / "first.txt").exists()
    assert git("show", f"{outcome.recovery_ref}:first.txt", cwd=path).stdout == "first\n"
    assert status(path, repository.id).dirty


def test_commit_hook_failure_and_path_escape_leave_canonical_checkout_unchanged(
    tmp_path: Path,
    git_environment: None,
) -> None:
    catalog, repository, path, base = initialize_repository(tmp_path)
    hooks = Path(git("rev-parse", "--git-path", "hooks", cwd=path).stdout.strip())
    if not hooks.is_absolute():
        hooks = path / hooks
    hook = hooks / "pre-commit"
    hook.write_text("#!/bin/sh\nexit 1\n", encoding="utf-8")
    hook.chmod(0o755)

    hook_failure = fix_repository(catalog, repository, (file_rule("hook", "hook.txt", "hook\n"),), "main")
    assert hook_failure.failures and hook_failure.commits == []
    assert head(path) == base
    assert not (path / "hook.txt").exists()

    escaped = Rule(
        "escape",
        "Reject paths outside the worktree",
        frozenset(),
        lambda root, repo: [Finding(str(repo.id), "escape", "../outside", "unsafe", fixable=True)],
        lambda root, repo: {"../outside": "unsafe\n"},
        "chore(repomod): reject unsafe path",
        ("**",),
    )
    escape_outcome = fix_repository(catalog, repository, (escaped,), "main")
    assert escape_outcome.failures
    assert not (path.parent / "outside").exists()
    assert head(path) == base


def test_dirty_or_nondefault_checkouts_are_skipped_without_modification(
    tmp_path: Path,
    git_environment: None,
) -> None:
    catalog, repository, path, base = initialize_repository(tmp_path)
    (path / "user.txt").write_text("user\n", encoding="utf-8")

    dirty = fix_repository(catalog, repository, (file_rule("first", "first.txt", "x"),), "main")
    assert dirty.skipped == "checkout is dirty"
    assert not (path / "first.txt").exists()
    (path / "user.txt").unlink()
    git("checkout", "-b", "feature", cwd=path)

    branch = fix_repository(catalog, repository, (file_rule("first", "first.txt", "x"),), "main")
    assert branch.skipped == "checkout is on feature; default branch is main"
    assert head(path) == base


def test_two_concurrent_fix_runs_create_only_one_commit(
    tmp_path: Path,
    git_environment: None,
) -> None:
    catalog, repository, path, base = initialize_repository(tmp_path)
    barrier = Barrier(3)
    rule = file_rule("first", "first.txt", "first\n")

    def run() -> object:
        barrier.wait()
        return fix_repository(catalog, repository, (rule,), "main")

    with ThreadPoolExecutor(max_workers=2) as pool:
        first = pool.submit(run)
        second = pool.submit(run)
        barrier.wait()
        results = [first.result(), second.result()]

    assert sum(len(result.commits) for result in results) == 1
    assert len(subjects(path, base)) == 1


def test_push_rejection_keeps_local_commit_and_never_rewrites_remote(
    tmp_path: Path,
    git_environment: None,
) -> None:
    catalog, repository, path, _ = initialize_repository(tmp_path)
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
