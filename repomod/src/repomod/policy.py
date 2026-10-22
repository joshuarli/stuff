from __future__ import annotations

from collections.abc import Iterable

from repomod.catalog import Catalog, repository_path
from repomod.github import topics_many
from repomod.rules import rules
from repomod.types import Finding, Repository, Rule


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
        and (not local_only or rule.id != "gha-syntax")
    ]
    if local_only:
        values = [rule for rule in values if rule.id != "topics"]
    return tuple(values)


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
    active_rules = selected_rules(rule_ids=rule_ids, local_only=local_only)
    findings: list[Finding] = []
    topic_results = {}
    if any(rule.id == "topics" for rule in active_rules):
        topic_results = {
            name: (additions, removals, error)
            for name, _, additions, removals, error in topics_many(selected, apply=False)
        }
    for repository in selected:
        try:
            root = repository_path(catalog, repository)
        except (OSError, ValueError) as error:
            findings.append(
                Finding(
                    str(repository.id),
                    "repo-integrity",
                    None,
                    f"cannot resolve canonical checkout: {error}",
                    kind="operational",
                )
            )
            continue
        for rule in active_rules:
            if not rule.applies_to.matches(repository.tags):
                continue
            if rule.id == "topics":
                result = topic_results.get(str(repository.id))
                if result is None:
                    findings.append(
                        Finding(
                            str(repository.id),
                            "topics",
                            None,
                            "cannot read GitHub topics: missing result",
                            kind="operational",
                        )
                    )
                    continue
                additions, removals, error = result
                if error is not None:
                    findings.append(
                        Finding(
                            str(repository.id),
                            "topics",
                            None,
                            f"cannot read GitHub topics: {error}",
                            kind="operational",
                        )
                    )
                    continue
                if additions or removals:
                    details = []
                    if additions:
                        details.append(f"add {', '.join(additions)}")
                    if removals:
                        details.append(f"remove {', '.join(removals)}")
                    findings.append(
                        Finding(str(repository.id), "topics", None, "; ".join(details), fixable=False)
                    )
                continue
            try:
                findings.extend(rule.check(root, repository))
            except (OSError, ValueError, RuntimeError) as error:
                findings.append(
                    Finding(str(repository.id), rule.id, None, f"rule could not complete: {error}", kind="operational")
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


def finding_dict(finding: Finding) -> dict[str, str | bool | None]:
    return {
        "repository": finding.repository,
        "rule": finding.rule,
        "file": finding.file,
        "message": finding.message,
        "severity": finding.severity,
        "fixable": finding.fixable,
        "kind": finding.kind,
    }
