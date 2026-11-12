from __future__ import annotations

import re
from pathlib import Path

from repomod.types import Finding, Repository, Rule, TagPredicate
from repomod.types import operational as _operational
from repomod.types import violation as _violation

_RULE_ID = "conventional-commits"

_CONVENTIONAL_TYPES = (
    "feat",
    "fix",
    "perf",
    "refactor",
    "docs",
    "test",
    "build",
    "ci",
    "chore",
    "revert",
)

_SUBJECT = re.compile(
    r"^(?P<type>[a-z]+)(?P<scope>\([^()\n]+\))?(?P<breaking>!)?(?P<separator>: )(?P<description>\S.*)$"
)
_SCOPE_INNER = re.compile(r"^\((?P<inner>[^()\n]+)\)$")


def _subject_problem(title: str) -> str | None:
    text = title.strip()
    if not text:
        return "PR title is empty; use a Conventional Commit subject like 'fix(scope): what changed'"
    match = _SUBJECT.fullmatch(text)
    if match is None:
        return (
            f"PR title {title!r} is not a Conventional Commit subject "
            f"(type[(scope)][!]: description with type in {', '.join(_CONVENTIONAL_TYPES)})"
        )
    if match.group("type") not in _CONVENTIONAL_TYPES:
        return (
            f"PR title type {match.group('type')!r} is not conventional "
            f"(use one of {', '.join(_CONVENTIONAL_TYPES)})"
        )
    scope = match.group("scope")
    if scope is not None:
        inner = _SCOPE_INNER.fullmatch(scope)
        if inner is None or not inner.group("inner").strip():
            return f"PR title scope {scope!r} must be a non-empty name"
    description = match.group("description")
    if description.endswith("."):
        return "PR title description must not end with a period"
    return None


def _placeholder(root: Path, repository: Repository) -> list[Finding]:
    return []


def titles_findings(
    repository: Repository,
    pulls: list[tuple[int, str, str, bool]],
    error: str | None,
) -> list[Finding]:
    if error is not None:
        return [_operational(repository, _RULE_ID, None, f"cannot read open pull requests: {error}")]
    findings: list[Finding] = []
    for number, title, url, draft in sorted(pulls):
        if draft:
            continue
        problem = _subject_problem(title)
        if problem is not None:
            findings.append(_violation(repository, _RULE_ID, None, f"PR #{number} {url}: {problem}"))
            continue
        if len(title.strip()) > 72:
            findings.append(
                _violation(
                    repository,
                    _RULE_ID,
                    None,
                    f"PR #{number} {url}: title exceeds 72 characters",
                    severity="warning",
                )
            )
    return findings


def conventional_commits_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            "conventional-commits",
            "Require Conventional Commit subjects on open pull requests",
            TagPredicate(),
            _placeholder,
            None,
            None,
            (),
        ),
    )
