from __future__ import annotations

from collections.abc import Iterable

from repomod.catalog import Catalog, repository_path
from repomod.github import protection_many, topics_many
from repomod.rules import rules
from repomod.types import Finding, Repository, Rule, operational, violation


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
        values = [rule for rule in values if rule.id not in ("topics", "github-protection", "audit-upgrades")]
    return tuple(values)


def validate_ignored_rules(repositories: Iterable[Repository]) -> None:
    known_ids = {rule.id for rule in rules()}
    for repository in repositories:
        unknown = repository.ignored_rules - known_ids
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


def lint_repositories(
    catalog: Catalog,
    repositories: Iterable[Repository] | None = None,
    *,
    tags: Iterable[str] = (),
    rule_ids: Iterable[str] = (),
    local_only: bool = False,
) -> list[Finding]:
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
    topic_results = {}
    repositories_with_topics = tuple(
        repository
        for repository in selected
        if any(rule.id == "topics" for rule in rules_by_repository[str(repository.id)])
    )
    if repositories_with_topics:
        topic_results = {
            name: (additions, removals, error)
            for name, _, additions, removals, error in topics_many(
                repositories_with_topics, apply=False
            )
        }
    protection_results: dict[str, tuple[bool, int, str | None]] = {}
    repositories_with_protection = tuple(
        repository
        for repository in selected
        if any(rule.id == "github-protection" for rule in rules_by_repository[str(repository.id)])
    )
    if repositories_with_protection:
        protection_results = {
            name: (has_protection, count, error)
            for name, has_protection, count, error in protection_many(
                repositories_with_protection
            )
        }
    for repository in selected:
        try:
            root = repository_path(catalog, repository)
        except (OSError, ValueError) as error:
            findings.append(
                operational(repository, "repo-integrity", None, f"cannot resolve canonical checkout: {error}")
            )
            continue
        for rule in rules_by_repository[str(repository.id)]:
            if not rule.applies_to.matches(repository.tags):
                continue
            if rule.id == "topics":
                result = topic_results.get(str(repository.id))
                if result is None:
                    findings.append(
                        operational(repository, "topics", None, "cannot read GitHub topics: missing result")
                    )
                    continue
                additions, removals, error = result
                if error is not None:
                    findings.append(
                        operational(repository, "topics", None, f"cannot read GitHub topics: {error}")
                    )
                    continue
                if additions or removals:
                    details = []
                    if additions:
                        details.append(f"add {', '.join(additions)}")
                    if removals:
                        details.append(f"remove {', '.join(removals)}")
                    findings.append(
                        violation(repository, "topics", None, "; ".join(details), fixable=False)
                    )
                continue
            if rule.id == "github-protection":
                result = protection_results.get(str(repository.id))
                if result is None:
                    findings.append(
                        operational(
                            repository,
                            "github-protection",
                            None,
                            "cannot read branch protection: missing result",
                        )
                    )
                    continue
                has_protection, count, error = result
                if error is not None:
                    findings.append(
                        operational(repository, "github-protection", None, f"cannot read branch protection: {error}")
                    )
                    continue
                if has_protection and count != 0:
                    findings.append(
                        violation(
                            repository,
                            "github-protection",
                            None,
                            f"branch protection requires {count} approving review(s); "
                            "owner-only repositories must require none",
                            fixable=False,
                        )
                    )
                continue
            try:
                findings.extend(rule.check(root, repository))
            except (OSError, ValueError, RuntimeError) as error:
                findings.append(
                    operational(repository, rule.id, None, f"rule could not complete: {error}")
                )
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
