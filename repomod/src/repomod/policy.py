from __future__ import annotations

import contextvars
from collections.abc import Iterable, Mapping, Sequence
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

from repomod.catalog import (
    Catalog,
    parse_banned_direct_ignore,
    parse_banned_transitive_ignore,
    parse_dependency_ignore,
    parse_git_source_ignore,
    parse_local_source_ignore,
    parse_lockfile_ignore,
    repository_path,
)
from repomod.github import open_pull_requests, protection_many, topics_many
from repomod.rules import rules
from repomod.rules.conventional_commits import titles_findings
from repomod.rules.mdtool import is_mdtool_code_ignore
from repomod.run_cache import run_cache
from repomod.types import Finding, OpenPullRequest, Repository, Rule, operational, violation
from repomod.workflow_checks import produced_check_names


class PolicyError(RuntimeError):
    pass


def selected_rules(
    *,
    rule_ids: Iterable[str] = (),
    local_only: bool = False,
) -> tuple[Rule, ...]:
    selected_ids = set(rule_ids)
    available = rules()
    known_ids = {rule.id for rule in available}
    unknown = selected_ids - known_ids
    if unknown:
        raise PolicyError(f"unknown rules: {', '.join(sorted(unknown))}")
    values = [
        rule
        for rule in available
        if (not selected_ids or rule.id in selected_ids)
    ]
    if local_only:
        values = [
        rule
        for rule in values
        if rule.id not in ("topics", "github-protection", "audit-upgrades", "audit-vuln", "conventional-commits")
    ]
    return tuple(values)


def validate_ignored_rules(repositories: Iterable[Repository]) -> None:
    known_ids = {rule.id for rule in rules()}
    for repository in repositories:
        unknown = {
            value
            for value in repository.ignored_rules - known_ids
            if not is_mdtool_code_ignore(value)
            and parse_dependency_ignore(value) is None
            and parse_banned_transitive_ignore(value) is None
            and parse_banned_direct_ignore(value) is None
            and parse_git_source_ignore(value) is None
            and parse_local_source_ignore(value) is None
            and parse_lockfile_ignore(value) is None
        }
        if unknown:
            raise PolicyError(
                f"unknown ignored rule codes for {repository.id}: {', '.join(sorted(unknown))}"
            )


def filter_repositories(
    repositories: Iterable[Repository],
    *,
    repo_values: Iterable[str] = (),
    tags: Iterable[str] = (),
) -> tuple[Repository, ...]:
    names = {value.casefold() for value in repo_values}
    selected_tags = set(tags)
    return tuple(
        repo
        for repo in repositories
        if (not names or str(repo.id).casefold() in names or repo.id.name.casefold() in names)
        and (not selected_tags or bool(set(repo.tags) & selected_tags))
    )


_BATCHED_RULE_IDS = frozenset({"topics", "github-protection", "conventional-commits"})
_RULE_PRIORITY = {
    "mdtool": 0,
    "audit-lock": 1,
    "audit-banned": 2,
    "audit-registry": 3,
    "gha-syntax": 4,
    "generated-artifacts": 5,
    "make-lint-target": 6,
    "language-tags": 7,
    "rust-project": 8,
    "registry-sources": 9,
}


def _check_rule(rule: Rule, root: Path, repository: Repository) -> list[Finding]:
    try:
        return rule.check(root, repository)
    except (OSError, ValueError, RuntimeError) as error:
        return [operational(repository, rule.id, None, f"rule could not complete: {error}")]


def _check_rule_in_context(
    context: contextvars.Context,
    rule: Rule,
    root: Path,
    repository: Repository,
) -> list[Finding]:
    return context.run(_check_rule, rule, root, repository)


def _batched_findings(
    rule: Rule,
    root: Path,
    repository: Repository,
    topic_results: Mapping[str, tuple[Sequence[str], Sequence[str], str | None]],
    protection_results: Mapping[str, tuple[bool, int, tuple[str, ...], str | None]],
    pull_results: Mapping[str, tuple[list[OpenPullRequest], str | None]],
) -> list[Finding] | None:
    if rule.id not in _BATCHED_RULE_IDS:
        return None
    name = str(repository.id)
    if rule.id == "topics":
        result = topic_results.get(name)
        if result is None:
            return [
                operational(repository, "topics", None, "cannot read GitHub topics: missing result")
            ]
        additions, removals, error = result
        if error is not None:
            return [
                operational(
                    repository, "topics", None, f"cannot read GitHub topics: {error}"
                )
            ]
        if not additions and not removals:
            return []
        details = []
        if additions:
            details.append(f"add {', '.join(additions)}")
        if removals:
            details.append(f"remove {', '.join(removals)}")
        return [violation(repository, "topics", None, "; ".join(details), fixable=False)]
    if rule.id == "github-protection":
        result = protection_results.get(name)
        if result is None:
            return [
                operational(
                    repository,
                    "github-protection",
                    None,
                    "cannot read branch protection: missing result",
                )
            ]
        has_protection, count, contexts, error = result
        if error is not None:
            return [
                operational(
                    repository,
                    "github-protection",
                    None,
                    f"cannot read branch protection: {error}",
                )
            ]
        if has_protection and count != 0:
            return [
                violation(
                    repository,
                    "github-protection",
                    None,
                    f"branch protection requires {count} approving review(s); "
                    "owner-only repositories must require none",
                    fixable=False,
                )
            ]
        produced = produced_check_names(root) if has_protection and contexts else None
        stale = sorted(set(contexts) - produced) if produced is not None else []
        if stale:
            return [
                violation(
                    repository,
                    "github-protection",
                    None,
                    "branch protection requires status checks no workflow produces: "
                    + ", ".join(stale),
                    fixable=False,
                )
            ]
        return []
    if rule.id == "conventional-commits":
        result = pull_results.get(name)
        if result is None:
            return [
                operational(
                    repository,
                    "conventional-commits",
                    None,
                    "cannot read open pull requests: missing result",
                )
            ]
        requests, error = result
        return titles_findings(
            repository,
            [
                (request.number, request.title, request.url, request.draft)
                for request in requests
            ],
            error,
        )
    return None


def lint_repositories(
    catalog: Catalog,
    repositories: Iterable[Repository] | None = None,
    *,
    tags: Iterable[str] = (),
    rule_ids: Iterable[str] = (),
    local_only: bool = False,
    checkouts: Mapping[str, Path] | None = None,
    workers: int = 1,
) -> list[Finding]:
    if workers < 1:
        raise ValueError("workers must be at least one")
    selected = filter_repositories(
        catalog.repositories if repositories is None else repositories,
        tags=tags,
    )
    validate_ignored_rules(selected)
    active_rules = selected_rules(rule_ids=rule_ids, local_only=local_only)
    rules_by_repository = {
        str(repository.id): tuple(
            rule for rule in active_rules if rule.id not in repository.ignored_rules
        )
        for repository in selected
    }
    findings: list[Finding] = []
    repositories_with_topics = tuple(
        repository
        for repository in selected
        if any(rule.id == "topics" for rule in rules_by_repository[str(repository.id)])
    )

    def fetch_topics() -> dict[str, tuple[Sequence[str], Sequence[str], str | None]]:
        return {
            name: (additions, removals, error)
            for name, _, additions, removals, error in topics_many(
                repositories_with_topics, apply=False, jobs=workers
            )
        }

    repositories_with_protection = tuple(
        repository
        for repository in selected
        if any(rule.id == "github-protection" for rule in rules_by_repository[str(repository.id)])
    )

    def fetch_protection() -> dict[str, tuple[bool, int, tuple[str, ...], str | None]]:
        return {
            name: (has_protection, count, contexts, error)
            for name, has_protection, count, contexts, error in protection_many(
                repositories_with_protection, jobs=workers
            )
        }

    repositories_with_pulls = tuple(
        repository
        for repository in selected
        if any(rule.id == "conventional-commits" for rule in rules_by_repository[str(repository.id)])
    )

    def fetch_pulls() -> dict[str, tuple[list[OpenPullRequest], str | None]]:
        return {
            name: (requests, error)
            for name, (requests, error) in open_pull_requests(repositories_with_pulls).items()
        }

    tasks: list[tuple[Rule, Path, Repository]] = []
    batched_tasks: list[tuple[Rule, Path, Repository]] = []
    for repository in selected:
        try:
            root = (checkouts or {}).get(str(repository.id)) or repository_path(catalog, repository)
        except (OSError, ValueError) as error:
            findings.append(
                operational(repository, "repo-integrity", None, f"cannot resolve canonical checkout: {error}")
            )
            continue
        for rule in rules_by_repository[str(repository.id)]:
            if rule.id in _BATCHED_RULE_IDS:
                if rule.applies_to.matches(repository.tags):
                    batched_tasks.append((rule, root, repository))
                continue
            if not rule.applies_to.matches(repository.tags):
                continue
            tasks.append((rule, root, repository))
    tasks.sort(key=lambda task: _RULE_PRIORITY.get(task[0].id, len(_RULE_PRIORITY)))
    with ThreadPoolExecutor(
        max_workers=3, thread_name_prefix="repomod-lint-fetch"
    ) as fetch_pool:
        topics_future = fetch_pool.submit(fetch_topics) if repositories_with_topics else None
        protection_future = (
            fetch_pool.submit(fetch_protection) if repositories_with_protection else None
        )
        pulls_future = fetch_pool.submit(fetch_pulls) if repositories_with_pulls else None
        with run_cache():
            if workers == 1:
                for rule, root, repository in tasks:
                    findings.extend(_check_rule(rule, root, repository))
            elif tasks:
                with ThreadPoolExecutor(
                    max_workers=min(workers, len(tasks)), thread_name_prefix="repomod-policy"
                ) as pool:
                    futures = [
                        pool.submit(_check_rule_in_context, contextvars.copy_context(), rule, root, repository)
                        for rule, root, repository in tasks
                    ]
                    for future in as_completed(futures):
                        findings.extend(future.result())
            topic_results = topics_future.result() if topics_future is not None else {}
            protection_results: dict[str, tuple[bool, int, tuple[str, ...], str | None]]
            protection_results = (
                protection_future.result() if protection_future is not None else {}
            )
            pull_results: dict[str, tuple[list[OpenPullRequest], str | None]]
            pull_results = pulls_future.result() if pulls_future is not None else {}
            for rule, root, repository in batched_tasks:
                batched = _batched_findings(
                    rule, root, repository, topic_results, protection_results, pull_results
                )
                findings.extend(batched or [])
    return sorted(
        findings,
        key=lambda finding: (
            finding.repository.casefold(),
            finding.rule,
            (finding.file or "").casefold(),
            finding.message,
        ),
    )


def finding_dict(finding: Finding) -> dict[str, str | bool | int | None]:
    return {
        "repository": finding.repository,
        "rule": finding.rule,
        "file": finding.file,
        "message": finding.message,
        "severity": finding.severity,
        "fixable": finding.fixable,
        "kind": finding.kind,
        "line": finding.line,
        "column": finding.column,
        "code": finding.code,
    }


def format_finding_short(finding: Finding) -> str:
    if finding.code is None:
        if finding.file is None:
            return f"{finding.rule} {finding.message}"
        if finding.line is None:
            return f"{finding.file} {finding.rule} {finding.message}"
        column = finding.column if finding.column is not None else 1
        return f"{finding.file} {finding.line}:{column} {finding.rule} {finding.message}"
    if finding.file is None:
        return finding.code
    if finding.line is None:
        return f"{finding.file} {finding.code}"
    column = finding.column if finding.column is not None else 1
    return f"{finding.file} {finding.line}:{column} {finding.code}"


def lint_legend(findings: Iterable[Finding]) -> list[tuple[str, str]]:
    seen: dict[str, str] = {}
    for finding in findings:
        if finding.code is not None and finding.code not in seen:
            seen[finding.code] = finding.message
    return sorted(seen.items())
