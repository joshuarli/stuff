from __future__ import annotations

from collections.abc import Callable, Iterable
from dataclasses import dataclass
from pathlib import Path
from typing import Literal, override

type Severity = Literal["error", "warning", "info"]
type CiState = Literal[
    "passed",
    "failed",
    "pending",
    "cancelled",
    "skipped",
    "neutral",
    "none",
    "unknown",
    "stale",
]
type FixPlan = dict[str, str]


@dataclass(frozen=True, order=True, slots=True)
class RepoId:
    owner: str
    name: str

    @classmethod
    def parse(cls, value: str) -> RepoId:
        parts = value.split("/")
        if len(parts) != 2:
            raise ValueError(f"expected OWNER/REPO, got {value!r}")
        return cls(parts[0], parts[1])

    @override
    def __str__(self) -> str:
        return f"{self.owner}/{self.name}"


@dataclass(frozen=True, slots=True)
class Repository:
    id: RepoId
    tags: tuple[str, ...]
    autofix: bool = True
    autofix_reason: str | None = None
    ignored_rules: frozenset[str] = frozenset()


@dataclass(frozen=True, slots=True)
class Catalog:
    path: Path
    root: Path
    personal_owner: str
    repositories: tuple[Repository, ...]


@dataclass(frozen=True, slots=True)
class Finding:
    repository: str
    rule: str
    file: str | None
    message: str
    severity: Severity = "error"
    fixable: bool = False
    kind: Literal["violation", "operational"] = "violation"
    line: int | None = None
    column: int | None = None
    code: str | None = None


@dataclass(frozen=True, slots=True)
class TagPredicate:
    all_of: frozenset[str] = frozenset()
    any_of: frozenset[str] = frozenset()

    def matches(self, tags: Iterable[str]) -> bool:
        values = set(tags)
        return self.all_of <= values and (not self.any_of or bool(self.any_of & values))

    def describe(self) -> str:
        parts = []
        if self.all_of:
            parts.append("all " + ",".join(sorted(self.all_of)))
        if self.any_of:
            parts.append("any " + ",".join(sorted(self.any_of)))
        return "; ".join(parts) or "all"


@dataclass(frozen=True, slots=True)
class Rule:
    id: str
    description: str
    applies_to: TagPredicate
    check: Callable[[Path, Repository], list[Finding]]
    fix: Callable[[Path, Repository], FixPlan] | None
    commit_message: str | None
    allowed_paths: tuple[str, ...]
    validate: Callable[[Path, Repository, tuple[str, ...]], list[str]] | None = None


@dataclass(frozen=True, slots=True)
class GitStatus:
    repository: str
    path: Path
    present: bool
    valid: bool
    branch: str | None
    detached: bool
    staged: int
    unstaged: int
    untracked: int
    ahead: int | None
    behind: int | None
    upstream: str | None
    origin_valid: bool
    error: str | None = None

    @property
    def dirty(self) -> bool:
        return bool(self.staged or self.unstaged or self.untracked)


@dataclass(frozen=True, slots=True)
class RemoteRepository:
    repository: str
    default_branch: str | None
    sha: str | None
    archived: bool | None
    error: str | None = None


@dataclass(frozen=True, slots=True)
class CheckResult:
    name: str
    state: str
    url: str | None
    source: str | None = None
    updated_at: str | None = None
    check_suite_id: str | None = None
    workflow_run_id: str | None = None
    run_attempt: int | None = None
    workflow_name: str | None = None


@dataclass(frozen=True, slots=True)
class WorkflowStatus:
    name: str
    state: CiState
    url: str | None = None
    path: str | None = None


@dataclass(frozen=True, slots=True)
class CiResult:
    repository: str
    default_branch: str | None
    sha: str | None
    state: CiState
    checks: tuple[CheckResult, ...]
    cached: bool = False
    age_seconds: int | None = None
    error: str | None = None
    workflows: tuple[WorkflowStatus, ...] | None = None
