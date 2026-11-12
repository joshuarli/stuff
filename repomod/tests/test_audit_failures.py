from __future__ import annotations

import argparse
import io
import json
from pathlib import Path
from typing import Any

import pytest
from git_helpers import make_catalog

from repomod import audit
from repomod.catalog import repository_path
from repomod.cmd import common
from repomod.cmd.lint import _lint
from repomod.policy import lint_repositories


def write_project(root: Path, ecosystem: str, *, invalid: bool = False) -> None:
    root.mkdir(parents=True)
    packages = ''.join(f'[[package]]\nname = "transitive"\nversion = "{version}"\n'
                       for version in ("2.0.0", "1.0.0", "1.0.0"))
    if ecosystem == "pypi":
        (root / "pyproject.toml").write_text('[project]\nname = "demo"\nversion = "1.0.0"\n'
                                            'dependencies = ["transitive>=2"]\n')
        (root / "uv.lock").write_text("this is not TOML" if invalid else packages)
    elif ecosystem == "crates":
        (root / "Cargo.toml").write_text('[package]\nname = "demo"\nversion = "1.0.0"\n'
                                        '[dependencies]\ntransitive = "2"\n')
        (root / "Cargo.lock").write_text("this is not TOML" if invalid else packages)
    else:
        (root / "package.json").write_text('{"dependencies":{"transitive":"2.0.0"}}')
        (root / "pnpm-lock.yaml").write_text("[invalid:" if invalid else
            "lockfileVersion: '9.0'\npackages:\n  transitive@2.0.0:\n    resolution: {}\n"
            "  transitive@1.0.0:\n    resolution: {}\nsnapshots:\n  transitive@1.0.0: {}\n")


@pytest.mark.parametrize("ecosystem", ["pypi", "crates", "npm"])
def test_lint_queries_every_locked_version(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, ecosystem: str,
) -> None:
    catalog, repository = make_catalog(tmp_path)
    write_project(repository_path(catalog, repository), ecosystem)
    monkeypatch.setattr(audit, "_audit_cache_path", lambda: tmp_path / "cache.json")
    queries: list[dict[str, Any]] = []

    def urlopen(request, **_kwargs):
        batch = json.loads(request.data)["queries"]
        queries.extend(batch)
        return io.BytesIO(json.dumps({"results": [
            {"vulns": [{"id": "OSV-second-version"}]} if item["version"] == "1.0.0" else {}
            for item in batch
        ]}).encode())

    monkeypatch.setattr(audit.urllib.request, "urlopen", urlopen)
    findings = lint_repositories(catalog, rule_ids=("audit-vuln",))
    assert sorted(item["version"] for item in queries) == ["1.0.0", "2.0.0"]
    assert len(findings) == 1
    assert findings[0].kind == "violation"
    assert "1.0.0 has OSV-second-version" in findings[0].message


@pytest.mark.parametrize("ecosystem", ["pypi", "crates", "npm"])
@pytest.mark.parametrize("rules", [("audit-lock",), ("audit-vuln",), ("audit-lock", "audit-vuln")])
def test_invalid_lockfiles_are_operational_errors_through_lint_cli(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, ecosystem: str, rules: tuple[str, ...],
) -> None:
    catalog, repository = make_catalog(tmp_path)
    write_project(repository_path(catalog, repository), ecosystem, invalid=True)
    monkeypatch.setattr(common, "default_catalog", lambda: catalog)
    monkeypatch.setattr(audit.urllib.request, "urlopen",
                        lambda *_args, **_kwargs: pytest.fail("no OSV on parse failure"))
    findings = lint_repositories(catalog, rule_ids=rules)
    assert {finding.rule for finding in findings} == set(rules)
    assert all(finding.kind == "operational" for finding in findings)
    assert _lint(argparse.Namespace(repo=[], tag=[], include_private=True, rule=rules, json=True)) == 2


@pytest.mark.parametrize("payload", [b"not json", b"{}", b'{"results":[]}',
    b'{"results":[{},{}]}', b'{"results":[null]}', b'{"results":[{"vulns":null}]}',
    b'{"results":[{"vulns":[null]}]}', b'{"results":[{"vulns":[{}]}]}',
    b'{"results":[{"error":"failed"}]}', b'{"results":[{"next_page_token":"more"}]}'])
def test_osv_invalid_results_never_update_cached_evidence(
    monkeypatch: pytest.MonkeyPatch, payload: bytes,
) -> None:
    monkeypatch.setattr(audit.urllib.request, "urlopen", lambda *_args, **_kwargs: io.BytesIO(payload))
    cache = {"osv": {"pypi:demo:1.0.0": {"vulns": [{"id": "OSV-old"}], "fetched_at": 1.0}}}
    before = json.dumps(cache, sort_keys=True)
    with pytest.raises(audit.VulnerabilityLookupError):
        audit.resolve_vulns([audit.LockedDependency("pypi", "demo", "1.0.0")],
                            offline=False, refresh=True, cache=cache, now=100000.0)
    assert json.dumps(cache, sort_keys=True) == before


def test_osv_transport_failure_is_an_operational_lint_error(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    catalog, repository = make_catalog(tmp_path)
    write_project(repository_path(catalog, repository), "pypi")
    monkeypatch.setattr(audit, "_audit_cache_path", lambda: tmp_path / "cache.json")
    monkeypatch.setattr(common, "default_catalog", lambda: catalog)

    def fail(*_args, **_kwargs):
        raise OSError("offline")

    monkeypatch.setattr(audit.urllib.request, "urlopen", fail)
    assert _lint(argparse.Namespace(repo=[], tag=[], include_private=True,
                                   rule=("audit-vuln",), json=True)) == 2
    cache = json.loads((tmp_path / "cache.json").read_text())
    assert cache["osv"] == {}
