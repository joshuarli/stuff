from __future__ import annotations

import time
from collections.abc import Callable
from dataclasses import replace
from pathlib import Path

from repomod.audit import (
    LockedDependency,
    VulnerabilityLookupError,
    _collect_locked,
    _collect_vuln_candidates,
    _policy_findings,
    audit_cache,
    resolve_vulns,
)
from repomod.run_cache import cached
from repomod.types import Finding, Repository, Rule, TagPredicate, operational, violation


def _collection(root: Path, repository: Repository) -> tuple[list[LockedDependency], list[Finding]]:
    return cached(
        ("audit_collection", root, repository),
        lambda: _collect_locked(root, repository),
    )


def _policy_rule(
    rule_id: str, *, skip_forks: bool = False
) -> Callable[[Path, Repository], list[Finding]]:
    def check(root: Path, repository: Repository) -> list[Finding]:
        if skip_forks and "fork" in repository.tags:
            return []
        findings = cached(
            ("audit_policy", root, str(repository.id), repository.tags, repository.ignored_rules),
            lambda: _policy_findings(root, repository),
        )
        selected = [finding for finding in findings if finding.rule == rule_id]
        if rule_id == "audit-lock":
            _, problems = _collection(root, repository)
            selected.extend(problems)
        return selected

    return check


def _vuln_check(root: Path, repository: Repository) -> list[Finding]:
    locked, problems = _collection(root, repository)
    if problems:
        return [replace(finding, rule="audit-vuln") for finding in problems]
    candidates = _collect_vuln_candidates(root, repository, locked)
    if not candidates:
        return []
    findings: list[Finding] = []
    with audit_cache() as cache:
        try:
            vulns = resolve_vulns(
                candidates, offline=False, refresh=False, cache=cache, now=time.time(),
            )
        except VulnerabilityLookupError as error:
            findings.append(operational(repository, "audit-vuln", None, str(error)))
            vulns = resolve_vulns(
                candidates, offline=True, refresh=False, cache=cache, now=time.time(),
            )
    for item in sorted(
        candidates, key=lambda entry: (entry.ecosystem, entry.name.casefold(), entry.locked)
    ):
        for vuln in vulns.get((item.ecosystem, item.name, item.locked), []):
            vuln_id = vuln.get("id", "unknown")
            summary = vuln.get("summary", "")
            detail = f" {summary}" if isinstance(summary, str) and summary else ""
            message = f"{item.ecosystem}:{item.name} {item.locked} has {vuln_id}{detail}"
            findings.append(violation(repository, "audit-vuln", None, message))
    return findings


def audit_finding_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            "audit-lock",
            "Require the lockfile each managed ecosystem needs",
            TagPredicate(),
            _policy_rule("audit-lock"),
            None,
            None,
            (),
        ),
        Rule(
            "audit-banned",
            "Reject banned dependencies and package managers",
            TagPredicate(),
            _policy_rule("audit-banned", skip_forks=True),
            None,
            None,
            (),
        ),
        Rule(
            "audit-registry",
            "Reject filesystem dependencies and warn on git dependencies",
            TagPredicate(),
            _policy_rule("audit-registry"),
            None,
            None,
            (),
        ),
        Rule(
            "audit-vuln",
            "Report locked dependencies with known OSV vulnerabilities",
            TagPredicate(),
            _vuln_check,
            None,
            None,
            (),
        ),
    )
