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
from repomod.fix import FixOutcome, _finding_signature, fix_repository, push_fix_branch, push_repository
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


def subjects(path: Path, base: str, target: str = "HEAD") -> list[str]:
    output = git("log", "--format=%s", f"{base}..{target}", cwd=path).stdout
    return output.splitlines()


def test_action_pin_findings_keep_identity_when_pinact_resolves_a_sha_version() -> None:
    unresolved = Finding(
        "acme/demo",
        "gha-pin",
        ".github/workflows/ci.yml",
        "line 18: actions/checkout: cannot identify the major version of this SHA",
    )
    resolved = Finding(
        "acme/demo",
        "gha-pin",
        ".github/workflows/ci.yml",
        "line 20: actions/checkout: expected actions/checkout@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )
    other_action = Finding(
        "acme/demo",
        "gha-pin",
        ".github/workflows/ci.yml",
        "line 20: actions/download-artifact: expected "
        "actions/download-artifact@bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    )

    assert _finding_signature(unresolved) == _finding_signature(resolved)
    assert _finding_signature(resolved) != _finding_signature(other_action)


def test_fix_branch_keeps_commits_off_default_and_resumes_repeated_fixes(
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

    assert [rule for rule, _ in outcome.commits] == ["first", "second", "third"]
    assert outcome.branch is not None
    assert len(subjects(path, base, outcome.branch)) == 3
    assert subjects(path, base, outcome.branch) == [
        rule.commit_message for rule in reversed(selected)
    ]
    assert git("rev-parse", f"refs/heads/{outcome.branch}", cwd=path).stdout.strip() == outcome.commits[-1][1]
    assert git("show", f"{outcome.branch}:first.txt", cwd=path).stdout == "first\n"
    assert head(path) == base
    assert not status(path, repository.id).dirty
    assert outcome.failures == []

    repeated = fix_repository(catalog, repository, selected, "main")

    assert repeated.commits == []
    assert repeated.branch == outcome.branch
    assert repeated.failures == []
    assert repeated.branch_head == outcome.commits[-1][1]
    assert head(path) == base


def test_diverged_fix_branch_blocks_until_verified_merged(
    tmp_path: Path,
    git_environment: None,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    catalog, repository, path, _base = initialize_repository(tmp_path)
    selected = (file_rule("first", "first.txt", "first\n"),)

    initial = fix_repository(catalog, repository, selected, "main")
    assert initial.branch is not None
    (path / "main.txt").write_text("main advanced independently\n", encoding="utf-8")
    git("add", "main.txt", cwd=path)
    git("commit", "-m", "advance main", cwd=path)
    monkeypatch.setattr(fix_module, "fix_branch_was_merged", lambda _repository, _branch: False)

    blocked = fix_repository(catalog, repository, selected, "main")

    assert blocked.commits == []
    assert blocked.failures == [
        (
            "branch",
            f"fix branch {initial.branch} diverged from the current default branch; "
            "integrate or delete it before fixing",
        )
    ]
    monkeypatch.setattr(fix_module, "fix_branch_was_merged", lambda _repository, _branch: True)

    resumed = fix_repository(catalog, repository, selected, "main")

    assert resumed.failures == []
    assert resumed.branch is not None
    assert resumed.branch != initial.branch
    assert [rule for rule, _ in resumed.commits] == ["first"]


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
    assert outcome.branch is not None
    assert not (path / "second.txt").exists()
    assert git("show", f"{outcome.branch}:first.txt", cwd=path).stdout == "first\n"
    assert git("show", f"{outcome.branch}:third.txt", cwd=path).stdout == "third\n"
    assert len(subjects(path, base, outcome.branch)) == 2
    assert head(path) == base


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
    assert outcome.branch is not None
    assert git("show", f"{outcome.branch}:safe.txt", cwd=path).stdout == "safe\n"
    assert len(subjects(path, base, outcome.branch)) == 1
    assert head(path) == base


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


@pytest.mark.parametrize(
    ("after", "expected_commits"),
    [("prelude\nbad\n", 1), ("prelude\nbad\nbad\n", 0)],
)
def test_policy_fixer_compares_line_findings_by_content_and_multiplicity(
    tmp_path: Path,
    git_environment: None,
    after: str,
    expected_commits: int,
) -> None:
    _catalog, repository, path, _base = initialize_repository(tmp_path)
    policy_file = path / "policy.txt"
    policy_file.write_text("bad\n", encoding="utf-8")
    git("add", "policy.txt", cwd=path)
    git("commit", "-m", "add policy fixture", cwd=path)

    def check(root: Path, repo: Repository) -> list[Finding]:
        source = (root / "policy.txt").read_text(encoding="utf-8")
        findings = [
            Finding(
                str(repo.id),
                "line-policy",
                "policy.txt",
                f"line {line_number}: existing lint rule: existing diagnostic",
            )
            for line_number, line in enumerate(source.splitlines(), start=1)
            if line == "bad"
        ]
        if not source.startswith("prelude\n"):
            findings.append(
                Finding(
                    str(repo.id),
                    "line-policy",
                    "policy.txt",
                    "add policy prelude",
                    fixable=True,
                )
            )
        return findings

    rule = Rule(
        "line-policy",
        "Normalize line-based diagnostics",
        TagPredicate(),
        check,
        lambda _root, _repo: {"policy.txt": after},
        "chore(repomod): normalize policy fixture",
        ("policy.txt",),
    )

    outcome = fix_repository(_catalog, repository, (rule,), "main")

    assert len(outcome.commits) == expected_commits
    if expected_commits:
        assert outcome.failures
        assert all("policy violations need manual review" in message for _, message in outcome.failures)
    else:
        assert any("fixer introduced policy violations" in message for _, message in outcome.failures)
    assert policy_file.read_text(encoding="utf-8") == "bad\n"


def test_legacy_recovery_ref_blocks_new_branch_fix(
    tmp_path: Path,
    git_environment: None,
) -> None:
    catalog, repository, path, base = initialize_repository(tmp_path)
    reference = "refs/repomod/recovery/acme/demo/saved"
    git("update-ref", reference, base, cwd=path)
    selected = (file_rule("first", "first.txt", "first\n"),)

    outcome = fix_repository(catalog, repository, selected, "main")

    assert outcome.commits == []
    assert outcome.recovery_ref == reference
    assert outcome.failures
    assert head(path) == base


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
