from __future__ import annotations

from pathlib import Path

import pytest

from repomod.audit import _collect_locked, _collect_vuln_candidates
from repomod.policy import selected_rules
from repomod.rules import rules
from repomod.rules.audit_findings import _policy_rule, _vuln_check
from repomod.types import RepoId, Repository


def repository(*tags: str) -> Repository:
    return Repository(RepoId("acme", "demo"), tuple(tags))


def write_rust(root: Path, name: str = "demo") -> None:
    (root / "Cargo.toml").write_text(
        f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2024"\n'
        '[dependencies]\nserde = "1"\n',
        encoding="utf-8",
    )
    (root / "Cargo.lock").write_text(
        f'version = 4\n[[package]]\nname = "{name}"\nversion = "0.1.0"\n'
        '[[package]]\nname = "serde"\nversion = "1.0.0"\n',
        encoding="utf-8",
    )


def test_audit_rules_are_registered_and_selectable() -> None:
    registered = {rule.id for rule in rules()}
    assert {
        "audit-lock",
        "audit-banned",
        "audit-registry",
        "audit-vuln",
    } <= registered
    offline = {rule.id for rule in selected_rules(local_only=True)}
    assert "audit-vuln" not in offline
    assert {"audit-lock", "audit-banned", "audit-registry"} <= offline


def test_policy_rule_reports_only_its_rule(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    (root / "pyproject.toml").write_text(
        '[project]\nname = "demo"\nversion = "1.0.0"\ndependencies = []\n',
        encoding="utf-8",
    )
    findings = _policy_rule("audit-lock")(root, repository("tool", "python"))
    assert findings
    assert {finding.rule for finding in findings} == {"audit-lock"}


def test_local_crate_name_collision_is_not_a_vulnerability_candidate(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    write_rust(root, name="json")
    locked, _ = _collect_locked(root, repository("tool", "rust"))
    candidates = _collect_vuln_candidates(root, repository("tool", "rust"), locked)
    names = [item.name for item in candidates]
    assert "json" not in names
    assert names == ["serde"]


def test_registry_audit_honors_catalog_source_ignore(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    (root / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "0.1.0"\nedition = "2024"\n'
        '[dependencies]\nfoo = { git = "https://example.invalid/foo" }\n',
        encoding="utf-8",
    )
    ignored = Repository(
        RepoId("acme", "demo"),
        ("tool", "rust"),
        ignored_rules=frozenset({"registry-sources:git:foo"}),
    )
    assert _policy_rule("audit-registry")(root, ignored) == []
    findings = _policy_rule("audit-registry")(root, repository("tool", "rust"))
    assert [finding.rule for finding in findings] == ["audit-registry"]
    assert "foo" in findings[0].message


def test_banned_transitive_ignore_suppresses_only_the_named_transitive_crate(
    tmp_path: Path,
) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    (root / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "0.1.0"\nedition = "2024"\n'
        '[dependencies]\nserde = "1"\n',
        encoding="utf-8",
    )
    (root / "Cargo.lock").write_text(
        'version = 4\n[[package]]\nname = "demo"\nversion = "0.1.0"\n'
        '[[package]]\nname = "serde"\nversion = "1.0.0"\n'
        '[[package]]\nname = "tokio"\nversion = "1.54.1"\n'
        '[[package]]\nname = "tokio-macros"\nversion = "2.7.2"\n',
        encoding="utf-8",
    )
    plain = repository("tool", "rust")
    ignored = Repository(
        RepoId("acme", "demo"),
        ("tool", "rust"),
        ignored_rules=frozenset({"audit-banned:transitive:tokio"}),
    )

    assert [
        finding.message
        for finding in _policy_rule("audit-banned")(root, plain)
    ] == [
        "remove tokio 1.54.1; use smolith instead",
        "remove tokio-macros 2.7.2; use smolith instead",
    ]
    assert [
        finding.message
        for finding in _policy_rule("audit-banned")(root, ignored)
    ] == ["remove tokio-macros 2.7.2; use smolith instead"]


def test_banned_transitive_ignore_does_not_hide_a_direct_dependency(
    tmp_path: Path,
) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    (root / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "0.1.0"\nedition = "2024"\n'
        '[dependencies]\ntokio = "1"\n',
        encoding="utf-8",
    )
    (root / "Cargo.lock").write_text(
        'version = 4\n[[package]]\nname = "demo"\nversion = "0.1.0"\n'
        '[[package]]\nname = "tokio"\nversion = "1.54.1"\n',
        encoding="utf-8",
    )
    ignored = Repository(
        RepoId("acme", "demo"),
        ("tool", "rust"),
        ignored_rules=frozenset({"audit-banned:transitive:tokio"}),
    )

    assert [
        finding.message
        for finding in _policy_rule("audit-banned")(root, ignored)
    ] == ["remove tokio; use smolith instead"]


def test_banned_direct_ignore_suppresses_the_direct_dependency_and_its_lock_entry(
    tmp_path: Path,
) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    (root / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "0.1.0"\nedition = "2024"\n'
        '[dependencies]\ntokio = "1"\n',
        encoding="utf-8",
    )
    (root / "Cargo.lock").write_text(
        'version = 4\n[[package]]\nname = "demo"\nversion = "0.1.0"\n'
        '[[package]]\nname = "tokio"\nversion = "1.54.1"\n'
        '[[package]]\nname = "tokio-macros"\nversion = "2.7.2"\n',
        encoding="utf-8",
    )
    ignored = Repository(
        RepoId("acme", "demo"),
        ("tool", "rust"),
        ignored_rules=frozenset({"audit-banned:direct:tokio"}),
    )

    assert [
        finding.message
        for finding in _policy_rule("audit-banned")(root, ignored)
    ] == ["remove tokio-macros 2.7.2; use smolith instead"]


def test_vuln_check_reports_osv_entries(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    write_rust(root)

    def vulns(candidates, **kwargs):
        return {
            ("crates", "serde", "1.0.0"): [
                {"id": "RUSTSEC-2026-0000", "summary": "example advisory"}
            ]
        }

    monkeypatch.setattr("repomod.rules.audit_findings.resolve_vulns", vulns)
    findings = _vuln_check(root, repository("tool", "rust"))
    assert [finding.rule for finding in findings] == ["audit-vuln"]
    assert "RUSTSEC-2026-0000 example advisory" in findings[0].message


def test_osv_informational_advisories_are_not_vulnerabilities(monkeypatch) -> None:
    from repomod.audit import LockedDependency, resolve_vulns

    advisory = {"id": "RUSTSEC-2026-0441", "modified": "2026-01-01"}
    detail = {
        "id": advisory["id"],
        "affected": [{"database_specific": {"informational": "unmaintained"}}],
    }
    monkeypatch.setattr("repomod.audit._http_json", lambda url: detail)
    candidates = [LockedDependency("crates", "pin-utils", "0.1.1")]
    cache = {"osv": {"crates:pin-utils:0.1.1": {"vulns": [advisory], "fetched_at": 1000.0}}}
    result = resolve_vulns(candidates, offline=False, refresh=False, cache=cache, now=1000.0)
    assert result[("crates", "pin-utils", "0.1.1")] == []


def test_osv_keeps_security_advisories_and_fails_when_classification_is_unavailable(monkeypatch) -> None:
    import pytest

    from repomod.audit import LockedDependency, resolve_vulns

    advisory = {"id": "RUSTSEC-2026-1234"}
    candidate = LockedDependency("crates", "example", "1.0.0")
    cache = {"osv": {"crates:example:1.0.0": {"vulns": [advisory], "fetched_at": 1000.0}}}
    monkeypatch.setattr("repomod.audit._http_json", lambda url: None)
    with pytest.raises(RuntimeError, match="could not classify OSV advisory"):
        resolve_vulns([candidate], offline=False, refresh=False, cache=cache, now=1000.0)
    security = {"id": advisory["id"], "affected": [{"database_specific": {"informational": None}}]}
    monkeypatch.setattr("repomod.audit._http_json", lambda url: security)
    result = resolve_vulns([candidate], offline=False, refresh=False, cache=cache, now=1000.0)
    assert result[("crates", "example", "1.0.0")] == [security]
