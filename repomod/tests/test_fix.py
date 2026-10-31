from __future__ import annotations

from pathlib import Path

import pytest
from fix_helpers import file_rule, git, initialize_repository, subjects

import repomod.fix as fix_module
from repomod.fix import fix_repository
from repomod.git import head, status
from repomod.types import Finding, Repository, Rule, TagPredicate


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
    assert any("workflow" in message for _, message in outcome.failures), outcome.failures
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
