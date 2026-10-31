from __future__ import annotations

import contextlib
import time
from collections.abc import Callable
from pathlib import Path

from repomod.audit import (
    _audit_cache_path,
    _collect_locked,
    _collect_vuln_candidates,
    _policy_findings,
    _read_cache,
    _resolve_vulns,
    _write_cache,
)
from repomod.types import Finding, Repository, Rule, TagPredicate, violation

_AUDITED = frozenset({"python", "typescript", "javascript", "rust", "go"})


def _policy_rule(rule_id: str) -> Callable[[Path, Repository], list[Finding]]:
    def check(root: Path, repository: Repository) -> list[Finding]:
        return [
            finding
            for finding in _policy_findings(root, repository)
            if finding.rule == rule_id
        ]

    return check


def _vuln_check(root: Path, repository: Repository) -> list[Finding]:
    cache = _read_cache(_audit_cache_path())
    if not isinstance(cache.get("osv"), dict):
        cache["osv"] = {}
    locked, _ = _collect_locked(root, repository)
    if not locked:
        return []
    candidates = _collect_vuln_candidates(root, repository, locked)
    vulns = _resolve_vulns(
        candidates,
        offline=False,
        refresh=False,
        cache=cache,
        now=time.time(),
    )
    with contextlib.suppress(OSError):
        _write_cache(_audit_cache_path(), cache)
    findings: list[Finding] = []
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
            TagPredicate(any_of=_AUDITED),
            _policy_rule("audit-lock"),
            None,
            None,
            (),
        ),
        Rule(
            "audit-banned",
            "Reject banned dependencies and package managers",
            TagPredicate(any_of=_AUDITED),
            _policy_rule("audit-banned"),
            None,
            None,
            (),
        ),
        Rule(
            "audit-registry",
            "Reject filesystem and git dependencies",
            TagPredicate(any_of=_AUDITED),
            _policy_rule("audit-registry"),
            None,
            None,
            (),
        ),
        Rule(
            "audit-vuln",
            "Report locked dependencies with known OSV vulnerabilities",
            TagPredicate(any_of=_AUDITED),
            _vuln_check,
            None,
            None,
            (),
        ),
    )
