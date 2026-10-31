from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from threading import Barrier
from typing import Any, cast

import pytest
from fix_helpers import file_rule, git, initialize_repository, subjects

import repomod.fix as fix_module
from repomod.fix import FixOutcome, fix_repository
from repomod.git import head, status
from repomod.types import Finding, Repository, Rule, TagPredicate


def test_worktree_cleanup_failure_reports_path_without_losing_fix_branch(
    tmp_path: Path,
    git_environment: None,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    catalog, repository, path, base = initialize_repository(tmp_path)
    original_run_git = fix_module.run_git

    def fail_cleanup(arguments, cwd=None, **kwargs):
        if arguments[:3] == ["worktree", "remove", "--force"]:
            raise RuntimeError("injected worktree cleanup failure")
        return original_run_git(arguments, cwd=cwd, **kwargs)

    monkeypatch.setattr(fix_module, "run_git", fail_cleanup)

    outcome = fix_repository(
        catalog, repository, (file_rule("first", "first.txt", "first\n"),), "main"
    )

    assert len(outcome.commits) == 1
    assert outcome.branch is not None
    assert head(path) == base
    assert outcome.cleanup_error is not None
    assert outcome.recovery_worktree is not None
    assert Path(outcome.recovery_worktree).is_dir()
    assert git("show", f"{outcome.branch}:first.txt", cwd=path).stdout == "first\n"
    assert git("merge-base", "--is-ancestor", base, outcome.branch, cwd=path).returncode == 0

def test_canonical_change_during_fix_preserves_commits_on_fix_branch(
    tmp_path: Path,
    git_environment: None,
) -> None:
    catalog, repository, path, base = initialize_repository(tmp_path)

    def concurrent_edit(root: Path, repo: Repository) -> None:
        (path / "concurrent.txt").write_text("owned by user\n", encoding="utf-8")

    outcome = fix_repository(
        catalog,
        repository,
        (file_rule("first", "first.txt", "first\n", on_fix=concurrent_edit),),
        "main",
    )

    assert outcome.branch is not None
    assert outcome.commits
    assert outcome.failures[0][0] == "publish"
    assert (path / "concurrent.txt").read_text(encoding="utf-8") == "owned by user\n"
    assert not (path / "first.txt").exists()
    assert git("show", f"{outcome.branch}:first.txt", cwd=path).stdout == "first\n"
    assert head(path) == base
    assert status(path, repository.id).dirty

def test_concurrent_canonical_commit_is_never_overwritten(
    tmp_path: Path,
    git_environment: None,
) -> None:
    catalog, repository, path, base = initialize_repository(tmp_path)

    def concurrent_commit(root: Path, repo: Repository) -> None:
        (path / "concurrent.txt").write_text("user commit\n", encoding="utf-8")
        git("add", "concurrent.txt", cwd=path)
        git("commit", "-m", "user commit", cwd=path)

    outcome = fix_repository(
        catalog,
        repository,
        (file_rule("first", "first.txt", "first\n", on_fix=concurrent_commit),),
        "main",
    )

    assert outcome.branch is not None
    assert outcome.failures[0][0] == "publish"
    assert head(path) != base
    assert (path / "concurrent.txt").read_text(encoding="utf-8") == "user commit\n"
    assert not (path / "first.txt").exists()
    assert git("show", f"{outcome.branch}:first.txt", cwd=path).stdout == "first\n"

def test_commit_hooks_cannot_change_the_fix(
    tmp_path: Path, git_environment: None
) -> None:
    catalog, repository, path, base = initialize_repository(tmp_path)
    hooks = Path(git("rev-parse", "--git-path", "hooks", cwd=path).stdout.strip())
    if not hooks.is_absolute():
        hooks = path / hooks
    hooks.mkdir(exist_ok=True)
    hook = hooks / "pre-commit"
    hook.write_text("#!/bin/sh\nexit 1\n", encoding="utf-8")
    hook.chmod(0o755)

    hook_failure = fix_repository(
        catalog, repository, (file_rule("hook", "hook.txt", "hook\n"),), "main"
    )
    assert hook_failure.failures
    assert hook_failure.commits == []
    assert head(path) == base
    assert not (path / "hook.txt").exists()

    hook.write_text(
        "#!/bin/sh\nprintf 'hook changed content\\n' > README.md\ngit add README.md\n",
        encoding="utf-8",
    )

    mutating = fix_repository(
        catalog, repository, (file_rule("first", "first.txt", "first\n"),), "main"
    )

    assert mutating.commits == []
    assert any("staged commit" in message for _, message in mutating.failures)
    assert head(path) == base
    assert (path / "README.md").read_text(encoding="utf-8") == "base\n"
    assert not (path / "first.txt").exists()

def test_invalid_plan_and_symlink_target_are_rejected(tmp_path: Path, git_environment: None) -> None:
    catalog, repository, path, base = initialize_repository(tmp_path)
    invalid = Rule(
        "invalid-plan",
        "Reject invalid plans",
        TagPredicate(),
        lambda root, repo: [Finding(str(repo.id), "invalid-plan", "README.md", "invalid", fixable=True)],
        lambda root, repo: cast(Any, {"README.md": None}),
        "chore(repomod): reject invalid fixer plan",
        ("README.md",),
    )

    invalid_outcome = fix_repository(catalog, repository, (invalid,), "main")

    assert invalid_outcome.commits == []
    assert invalid_outcome.failures
    assert head(path) == base

    link = path / "linked.txt"
    link.symlink_to("README.md")
    git("add", "linked.txt", cwd=path)
    git("commit", "-m", "add symlink", cwd=path)
    symlink_rule = file_rule("symlink", "linked.txt", "replacement\n")

    symlink_outcome = fix_repository(catalog, repository, (symlink_rule,), "main")

    assert symlink_outcome.commits == []
    assert any("symlink" in message for _, message in symlink_outcome.failures)
    assert link.is_symlink()
    assert link.read_text(encoding="utf-8") == "base\n"
    symlink_head = head(path)

    escaped = Rule(
        "escape",
        "Reject paths outside the worktree",
        TagPredicate(),
        lambda root, repo: [
            Finding(str(repo.id), "escape", "../outside", "unsafe", fixable=True)
        ],
        lambda root, repo: {"../outside": "unsafe\n"},
        "chore(repomod): reject unsafe path",
        ("**",),
    )
    escape_outcome = fix_repository(catalog, repository, (escaped,), "main")

    assert escape_outcome.commits == []
    assert escape_outcome.failures
    assert not (path.parent / "outside").exists()
    assert head(path) == symlink_head

def test_interruption_preserves_completed_commit_on_fix_branch(
    tmp_path: Path,
    git_environment: None,
) -> None:
    catalog, repository, path, base = initialize_repository(tmp_path)

    def interrupt(root: Path, repo: Repository) -> None:
        raise KeyboardInterrupt

    with pytest.raises(KeyboardInterrupt):
        fix_repository(
            catalog,
            repository,
            (
                file_rule("first", "first.txt", "first\n"),
                file_rule("interrupt", "interrupt.txt", "interrupt\n", on_fix=interrupt),
            ),
            "main",
        )

    branches = git(
        "for-each-ref", "--format=%(refname)", "refs/heads/repomod/fix", cwd=path
    ).stdout.splitlines()
    assert len(branches) == 1
    assert git("show", f"{branches[0]}:first.txt", cwd=path).stdout == "first\n"
    assert head(path) == base
    assert not (path / "first.txt").exists()
    worktrees = git("worktree", "list", "--porcelain", cwd=path).stdout.splitlines()
    assert sum(line.startswith("worktree ") for line in worktrees) == 1

def test_unfixable_checkout_states_are_skipped_without_modification(
    tmp_path: Path,
    git_environment: None,
) -> None:
    catalog, repository, path, base = initialize_repository(tmp_path)
    (path / "user.txt").write_text("user\n", encoding="utf-8")

    dirty = fix_repository(
        catalog, repository, (file_rule("first", "first.txt", "x"),), "main"
    )
    assert dirty.skipped == "checkout is dirty"
    assert not (path / "first.txt").exists()
    (path / "user.txt").unlink()
    git("checkout", "-b", "feature", cwd=path)

    branch = fix_repository(
        catalog, repository, (file_rule("first", "first.txt", "x"),), "main"
    )
    assert branch.skipped is None
    assert branch.commits
    assert head(path) == base
    assert git("rev-parse", "--abbrev-ref", "HEAD", cwd=path).stdout.strip() == "feature"
    git("checkout", "main", cwd=path)

    diverged = fix_repository(
        catalog,
        repository,
        (file_rule("first", "first.txt", "first\n"),),
        "main",
        "0" * 40,
    )
    assert diverged.skipped == "local default branch differs from GitHub default branch; sync before fixing"
    assert diverged.commits == []
    assert head(path) == base

def test_two_concurrent_fix_runs_create_only_one_commit(
    tmp_path: Path,
    git_environment: None,
) -> None:
    catalog, repository, path, base = initialize_repository(tmp_path)
    barrier = Barrier(3)
    rule = file_rule("first", "first.txt", "first\n")

    def run() -> FixOutcome:
        barrier.wait()
        return fix_repository(catalog, repository, (rule,), "main")

    with ThreadPoolExecutor(max_workers=2) as pool:
        first = pool.submit(run)
        second = pool.submit(run)
        barrier.wait()
        results = [first.result(), second.result()]

    assert sum(len(result.commits) for result in results) == 1
    fix_branch = next(result.branch for result in results if result.commits)
    assert fix_branch is not None
    assert len(subjects(path, base, fix_branch)) == 1
    assert head(path) == base
