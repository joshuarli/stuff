from __future__ import annotations

import fnmatch
import re
import uuid
from collections.abc import Mapping
from dataclasses import dataclass, field
from pathlib import Path, PurePosixPath
from typing import Any

from repomod.catalog import Catalog, repository_path
from repomod.git import GitError, head, repository_lock, run_git, status
from repomod.rules import rules as all_policy_rules
from repomod.types import Finding, Repository, Rule


@dataclass(slots=True)
class FixOutcome:
    repository: str
    commits: list[tuple[str, str]] = field(default_factory=list)
    failures: list[tuple[str, str]] = field(default_factory=list)
    skipped: str | None = None
    recovery_ref: str | None = None
    recovery_worktree: str | None = None
    cleanup_error: str | None = None


def _allowed(path: str, patterns: tuple[str, ...]) -> bool:
    return any(
        fnmatch.fnmatchcase(path, pattern)
        or (pattern.startswith("**/") and fnmatch.fnmatchcase(path, pattern.removeprefix("**/")))
        for pattern in patterns
    )


def _validate_path(root: Path, relative: str, patterns: tuple[str, ...]) -> Path:
    candidate = PurePosixPath(relative)
    if candidate.is_absolute() or not candidate.parts or ".." in candidate.parts or "\\" in relative:
        raise GitError(f"fixer returned an unsafe path: {relative!r}")
    normalized = candidate.as_posix()
    if normalized != relative or not _allowed(normalized, patterns):
        raise GitError(f"fixer attempted to change a path outside its allowed scope: {relative}")
    current = root
    for part in candidate.parts:
        current = current / part
        if current.is_symlink():
            raise GitError(f"fixer path contains a symlink: {relative}")
    if not current.resolve(strict=False).is_relative_to(root.resolve()):
        raise GitError(f"fixer path escapes its worktree: {relative}")
    return current


def _changed_paths(root: Path) -> tuple[str, ...]:
    output = run_git(
        ["status", "--porcelain=v1", "--untracked-files=all", "-z"], cwd=root
    ).stdout
    paths: list[str] = []
    records = output.split("\0")
    index = 0
    while index < len(records):
        record = records[index]
        index += 1
        if not record:
            continue
        if len(record) < 4:
            raise GitError("Git returned malformed porcelain status")
        paths.append(record[3:])
        if record[0] in {"R", "C"} or record[1] in {"R", "C"}:
            index += 1
    return tuple(sorted(set(paths), key=str.casefold))


def _staged_paths(root: Path) -> tuple[str, ...]:
    output = run_git(["diff", "--cached", "--name-only", "-z"], cwd=root).stdout
    return tuple(sorted({path for path in output.split("\0") if path}, key=str.casefold))


def _reset_failed_fixer(root: Path, start: str) -> None:
    run_git(["reset", "--hard", start], cwd=root)
    run_git(["clean", "-fdx"], cwd=root)


def _write_plan(
    root: Path,
    plan: Mapping[Any, Any],
    patterns: tuple[str, ...],
) -> tuple[str, ...]:
    changed: list[str] = []
    entries = []
    for path, content in plan.items():
        if not isinstance(path, str) or not isinstance(content, str):
            raise GitError("fixer returned a plan with a non-text path or file value")
        entries.append((path, content))
    for relative, content in sorted(entries, key=lambda entry: entry[0].casefold()):
        path = _validate_path(root, relative, patterns)
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("w", encoding="utf-8", newline="") as output:
            output.write(content)
        changed.append(relative)
    return tuple(changed)


def _commit_paths(root: Path, before: str, rule: Rule, paths: tuple[str, ...]) -> str:
    if not paths or rule.commit_message is None:
        raise GitError(f"fixer {rule.id} has no commit message or changed files")
    run_git(["add", "--", *paths], cwd=root)
    staged = _staged_paths(root)
    if not staged or any(path not in paths for path in staged):
        raise GitError(f"fixer {rule.id} staged files outside its planned changes")
    expected_tree = run_git(["write-tree"], cwd=root).stdout.strip()
    result = run_git(["commit", "-m", rule.commit_message], cwd=root, check=False)
    after = head(root)
    if result.returncode != 0:
        _reset_failed_fixer(root, before)
        detail = result.stderr.strip() or result.stdout.strip() or "git commit failed"
        raise GitError(detail)
    parent = run_git(["rev-parse", "HEAD^"], cwd=root, check=False)
    actual_tree = run_git(["rev-parse", "HEAD^{tree}"], cwd=root).stdout.strip()
    if parent.returncode or parent.stdout.strip() != before or actual_tree != expected_tree:
        _reset_failed_fixer(root, before)
        raise GitError(f"commit hook changed the staged commit for fixer {rule.id}")
    changed = run_git(
        ["diff-tree", "--no-commit-id", "--name-only", "-r", "-z", "HEAD"],
        cwd=root,
    ).stdout
    committed_paths = tuple(path for path in changed.split("\0") if path)
    if not committed_paths or any(path not in paths for path in committed_paths):
        _reset_failed_fixer(root, before)
        raise GitError(f"commit hook changed files outside fixer {rule.id}'s planned changes")
    return after


def _policy_findings(
    root: Path,
    repository: Repository,
    extra_rule: Rule,
) -> tuple[Finding, ...]:
    registered = {rule.id: rule for rule in all_policy_rules()}
    registered[extra_rule.id] = extra_rule
    findings = []
    for rule in registered.values():
        if rule.applies_to.matches(repository.tags):
            findings.extend(rule.check(root, repository))
    return tuple(findings)


def _finding_signature(finding: Finding) -> tuple[str, str | None, str, str, str]:
    return (
        finding.rule,
        finding.file,
        finding.message,
        finding.severity,
        finding.kind,
    )


def _manual_violations(findings: tuple[Finding, ...]) -> str | None:
    remaining = [
        finding
        for finding in findings
        if finding.severity == "error" and finding.kind == "violation" and not finding.fixable
    ]
    if not remaining:
        return None
    details = "; ".join(
        f"{finding.file or 'repository'}: {finding.message}"
        for finding in sorted(remaining, key=lambda item: (item.file or "", item.message))
    )
    return "policy violations need manual review: " + details


def _fix_one_rule(root: Path, repository: Repository, rule: Rule) -> tuple[str | None, str | None]:
    before = head(root)
    try:
        findings = rule.check(root, repository)
        actionable = tuple(finding for finding in findings if finding.fixable)
        if not actionable:
            return None, _manual_violations(tuple(findings))
        if rule.fix is None:
            return None, "rule has fixable findings but no fixer"
        if _changed_paths(root):
            return None, "disposable worktree was not clean before the fixer"
        baseline = {
            _finding_signature(finding)
            for finding in _policy_findings(root, repository, rule)
            if finding.severity == "error" and finding.kind == "violation"
        }
        plan = rule.fix(root, repository)
        if _changed_paths(root):
            raise GitError("fixer modified files directly instead of returning an explicit file plan")
        if not plan:
            return None, "fixer returned no changes for a fixable finding"
        planned = _write_plan(root, plan, rule.allowed_paths)
        changed = _changed_paths(root)
        if not changed:
            after_findings = rule.check(root, repository)
            if not any(finding.fixable for finding in after_findings):
                return None, None
            return None, "fixer produced no repository changes and the finding remains"
        if any(path not in planned for path in changed):
            raise GitError("fixer changed paths outside its explicit file plan")
        if any(not _allowed(path, rule.allowed_paths) for path in changed):
            raise GitError("fixer changed a path outside its declared scope")
        if rule.validate is not None:
            errors = rule.validate(root, repository, changed)
            if errors:
                raise GitError("; ".join(errors))
        introduced = [
            finding
            for finding in _policy_findings(root, repository, rule)
            if finding.severity == "error"
            and finding.kind == "violation"
            and _finding_signature(finding) not in baseline
        ]
        if introduced:
            details = "; ".join(
                f"{finding.rule}: {finding.file or 'repository'}: {finding.message}"
                for finding in introduced
            )
            raise GitError("fixer introduced policy violations: " + details)
        after_findings = rule.check(root, repository)
        if any(finding.fixable for finding in after_findings):
            raise GitError("fixer postcondition failed; fixable findings remain")
        new_head = _commit_paths(root, before, rule, changed)
        if _changed_paths(root):
            _reset_failed_fixer(root, before)
            raise GitError("commit hook left uncommitted changes in the disposable worktree")
        after_findings = tuple(rule.check(root, repository))
        if any(finding.fixable for finding in after_findings):
            _reset_failed_fixer(root, before)
            raise GitError("commit hook invalidated the fixer postcondition")
        return new_head, _manual_violations(after_findings)
    except (GitError, OSError, UnicodeError, ValueError, RuntimeError, TypeError) as error:
        _reset_failed_fixer(root, before)
        return None, str(error)


def _managed_worktree_root(catalog: Catalog) -> Path:
    path = catalog.root / ".repomod" / "worktrees"
    current = catalog.root
    for part in (".repomod", "worktrees"):
        current = current / part
        if current.is_symlink():
            raise GitError(f"repomod worktree path contains a symlink: {current}")
    path.mkdir(parents=True, exist_ok=True)
    return path


def _prior_worktrees(root: Path, repository: Repository) -> tuple[str, ...]:
    candidate_root = root / ".repomod" / "worktrees" / repository.id.owner / repository.id.name
    current = root
    for part in (".repomod", "worktrees", repository.id.owner, repository.id.name):
        current = current / part
        if current.is_symlink():
            raise GitError(f"repomod worktree path contains a symlink: {current}")
    if not candidate_root.is_dir():
        return ()
    return tuple(
        str(child)
        for child in sorted(candidate_root.iterdir(), key=lambda path: path.name)
        if re.fullmatch(r"[0-9a-f]{32}", child.name)
        and (child.exists() or child.is_symlink())
    )


def _recovery_reference(repository: Repository, run_id: str) -> str:
    owner = repository.id.owner.replace(".", "-")
    name = repository.id.name.replace(".", "-")
    return f"refs/repomod/recovery/{owner}/{name}/{run_id}"


def _recovery_references(root: Path, repository: Repository) -> tuple[str, ...]:
    owner = repository.id.owner.replace(".", "-")
    name = repository.id.name.replace(".", "-")
    prefix = f"refs/repomod/recovery/{owner}/{name}/"
    result = run_git(["for-each-ref", "--format=%(refname)", prefix], cwd=root)
    return tuple(line for line in result.stdout.splitlines() if line)


def _ref_points_to(root: Path, reference: str, expected: str) -> bool:
    result = run_git(["rev-parse", "--verify", reference], cwd=root, check=False)
    return result.returncode == 0 and result.stdout.strip() == expected


def _retain_worktree(outcome: FixOutcome, worktree: Path, reason: str) -> None:
    if outcome.recovery_worktree is None:
        outcome.recovery_worktree = str(worktree)
    if not any(rule == "recovery" for rule, _ in outcome.failures):
        outcome.failures.append(("recovery", reason))


def _remove_worktree(
    canonical: Path,
    worktree: Path,
    outcome: FixOutcome,
) -> None:
    try:
        result = run_git(
            ["worktree", "remove", "--force", str(worktree)],
            cwd=canonical,
            check=False,
        )
        if result.returncode:
            detail = result.stderr.strip() or "could not remove repomod worktree"
            outcome.cleanup_error = "; ".join(filter(None, (outcome.cleanup_error, detail)))
            if worktree.exists():
                outcome.recovery_worktree = str(worktree)
            return
    except Exception as error:
        outcome.cleanup_error = "; ".join(filter(None, (outcome.cleanup_error, str(error))))
        if worktree.exists():
            outcome.recovery_worktree = str(worktree)


def _protect_worktree(
    canonical: Path,
    worktree: Path,
    reference: str,
    baseline: str,
    outcome: FixOutcome,
) -> bool:
    try:
        worktree_head = head(worktree)
        canonical_head = head(canonical)
    except Exception as error:
        _retain_worktree(outcome, worktree, f"cannot verify interrupted worktree: {error}")
        return False
    if worktree_head in (baseline, canonical_head):
        outcome.recovery_worktree = None
        return True
    try:
        if _ref_points_to(canonical, reference, worktree_head):
            outcome.recovery_ref = reference
            outcome.recovery_worktree = None
            return True
    except Exception as error:
        _retain_worktree(outcome, worktree, f"cannot verify recovery ref {reference}: {error}")
        return False
    try:
        run_git(["update-ref", reference, worktree_head], cwd=canonical)
    except Exception as error:
        _retain_worktree(
            outcome,
            worktree,
            f"could not protect {worktree_head} with {reference}: {error}",
        )
        return False
    if not _ref_points_to(canonical, reference, worktree_head):
        _retain_worktree(
            outcome,
            worktree,
            f"recovery ref {reference} does not point to {worktree_head}",
        )
        return False
    outcome.recovery_ref = reference
    outcome.recovery_worktree = None
    return True


def fix_repository(
    catalog: Catalog,
    repository: Repository,
    selected: tuple[Rule, ...],
    default_branch: str,
) -> FixOutcome:
    outcome = FixOutcome(str(repository.id))
    worktree: Path | None = None
    canonical: Path | None = None
    start_head: str | None = None
    reference: str | None = None
    if not repository.autofix:
        outcome.skipped = repository.autofix_reason or "automatic fixes are disabled"
        return outcome
    try:
        canonical = repository_path(catalog, repository)
        initial = status(canonical, repository.id)
        if not initial.present or not initial.valid:
            outcome.skipped = initial.error or "checkout is unavailable"
            return outcome
        with repository_lock(catalog.root, repository.id):
            current = status(canonical, repository.id)
            if not current.present or not current.valid:
                outcome.skipped = current.error or "checkout is unavailable"
                return outcome
            if not current.origin_valid:
                outcome.skipped = current.error or "checkout origin is invalid"
                return outcome
            if current.dirty:
                outcome.skipped = "checkout is dirty"
                return outcome
            if current.detached:
                outcome.skipped = "checkout HEAD is detached"
                return outcome
            if current.branch != default_branch:
                outcome.skipped = f"checkout is on {current.branch}; default branch is {default_branch}"
                return outcome
            prior_refs = _recovery_references(canonical, repository)
            prior_worktrees = _prior_worktrees(catalog.root, repository)
            if prior_refs or prior_worktrees:
                if prior_refs:
                    outcome.recovery_ref = prior_refs[0]
                if prior_worktrees:
                    outcome.recovery_worktree = prior_worktrees[0]
                evidence = ", ".join((*prior_refs, *prior_worktrees))
                outcome.failures.append(
                    ("recovery", f"unresolved repomod recovery state: {evidence}")
                )
                return outcome
            start_head = head(canonical)
            worktree_root = _managed_worktree_root(catalog)
            run_id = uuid.uuid4().hex
            worktree_parent = worktree_root / repository.id.owner / repository.id.name
            current = catalog.root
            for part in (".repomod", "worktrees", repository.id.owner, repository.id.name):
                current = current / part
                if current.is_symlink():
                    raise GitError(f"repomod worktree path contains a symlink: {current}")
            worktree_parent.mkdir(parents=True, exist_ok=True)
            worktree = worktree_parent / run_id
            reference = _recovery_reference(repository, run_id)
            run_git(["worktree", "add", "--detach", str(worktree), start_head], cwd=canonical)
            try:
                for rule in selected:
                    if not rule.applies_to.matches(repository.tags):
                        continue
                    new_head, error = _fix_one_rule(worktree, repository, rule)
                    if new_head is not None:
                        outcome.commits.append((rule.id, new_head))
                        try:
                            run_git(["update-ref", reference, new_head], cwd=canonical)
                        except Exception as ref_error:
                            outcome.failures.append(("recovery", f"could not update {reference}: {ref_error}"))
                            break
                    if error is not None:
                        outcome.failures.append((rule.id, error))
                if outcome.commits:
                    final_head = outcome.commits[-1][1]
                    if not _ref_points_to(canonical, reference, final_head):
                        _retain_worktree(
                            outcome,
                            worktree,
                            f"recovery ref {reference} does not yet protect {final_head}",
                        )
                    else:
                        current = status(canonical, repository.id)
                        if (
                            current.dirty
                            or current.detached
                            or current.branch != default_branch
                            or not current.origin_valid
                            or head(canonical) != start_head
                        ):
                            outcome.recovery_ref = reference
                            outcome.failures.append(
                                (
                                    "publish",
                                    "canonical checkout changed during fixing; "
                                    "commits preserved under recovery ref",
                                )
                            )
                        else:
                            try:
                                run_git(["merge", "--ff-only", final_head], cwd=canonical)
                            except Exception as publish_error:
                                if _ref_points_to(canonical, reference, final_head):
                                    outcome.recovery_ref = reference
                                else:
                                    _retain_worktree(
                                        outcome,
                                        worktree,
                                        f"could not verify recovery ref {reference}; commits remain in the worktree",
                                    )
                                outcome.failures.append(("publish", str(publish_error)))
                            else:
                                try:
                                    run_git(["update-ref", "-d", reference], cwd=canonical)
                                except Exception as ref_error:
                                    if _ref_points_to(canonical, reference, final_head):
                                        outcome.recovery_ref = reference
                                    outcome.cleanup_error = (
                                        f"could not remove recovery ref {reference}: {ref_error}"
                                    )
            finally:
                if worktree.exists():
                    if start_head is None or reference is None:
                        _retain_worktree(
                            outcome,
                            worktree,
                            "worktree recovery metadata is unavailable",
                        )
                    else:
                        protected = _protect_worktree(
                            canonical, worktree, reference, start_head, outcome
                        )
                        if protected:
                            _remove_worktree(canonical, worktree, outcome)
            return outcome
    except Exception as error:
        if worktree is not None and worktree.exists():
            outcome.recovery_worktree = str(worktree)
        elif canonical is not None and reference is not None and outcome.commits:
            try:
                if _ref_points_to(canonical, reference, outcome.commits[-1][1]):
                    outcome.recovery_ref = reference
            except Exception as ref_error:
                outcome.cleanup_error = f"could not verify recovery ref {reference}: {ref_error}"
        outcome.failures.append(("repository", str(error)))
        return outcome


def push_repository(repository: Repository, path: Path) -> tuple[str, str | None]:
    current = status(path, repository.id)
    if not current.present or not current.valid:
        return "skipped", current.error or "checkout is unavailable"
    if not current.origin_valid:
        return "failed", current.error or "origin is invalid"
    if current.dirty:
        return "skipped", "checkout is dirty"
    if current.detached or current.branch is None:
        return "skipped", "HEAD is detached"
    if current.upstream is None or not current.upstream.startswith("origin/"):
        return "skipped", "branch has no origin upstream"
    if current.upstream.removeprefix("origin/") != current.branch:
        return "skipped", "upstream branch does not match the current branch"
    if current.behind is not None and current.behind > 0:
        return "failed", "branch is behind its last-fetched upstream; sync before pushing"
    if current.ahead is None or current.ahead == 0:
        return "skipped", "no local commits ahead of the last-fetched upstream"
    checked_head = head(path)
    confirmed = status(path, repository.id)
    if confirmed != current or head(path) != checked_head:
        return "skipped", "checkout changed before push"
    result = run_git(
        ["push", "origin", f"{checked_head}:refs/heads/{current.branch}"],
        cwd=path,
        check=False,
    )
    if result.returncode:
        return "failed", result.stderr.strip() or result.stdout.strip() or "git push failed"
    return "pushed", None
