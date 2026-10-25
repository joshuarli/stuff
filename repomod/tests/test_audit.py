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
from repomod.github import UpstreamStatus
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
    messages = [finding.message for finding in findings if finding.rule == "audit-registry"]
    assert any(
        "'shadowdriver'" in message and "git" in message and "registry version" in message
        for message in messages
    )


def test_js_registry_message_names_spec_and_remedy(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    (root / "package.json").write_text(
        '{"dependencies":{"left-pad":"file:../left-pad"}}\n', encoding="utf-8"
    )
    (root / "pnpm-lock.yaml").write_text("lockfileVersion: '9.0'\n", encoding="utf-8")
    findings = _policy_findings(root, repository("tool", "typescript"))
    messages = [finding.message for finding in findings if finding.rule == "audit-registry"]
    assert any(
        "'left-pad'" in message and "file:../left-pad" in message and "registry version" in message
        for message in messages
    )


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


def test_rust_self_dependency_on_root_is_exempt(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    (root / "Cargo.toml").write_text(
        '[package]\nname = "rawzip"\nversion = "0.5.1"\nedition = "2024"\n',
        encoding="utf-8",
    )
    bench = root / "bench"
    bench.mkdir()
    (bench / "Cargo.toml").write_text(
        '[package]\nname = "rawzip-bench"\nversion = "0.0.1"\nedition = "2024"\n'
        '[dependencies]\nrawzip = { path = ".." }\n',
        encoding="utf-8",
    )
    (root / "Cargo.lock").write_text(
        'version = 4\n[[package]]\nname = "rawzip"\nversion = "0.5.1"\n'
        '[[package]]\nname = "rawzip-bench"\nversion = "0.0.1"\n',
        encoding="utf-8",
    )
    findings = _policy_findings(root, repository("tool", "rust"))
    assert [finding for finding in findings if finding.rule == "audit-registry"] == []
    locked, _ = _collect_locked(root, repository("tool", "rust"))
    assert all(not (item.ecosystem == "crates" and item.name == "rawzip") for item in locked)


def test_rust_relative_crate_without_version_is_exempt(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    (root / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "0.1.0"\nedition = "2024"\n'
        '[workspace]\nmembers = ["crates/foo", "crates/bar"]\n',
        encoding="utf-8",
    )
    foo = root / "crates" / "foo"
    foo.mkdir(parents=True)
    (foo / "Cargo.toml").write_text(
        '[package]\nname = "foo"\nversion = "0.1.0"\nedition = "2024"\n',
        encoding="utf-8",
    )
    bar = root / "crates" / "bar"
    bar.mkdir(parents=True)
    (bar / "Cargo.toml").write_text(
        '[package]\nname = "bar"\nversion = "0.1.0"\nedition = "2024"\n'
        '[dependencies]\nfoo = { path = "../foo" }\n',
        encoding="utf-8",
    )
    (root / "Cargo.lock").write_text(
        'version = 4\n[[package]]\nname = "demo"\nversion = "0.1.0"\n'
        '[[package]]\nname = "foo"\nversion = "0.1.0"\n'
        '[[package]]\nname = "bar"\nversion = "0.1.0"\n',
        encoding="utf-8",
    )
    findings = _policy_findings(root, repository("tool", "rust"))
    assert [finding for finding in findings if finding.rule == "audit-registry"] == []
    locked, _ = _collect_locked(root, repository("tool", "rust"))
    assert all(not (item.ecosystem == "crates" and item.name == "foo") for item in locked)


def test_rust_workspace_inherited_relative_is_exempt_from_banned(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    (root / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "0.1.0"\nedition = "2024"\n'
        '[workspace]\nmembers = ["crates/foo"]\n'
        '[workspace.dependencies]\n'
        'hyper = { package = "laputa-hyper-futures-lite", version = "1.12.0", '
        'path = "crates/hyper-fork" }\n'
        '[dependencies]\nhyper = { workspace = true }\n',
        encoding="utf-8",
    )
    fork = root / "crates" / "hyper-fork"
    fork.mkdir(parents=True)
    (fork / "Cargo.toml").write_text(
        '[package]\nname = "laputa-hyper-futures-lite"\nversion = "1.12.0"\nedition = "2021"\n',
        encoding="utf-8",
    )
    foo = root / "crates" / "foo"
    foo.mkdir(parents=True)
    (foo / "Cargo.toml").write_text(
        '[package]\nname = "foo"\nversion = "0.1.0"\nedition = "2024"\n'
        '[dependencies]\nhyper = { workspace = true }\n',
        encoding="utf-8",
    )
    (root / "Cargo.lock").write_text(
        'version = 4\n[[package]]\nname = "demo"\nversion = "0.1.0"\n'
        '[[package]]\nname = "laputa-hyper-futures-lite"\nversion = "1.12.0"\n',
        encoding="utf-8",
    )
    findings = _policy_findings(root, repository("tool", "rust"))
    assert [finding for finding in findings if finding.rule == "audit-banned"] == []
    assert [finding for finding in findings if finding.rule == "audit-registry"] == []


def test_js_relative_link_is_exempt(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    ts = root / "ts"
    ts.mkdir()
    (ts / "package.json").write_text('{"name":"shadowdriver","version":"0.1.0"}\n', encoding="utf-8")
    (ts / "pnpm-lock.yaml").write_text("lockfileVersion: '9.0'\n", encoding="utf-8")
    app = root / "grab"
    app.mkdir()
    (app / "package.json").write_text(
        '{"dependencies":{"shadowdriver":"link:../ts"}}\n', encoding="utf-8"
    )
    (app / "pnpm-lock.yaml").write_text("lockfileVersion: '9.0'\n", encoding="utf-8")
    findings = _policy_findings(root, repository("tool", "typescript"))
    assert [finding for finding in findings if finding.rule == "audit-registry"] == []


def test_rust_registry_message_names_mechanism_and_remedy(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    (root / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "0.1.0"\nedition = "2024"\n'
        '[dependencies]\nlocal = { path = "crates/local" }\n'
        'remote = { git = "https://example.com/repo.git" }\n',
        encoding="utf-8",
    )
    findings = _policy_findings(root, repository("tool", "rust"))
    messages = [finding.message for finding in findings if finding.rule == "audit-registry"]
    assert any(
        "'local'" in message and 'path "crates/local"' in message and "registry version" in message
        for message in messages
    )
    assert any(
        "'remote'" in message and "git" in message and "registry version" in message
        for message in messages
    )


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


def _upstream_status(
    behind_by: int | None = 7,
    ahead_by: int | None = 1,
    state: str = "diverged",
    error: str | None = None,
) -> UpstreamStatus:
    return UpstreamStatus(
        "acme/demo",
        "cli/cli",
        "trunk",
        "b" * 40,
        "c" * 40,
        behind_by,
        ahead_by,
        state,
        False,
        None,
        error,
    )


def _upstream_outcomes(tmp_path: Path, statuses: dict[str, UpstreamStatus]):
    repo_root(tmp_path)
    repo = repository("tool", "fork")
    catalog = catalog_for(tmp_path, repo)
    return audit_repositories(
        catalog,
        (repo,),
        offline=True,
        cache_path=tmp_path / "cache.json",
        now=1000.0,
        upstream_statuses=statuses,
    )


def test_audit_upstream_behind_is_reported(tmp_path: Path) -> None:
    outcomes = _upstream_outcomes(tmp_path, {"acme/demo": _upstream_status()})
    rules = [finding.rule for finding in outcomes[0].findings]
    assert rules == ["audit-upstream"]
    assert outcomes[0].findings[0].message == (
        "fork is 7 commits behind cli/cli@trunk, 1 ahead"
    )


def test_audit_upstream_ahead_only_is_not_reported(tmp_path: Path) -> None:
    outcomes = _upstream_outcomes(
        tmp_path, {"acme/demo": _upstream_status(behind_by=0, ahead_by=3, state="ahead")}
    )
    assert outcomes[0].findings == []


def test_audit_upstream_in_sync_is_not_reported(tmp_path: Path) -> None:
    outcomes = _upstream_outcomes(
        tmp_path, {"acme/demo": _upstream_status(behind_by=0, ahead_by=0, state="identical")}
    )
    assert outcomes[0].findings == []


def test_audit_upstream_respects_ignored_rules(tmp_path: Path) -> None:
    repo_root(tmp_path)
    repo = Repository(RepoId("acme", "demo"), ("tool", "fork"), ignored_rules=frozenset({"audit-upstream"}))
    catalog = catalog_for(tmp_path, repo)
    outcomes = audit_repositories(
        catalog,
        (repo,),
        offline=True,
        cache_path=tmp_path / "cache.json",
        now=1000.0,
        upstream_statuses={"acme/demo": _upstream_status()},
    )
    assert outcomes[0].findings == []


def test_audit_upstream_error_is_operational(tmp_path: Path) -> None:
    outcomes = _upstream_outcomes(
        tmp_path,
        {"acme/demo": _upstream_status(behind_by=None, ahead_by=None, state="unknown", error="boom")},
    )
    findings = outcomes[0].findings
    assert len(findings) == 1
    assert findings[0].rule == "audit-upstream"
    assert findings[0].kind == "operational"
    assert "boom" in findings[0].message


def test_audit_without_upstream_statuses_skips_upstream(tmp_path: Path) -> None:
    repo_root(tmp_path)
    repo = repository("tool", "python")
    catalog = catalog_for(tmp_path, repo)
    outcomes = audit_repositories(
        catalog, (repo,), offline=True, cache_path=tmp_path / "cache.json", now=1000.0
    )
    assert all(finding.rule != "audit-upstream" for finding in outcomes[0].findings)
