from __future__ import annotations

import os
import subprocess
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from threading import Barrier
from typing import Any, cast

import pytest

import repomod.fix as fix_module
from repomod.catalog import Catalog, load_catalog
from repomod.fix import FixOutcome, fix_repository, push_repository
from repomod.git import head, status
from repomod.types import Finding, Repository, Rule, TagPredicate


def git(
    *arguments: str, cwd: Path | None = None, check: bool = True
) -> subprocess.CompletedProcess[str]:
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
    global_config.write_text(
        "[user]\n\tname = Test User\n\temail = test@example.invalid\n",
        encoding="utf-8",
    )
    monkeypatch.setenv("GIT_CONFIG_GLOBAL", str(global_config))
    monkeypatch.setenv("GIT_CONFIG_NOSYSTEM", "1")
    monkeypatch.setenv("GIT_TERMINAL_PROMPT", "0")


def initialize_repository(
    tmp_path: Path, *, push: bool = True
) -> tuple[Catalog, Repository, Path, str]:
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
    catalog = load_catalog(
        catalog_path, {"HOME": str(tmp_path), "REPOMOD_ROOT": str(root)}
    )
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


def file_rule(
    rule_id: str, filename: str, value: str, *, validate=None, on_fix=None
) -> Rule:
    def check(root: Path, repository: Repository) -> list[Finding]:
        path = root / filename
        if path.is_file() and path.read_text(encoding="utf-8") == value:
            return []
        return [
            Finding(
                str(repository.id),
                rule_id,
                filename,
                "file needs its policy content",
                fixable=True,
            )
        ]

    def fix(root: Path, repository: Repository) -> dict[str, str]:
        if on_fix is not None:
            on_fix(root, repository)
        return {filename: value}

    return Rule(
        rule_id,
        f"Repair {filename}",
        TagPredicate(),
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
    for (_rule_id, commit), filename in zip(
        outcome.commits, ("first.txt", "second.txt", "third.txt"), strict=True
    ):
        committed_paths = git(
            "show", "--format=", "--name-only", commit, cwd=path
        ).stdout.splitlines()
        assert committed_paths == [filename]
        assert (path / filename).read_text(encoding="utf-8") == (
            filename.removesuffix(".txt") + "\n"
        )
    repeated = fix_repository(catalog, repository, selected, "main")
    assert repeated.commits == []
    assert repeated.failures == []
    assert subjects(path, base) == [rule.commit_message for rule in reversed(selected)]
    assert len(
        git("worktree", "list", "--porcelain", cwd=path).stdout.split("worktree ")
    ) == 2


def test_failed_middle_fixer_is_discarded_while_later_success_commits(
    tmp_path: Path,
    git_environment: None,
) -> None:
    catalog, repository, path, base = initialize_repository(tmp_path)
    def fail_validation(root, repo, changed):
        return ["fixture validation rejected B"]
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


def test_safe_commit_with_remaining_manual_violation_returns_partial_failure(
    tmp_path: Path,
    git_environment: None,
) -> None:
    catalog, repository, path, base = initialize_repository(tmp_path)

    def check(root: Path, repo: Repository) -> list[Finding]:
        findings = []
        if not (root / "safe.txt").is_file():
            findings.append(Finding(str(repo.id), "partial", "safe.txt", "safe fix needed", fixable=True))
        findings.append(Finding(str(repo.id), "partial", "manual.txt", "owner review needed"))
        return findings

    rule = Rule(
        "partial",
        "Apply safe part of a mixed policy",
        TagPredicate(),
        check,
        lambda _root, _repo: {"safe.txt": "safe\n"},
        "chore(repomod): apply safe partial policy",
        ("safe.txt",),
    )

    outcome = fix_repository(catalog, repository, (rule,), "main")

    assert [rule_id for rule_id, _ in outcome.commits] == ["partial"]
    assert outcome.failures == [("partial", "policy violations need manual review: manual.txt: owner review needed")]
    assert (path / "safe.txt").read_text(encoding="utf-8") == "safe\n"
    assert len(subjects(path, base)) == 1


def test_new_workflow_violation_rolls_back_an_otherwise_successful_fixer(
    tmp_path: Path,
    git_environment: None,
) -> None:
    catalog, repository, path, base = initialize_repository(tmp_path)
    workflow = ".github/workflows/ci.yml"
    content = (
        "name: ci\non: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n"
        "    steps:\n      - uses: actions/checkout@main\n"
    )

    outcome = fix_repository(
        catalog, repository, (file_rule("workflow", workflow, content),), "main"
    )

    assert outcome.commits == []
    assert any("gha-pin" in message for _, message in outcome.failures), outcome.failures
    assert not (path / workflow).exists()
    assert head(path) == base


def test_merge_failure_keeps_exact_recovery_ref_and_blocks_repeated_fixes(
    tmp_path: Path,
    git_environment: None,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    catalog, repository, path, base = initialize_repository(tmp_path)
    original_run_git = fix_module.run_git

    def fail_merge(arguments, cwd=None, **kwargs):
        if arguments[:2] == ["merge", "--ff-only"]:
            raise fix_module.GitError("injected fast-forward failure")
        return original_run_git(arguments, cwd=cwd, **kwargs)

    monkeypatch.setattr(fix_module, "run_git", fail_merge)
    selected = (
        file_rule("first", "first.txt", "first\n"),
        file_rule("second", "second.txt", "second\n"),
    )

    outcome = fix_repository(catalog, repository, selected, "main")

    assert len(outcome.commits) == 2
    assert outcome.recovery_ref is not None
    assert git("rev-parse", outcome.recovery_ref, cwd=path).stdout.strip() == outcome.commits[-1][1]
    assert git("show", f"{outcome.recovery_ref}:first.txt", cwd=path).stdout == "first\n"
    assert head(path) == base
    assert not status(path, repository.id).dirty
    assert any(rule == "publish" for rule, _ in outcome.failures)

    repeated = fix_repository(catalog, repository, selected, "main")

    assert repeated.commits == []
    assert repeated.recovery_ref == outcome.recovery_ref
    assert repeated.failures
    assert head(path) == base


def test_recovery_ref_update_failure_retains_worktree_for_next_run(
    tmp_path: Path,
    git_environment: None,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    catalog, repository, path, base = initialize_repository(tmp_path)
    original_run_git = fix_module.run_git

    def fail_recovery_ref(arguments, cwd=None, **kwargs):
        if arguments[:1] == ["update-ref"] and len(arguments) == 3:
            raise RuntimeError("injected recovery ref failure")
        return original_run_git(arguments, cwd=cwd, **kwargs)

    monkeypatch.setattr(fix_module, "run_git", fail_recovery_ref)
    selected = (file_rule("first", "first.txt", "first\n"),)

    outcome = fix_repository(catalog, repository, selected, "main")

    assert len(outcome.commits) == 1
    assert outcome.recovery_ref is None
    assert outcome.recovery_worktree is not None
    assert Path(outcome.recovery_worktree).is_dir()
    assert git("rev-parse", "HEAD", cwd=Path(outcome.recovery_worktree)).stdout.strip() == outcome.commits[0][1]
    assert head(path) == base

    repeated = fix_repository(catalog, repository, selected, "main")

    assert repeated.commits == []
    assert repeated.recovery_worktree == outcome.recovery_worktree
    assert repeated.failures
    assert Path(outcome.recovery_worktree).is_dir()


def test_worktree_cleanup_failure_reports_path_without_losing_published_commit(
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
    assert head(path) == outcome.commits[-1][1]
    assert outcome.cleanup_error is not None
    assert outcome.recovery_worktree is not None
    assert Path(outcome.recovery_worktree).is_dir()
    assert git("show", f"{outcome.commits[-1][1]}:first.txt", cwd=path).stdout == "first\n"
    assert git("merge-base", "--is-ancestor", base, "HEAD", cwd=path).returncode == 0


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
    assert outcome.commits
    assert outcome.failures[0][0] == "publish"
    assert (path / "concurrent.txt").read_text(encoding="utf-8") == "owned by user\n"
    assert not (path / "first.txt").exists()
    assert git("show", f"{outcome.recovery_ref}:first.txt", cwd=path).stdout == (
        "first\n"
    )
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

    assert outcome.recovery_ref is not None
    assert outcome.failures[0][0] == "publish"
    assert head(path) != base
    assert (path / "concurrent.txt").read_text(encoding="utf-8") == "user commit\n"
    assert not (path / "first.txt").exists()
    assert git("show", f"{outcome.recovery_ref}:first.txt", cwd=path).stdout == "first\n"


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

    hook_failure = fix_repository(
        catalog, repository, (file_rule("hook", "hook.txt", "hook\n"),), "main"
    )
    assert hook_failure.failures
    assert hook_failure.commits == []
    assert head(path) == base
    assert not (path / "hook.txt").exists()

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
    assert escape_outcome.failures
    assert not (path.parent / "outside").exists()
    assert head(path) == base


def test_commit_hook_cannot_change_the_staged_tree(tmp_path: Path, git_environment: None) -> None:
    catalog, repository, path, base = initialize_repository(tmp_path)
    hooks = Path(git("rev-parse", "--git-path", "hooks", cwd=path).stdout.strip())
    if not hooks.is_absolute():
        hooks = path / hooks
    hook = hooks / "pre-commit"
    hook.write_text(
        "#!/bin/sh\nprintf 'hook changed content\\n' > README.md\ngit add README.md\n",
        encoding="utf-8",
    )
    hook.chmod(0o755)

    outcome = fix_repository(
        catalog, repository, (file_rule("first", "first.txt", "first\n"),), "main"
    )

    assert outcome.commits == []
    assert any("staged commit" in message for _, message in outcome.failures)
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


def test_interruption_preserves_completed_commit_in_recovery_ref(
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

    refs = git("for-each-ref", "--format=%(refname)", "refs/repomod/recovery", cwd=path).stdout.splitlines()
    assert len(refs) == 1
    assert git("show", f"{refs[0]}:first.txt", cwd=path).stdout == "first\n"
    assert head(path) == base
    assert not (path / "first.txt").exists()
    assert len(git("worktree", "list", "--porcelain", cwd=path).stdout.split("worktree ")) == 2


def test_dirty_or_nondefault_checkouts_are_skipped_without_modification(
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
    assert branch.skipped == "checkout is on feature; default branch is main"
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
    assert len(subjects(path, base)) == 1


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
