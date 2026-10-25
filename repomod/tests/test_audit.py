from __future__ import annotations

from pathlib import Path

from repomod.audit import (
    _collect_locked,
    _is_banned_rust,
    _is_outdated,
    _js_spec_is_filesystem,
    _pep508_is_filesystem,
    _pep508_name,
    _pnpm_locked_versions,
    _policy_findings,
    _version_key,
    audit_repositories,
)
from repomod.catalog import Catalog
from repomod.types import RepoId, Repository


def repository(*tags: str) -> Repository:
    return Repository(RepoId("acme", "demo"), tuple(tags))


def catalog_for(root: Path, repository: Repository) -> Catalog:
    return Catalog(Path("catalog.toml"), root, "acme", (repository,))


def repo_root(tmp_path: Path) -> Path:
    root = tmp_path / "demo"
    root.mkdir(exist_ok=True)
    return root


def write_python(root: Path, *, lock: bool = True, requirements: bool = False) -> None:
    (root / "pyproject.toml").write_text(
        '[project]\nname = "demo"\nversion = "1.0.0"\nrequires-python = ">=3.15"\n'
        'dependencies = ["requests>=2.0"]\n',
        encoding="utf-8",
    )
    if lock:
        (root / "uv.lock").write_text(
            'version = 1\nrequires-python = ">=3.15"\n'
            '[[package]]\nname = "requests"\nversion = "2.0.0"\n',
            encoding="utf-8",
        )
    if requirements:
        (root / "requirements.txt").write_text("requests==2.0.0\n", encoding="utf-8")


def test_version_key_orders_numeric_releases() -> None:
    assert _version_key("1.10.0") > _version_key("1.9.0")
    assert _version_key("v2.0.0") == _version_key("2.0.0")
    assert _is_outdated("1.0.0", "1.0.1")
    assert not _is_outdated("1.0.1", "1.0.1")
    assert not _is_outdated("2.0.0", "1.9.9")


def test_pep508_name_and_filesystem() -> None:
    assert _pep508_name("requests>=2.0") == "requests"
    assert _pep508_name("my-pkg @ file:./local") == "my-pkg"
    assert _pep508_is_filesystem("demo @ file:./local")
    assert _pep508_is_filesystem("demo @ git+https://example.com/repo.git")
    assert not _pep508_is_filesystem("requests>=2.0")


def test_js_filesystem_specs() -> None:
    assert _js_spec_is_filesystem("file:../local")
    assert _js_spec_is_filesystem("link:./other")
    assert _js_spec_is_filesystem("workspace:*")
    assert _js_spec_is_filesystem("git+https://example.com/repo.git")
    assert not _js_spec_is_filesystem("^4.0.0")


def test_banned_rust_matches_variants() -> None:
    assert _is_banned_rust("clap")
    assert _is_banned_rust("clap_builder")
    assert _is_banned_rust("tokio")
    assert _is_banned_rust("tokio-util")
    assert not _is_banned_rust("lexopt")


def test_python_missing_lock_and_requirements(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    write_python(root, lock=False, requirements=True)
    repo = repository("tool", "python")
    findings = _policy_findings(root, repo)
    codes = {(finding.rule, finding.file) for finding in findings}
    assert ("audit-lock", "uv.lock") in codes
    assert any(rule == "audit-banned" and (file or "").startswith("requirements") for rule, file in codes)


def test_python_git_source_is_registry_violation(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    (root / "pyproject.toml").write_text(
        '[project]\nname = "demo"\nversion = "1.0.0"\ndependencies = ["shadowdriver"]\n'
        '[tool.uv.sources]\nshadowdriver = { git = "https://example.com/repo.git" }\n',
        encoding="utf-8",
    )
    (root / "uv.lock").write_text(
        'version = 1\n[[package]]\nname = "shadowdriver"\nversion = "0.1.0"\n',
        encoding="utf-8",
    )
    findings = _policy_findings(root, repository("tool", "python"))
    assert any(finding.rule == "audit-registry" for finding in findings)


def test_rust_bans_direct_and_transitive(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    (root / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "0.1.0"\nedition = "2024"\n'
        '[dependencies]\nclap = "4.0"\nlocal = { path = "crates/local" }\n'
        'remote = { git = "https://example.com/repo.git" }\n',
        encoding="utf-8",
    )
    (root / "Cargo.lock").write_text(
        'version = 4\n[[package]]\nname = "demo"\nversion = "0.1.0"\n'
        '[[package]]\nname = "clap"\nversion = "4.0.0"\n'
        '[[package]]\nname = "tokio"\nversion = "1.0.0"\n',
        encoding="utf-8",
    )
    findings = _policy_findings(root, repository("tool", "rust"))
    rules = [finding.rule for finding in findings]
    assert rules.count("audit-registry") >= 2
    assert any("clap" in finding.message for finding in findings if finding.rule == "audit-banned")
    assert any("tokio" in finding.message for finding in findings if finding.rule == "audit-banned")


def test_rust_versioned_path_is_registry_compatible(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    (root / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "0.1.0"\nedition = "2024"\n'
        '[dependencies]\ngrep = { version = "0.4.1", path = "crates/grep" }\n',
        encoding="utf-8",
    )
    findings = _policy_findings(root, repository("tool", "rust"))
    assert not [finding for finding in findings if finding.rule == "audit-registry"]


def test_pnpm_locked_versions_parses_importers(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    (root / "package.json").write_text('{"dependencies":{"effect":"4.0.6"}}\n', encoding="utf-8")
    (root / "pnpm-lock.yaml").write_text(
        "lockfileVersion: '9.0'\n\nimporters:\n\n  .:\n    dependencies:\n"
        "      effect:\n        specifier: 4.0.6\n        version: 4.0.6(effect@4.0.6)\n",
        encoding="utf-8",
    )
    assert _pnpm_locked_versions(root) == {"effect": "4.0.6"}


def test_audit_outdated_and_vuln_with_fake_fetchers(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    write_python(root)
    repo = repository("tool", "python")
    catalog = catalog_for(tmp_path, repo)

    def latest(ecosystem: str, name: str) -> str | None:
        assert (ecosystem, name) == ("pypi", "requests")
        return "2.1.0"

    def osv(queries: list[dict[str, object]]) -> list[list[dict[str, object]]]:
        assert len(queries) == 1
        typed: list[list[dict[str, object]]] = [[{"id": "GHSA-1", "summary": "bad"}]]
        return typed

    outcomes = audit_repositories(
        catalog,
        (repo,),
        offline=False,
        cache_path=tmp_path / "cache.json",
        now=1000.0,
        latest_fetcher=latest,
        osv_fetcher=osv,  # type: ignore[arg-type]
    )
    assert len(outcomes) == 1
    rules = [finding.rule for finding in outcomes[0].findings]
    assert "audit-outdated" in rules
    assert "audit-vuln" in rules


def test_audit_offline_uses_cache_without_network(tmp_path: Path) -> None:
    import json

    root = repo_root(tmp_path)
    write_python(root)
    repo = repository("tool", "python")
    catalog = catalog_for(tmp_path, repo)
    cache = tmp_path / "cache.json"
    cache.write_text(
        json.dumps(
            {
                "latest": {
                    "pypi:requests:2.0.0": {"version": "9.9.9", "fetched_at": 1000.0},
                },
                "osv": {},
            }
        ),
        encoding="utf-8",
    )

    outcomes = audit_repositories(
        catalog,
        (repo,),
        offline=True,
        cache_path=cache,
        now=1100.0,
    )
    assert any(finding.rule == "audit-outdated" for finding in outcomes[0].findings)


def test_go_replace_is_banned(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    (root / "go.mod").write_text(
        "module example.com/demo\n\ngo 1.27\nrequire example.com/other v1.0.0\n"
        "replace example.com/other => ../other\n",
        encoding="utf-8",
    )
    findings = _policy_findings(root, repository("tool", "go"))
    assert sum(1 for finding in findings if finding.rule == "audit-registry") == 1


def test_collect_locked_prefers_lockfile_versions(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    write_python(root)
    locked, _ = _collect_locked(root, repository("tool", "python"))
    assert [(item.ecosystem, item.name, item.locked) for item in locked] == [
        ("pypi", "requests", "2.0.0")
    ]
