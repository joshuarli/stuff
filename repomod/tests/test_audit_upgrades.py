from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest

from repomod.audit import _cargo_req_satisfies, _policy_findings, _upgrade_tasks
from repomod.policy import selected_rules
from repomod.rules import rules
from repomod.rules.audit_upgrades import _fix, _outdated_findings, audit_upgrade_rules
from repomod.types import RepoId, Repository


def repository(*tags: str) -> Repository:
    return Repository(RepoId("acme", "demo"), tuple(tags))


def repo_root(tmp_path: Path) -> Path:
    root = tmp_path / "demo"
    root.mkdir(exist_ok=True)
    return root


@pytest.fixture
def git_environment(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    global_config = tmp_path / "gitconfig"
    global_config.write_text(
        "[user]\n\tname = Test User\n\temail = test@example.invalid\n",
        encoding="utf-8",
    )
    monkeypatch.setenv("GIT_CONFIG_GLOBAL", str(global_config))
    monkeypatch.setenv("GIT_CONFIG_NOSYSTEM", "1")
    monkeypatch.setenv("GIT_TERMINAL_PROMPT", "0")


def write_python(root: Path, version: str = "2.0.0") -> None:
    (root / "pyproject.toml").write_text(
        '[project]\nname = "demo"\nversion = "1.0.0"\nrequires-python = ">=3.15"\n'
        'dependencies = ["requests>=2.0"]\n',
        encoding="utf-8",
    )
    (root / "uv.lock").write_text(
        'version = 1\nrequires-python = ">=3.15"\n'
        f'[[package]]\nname = "requests"\nversion = "{version}"\n',
        encoding="utf-8",
    )


def test_rule_is_registered_and_selectable() -> None:
    assert any(rule.id == "audit-upgrades" for rule in rules())
    assert any(rule.id == "audit-upgrades" for rule in audit_upgrade_rules())
    selected = selected_rules(rule_ids=["audit-upgrades"])
    assert [rule.id for rule in selected] == ["audit-upgrades"]
    offline = selected_rules(local_only=True)
    assert all(rule.id != "audit-upgrades" for rule in offline)


def test_check_reports_fixable_outdated() -> None:
    import tempfile

    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        write_python(root)
        repo = repository("tool", "python")

        def latest(ecosystem: str, name: str) -> str | None:
            assert (ecosystem, name) == ("pypi", "requests")
            return "2.1.0"

        findings = _outdated_findings(root, repo, {"latest": {}, "osv": {}}, 1000.0, latest)
        assert len(findings) == 1
        finding = findings[0]
        assert finding.rule == "audit-upgrades"
        assert finding.fixable
        assert "2.0.0" in finding.message
        assert "2.1.0" in finding.message


def test_check_reports_operational_when_unresolvable() -> None:
    import tempfile

    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        write_python(root)
        repo = repository("tool", "python")
        findings = _outdated_findings(root, repo, {"latest": {}, "osv": {}}, 1000.0, lambda _e, _n: None)
        assert len(findings) == 1
        assert findings[0].kind == "operational"
        assert "pypi:requests" in findings[0].message


def test_check_is_clean_when_current() -> None:
    import tempfile

    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        write_python(root, version="2.1.0")
        repo = repository("tool", "python")
        findings = _outdated_findings(
            root, repo, {"latest": {}, "osv": {}}, 1000.0, lambda _e, _n: "2.1.0"
        )
        assert findings == []


def test_check_skips_non_registry_sources(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    (root / "pyproject.toml").write_text(
        '[project]\nname = "demo"\nversion = "1.0.0"\ndependencies = ["shadowdriver @ git+https://example.com/repo.git"]\n',
        encoding="utf-8",
    )
    (root / "uv.lock").write_text(
        'version = 1\n[[package]]\nname = "shadowdriver"\nversion = "0.1.0"\n',
        encoding="utf-8",
    )
    repo = repository("tool", "python")
    findings = _outdated_findings(root, repo, {"latest": {}, "osv": {}}, 1000.0, lambda _e, _n: None)
    assert findings == []


def test_check_skips_uv_sources_overrides(tmp_path: Path) -> None:
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
    repo = repository("tool", "python")
    findings = _outdated_findings(root, repo, {"latest": {}, "osv": {}}, 1000.0, lambda _e, _n: None)
    assert findings == []


def test_fixer_returns_lockfile_plan_without_touching_worktree(
    tmp_path: Path, monkeypatch
) -> None:
    root = repo_root(tmp_path)
    write_python(root)
    repo = repository("tool", "python")
    before = (root / "uv.lock").read_text(encoding="utf-8")
    monkeypatch.setattr("repomod.rules.audit_upgrades.shutil.which", lambda _name: "/usr/bin/uv")

    def fake_run(argv, *, cwd, env, capture_output, text, encoding, errors, timeout, check):
        assert argv == ["uv", "lock", "--upgrade"]
        assert env["UV_CACHE_DIR"]
        lock = Path(cwd) / "uv.lock"
        lock.write_text(lock.read_text(encoding="utf-8").replace('"2.0.0"', '"2.1.0"'), encoding="utf-8")
        return subprocess.CompletedProcess(argv, 0, "", "")

    monkeypatch.setattr("repomod.rules.audit_upgrades.subprocess.run", fake_run)
    plan = _fix(root, repo)
    assert set(plan) == {"uv.lock"}
    assert '"2.1.0"' in plan["uv.lock"]
    assert (root / "uv.lock").read_text(encoding="utf-8") == before


def test_fixer_plan_clears_check(tmp_path: Path, monkeypatch) -> None:
    root = repo_root(tmp_path)
    write_python(root)
    repo = repository("tool", "python")
    monkeypatch.setattr("repomod.rules.audit_upgrades.shutil.which", lambda _name: "/usr/bin/uv")

    def fake_run(argv, *, cwd, env, capture_output, text, encoding, errors, timeout, check):
        lock = Path(cwd) / "uv.lock"
        lock.write_text(lock.read_text(encoding="utf-8").replace('"2.0.0"', '"2.1.0"'), encoding="utf-8")
        return subprocess.CompletedProcess(argv, 0, "", "")

    monkeypatch.setattr("repomod.rules.audit_upgrades.subprocess.run", fake_run)
    plan = _fix(root, repo)
    for relative, content in plan.items():
        (root / relative).write_text(content, encoding="utf-8")
    findings = _outdated_findings(
        root, repo, {"latest": {}, "osv": {}}, 1000.0, lambda _e, _n: "2.1.0"
    )
    assert [finding for finding in findings if finding.fixable] == []


def test_fixer_requires_tool(tmp_path: Path, monkeypatch) -> None:
    import pytest

    root = repo_root(tmp_path)
    write_python(root)
    monkeypatch.setattr("repomod.rules.audit_upgrades.shutil.which", lambda _name: None)
    with pytest.raises(RuntimeError, match="uv"):
        _fix(root, repository("tool", "python"))


def test_upgrade_tasks_cover_nested_js_projects(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    project = root / "ts"
    project.mkdir()
    (project / "package.json").write_text('{"dependencies":{}}\n', encoding="utf-8")
    (project / "pnpm-lock.yaml").write_text("lockfileVersion: '9.0'\n", encoding="utf-8")
    tasks = _upgrade_tasks(root, repository("tool", "typescript"))
    assert [(task.directory, task.argv, task.tool) for task in tasks] == [
        (project, ["pnpm", "update", "--latest"], "pnpm")
    ]


def test_upgrade_tasks_cover_go_tidy(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    (root / "go.mod").write_text("module example.com/demo\n\ngo 1.27\n", encoding="utf-8")
    tasks = _upgrade_tasks(root, repository("tool", "go"))
    assert [task.argv for task in tasks] == [["go", "get", "-u", "./..."], ["go", "mod", "tidy"]]


def test_yarn_artifacts_are_banned(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    (root / "package.json").write_text(
        '{"packageManager":"yarn@4.0.0","dependencies":{"left-pad":"file:../left-pad"}}\n',
        encoding="utf-8",
    )
    (root / "pnpm-lock.yaml").write_text("lockfileVersion: '9.0'\n", encoding="utf-8")
    (root / "yarn.lock").write_text("# yarn\n", encoding="utf-8")
    (root / ".yarnrc.yml").write_text("nodeLinker: node-modules\n", encoding="utf-8")
    yarn_dir = root / ".yarn"
    yarn_dir.mkdir()
    findings = _policy_findings(root, repository("tool", "typescript"))
    banned = sorted(
        finding.file or "" for finding in findings if finding.rule == "audit-banned"
    )
    assert banned == [".yarn", ".yarnrc.yml", "package.json", "yarn.lock"]
    assert any("pnpm" in finding.message for finding in findings if finding.rule == "audit-banned")


def test_http_client_ban_suggests_replacements(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    (root / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "0.1.0"\nedition = "2024"\n'
        '[dependencies]\nreqwest = "0.12"\nhyper = "1.0"\nclap = "4.0"\n',
        encoding="utf-8",
    )
    (root / "Cargo.lock").write_text(
        'version = 4\n[[package]]\nname = "demo"\nversion = "0.1.0"\n'
        '[[package]]\nname = "reqwest"\nversion = "0.12.0"\n'
        '[[package]]\nname = "hyper"\nversion = "1.0.0"\n'
        '[[package]]\nname = "clap"\nversion = "4.0.0"\n',
        encoding="utf-8",
    )
    findings = _policy_findings(root, repository("tool", "rust"))
    banned = {finding.message for finding in findings if finding.rule == "audit-banned"}
    assert any("reqwest" in message and "h12tiny" in message for message in banned)
    assert any("hyper" in message and "laputa-h2-futures-lite" in message for message in banned)
    assert any(message == "remove bloated dependency clap" for message in banned)


def test_fixer_flows_through_worktree_commit(
    tmp_path: Path, git_environment: None, monkeypatch: pytest.MonkeyPatch
) -> None:
    import json
    import time

    from repomod.fix import fix_repository as run_fix

    root = tmp_path / "fleet"
    root.mkdir()
    path = root / "demo"
    subprocess.run(["git", "init", "--initial-branch=main", str(path)], check=True)
    subprocess.run(["git", "config", "user.name", "Test User"], cwd=path, check=True)
    subprocess.run(["git", "config", "user.email", "test@example.invalid"], cwd=path, check=True)
    (path / "src").mkdir()
    (path / "src" / "demo.py").write_text("VALUE = 1\n", encoding="utf-8")
    (path / "pyproject.toml").write_text(
        '[project]\nname = "demo"\nversion = "1.0.0"\nrequires-python = ">=3.15"\n'
        'dependencies = ["requests>=2.0"]\n'
        "[dependency-groups]\ndev = [\n"
        '    "uv==0.13.5",\n    "ruff==0.17.2",\n    "ty==0.0.91",\n]\n'
        '[tool.ruff]\ntarget-version = "py315"\n'
        '[tool.ty.environment]\npython-version = "3.15"\n',
        encoding="utf-8",
    )
    (path / "uv.lock").write_text(
        'version = 1\nrequires-python = ">=3.15"\n'
        '[[package]]\nname = "requests"\nversion = "2.0.0"\n'
        '[[package]]\nname = "uv"\nversion = "0.13.5"\n'
        '[[package]]\nname = "ruff"\nversion = "0.17.2"\n'
        '[[package]]\nname = "ty"\nversion = "0.0.91"\n',
        encoding="utf-8",
    )
    subprocess.run(["git", "add", "."], cwd=path, check=True)
    subprocess.run(["git", "commit", "-m", "base"], cwd=path, check=True)
    subprocess.run(["git", "remote", "add", "origin", "https://github.com/acme/demo.git"], cwd=path, check=True)

    catalog_path = tmp_path / "catalog.toml"
    catalog_path.write_text(
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n\n'
        '[repos."acme"]\ndemo = ["tool", "python"]\n',
        encoding="utf-8",
    )
    from repomod.catalog import load_catalog

    catalog = load_catalog(catalog_path, {"HOME": str(tmp_path), "REPOMOD_ROOT": str(root)})
    repository = catalog.repositories[0]

    cache_dir = tmp_path / "cache"
    cache_dir.mkdir()
    monkeypatch.setenv("XDG_CACHE_HOME", str(tmp_path / "xdg"))
    (tmp_path / "xdg" / "repomod").mkdir(parents=True)
    (tmp_path / "xdg" / "repomod" / "audit.json").write_text(
        json.dumps(
            {
                "latest": {
                    "pypi:requests:2.0.0": {"version": "2.1.0", "fetched_at": time.time()}
                },
                "osv": {},
            }
        ),
        encoding="utf-8",
    )

    bindir = tmp_path / "bin"
    bindir.mkdir()
    fake_uv = bindir / "uv"
    fake_uv.write_text(
        "#!/usr/bin/env python3\n"
        "import pathlib\n"
        "lock = pathlib.Path('uv.lock')\n"
        "lock.write_text(lock.read_text().replace('\"2.0.0\"', '\"2.1.0\"'))\n",
        encoding="utf-8",
    )
    fake_uv.chmod(0o755)
    monkeypatch.setenv("PATH", f"{bindir}{os.pathsep}{os.environ['PATH']}")

    outcome = run_fix(catalog, repository, audit_upgrade_rules(), "main", None)
    assert outcome.failures == []
    assert [rule for rule, _ in outcome.commits] == ["audit-upgrades"]
    assert outcome.branch is not None
    branch_lock = subprocess.run(
        ["git", "show", f"{outcome.branch}:uv.lock"],
        cwd=path,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    assert '"2.1.0"' in branch_lock
    assert '"2.0.0"' in (path / "uv.lock").read_text(encoding="utf-8")

    rerun = run_fix(catalog, repository, audit_upgrade_rules(), "main", None)
    assert rerun.commits == []


def test_local_path_named_like_ban_is_not_banned(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    (root / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "0.1.0"\nedition = "2024"\n'
        '[dependencies]\nh2-support = { path = "../h2-support" }\n',
        encoding="utf-8",
    )
    findings = _policy_findings(root, repository("tool", "rust"))
    assert [finding for finding in findings if finding.rule == "audit-banned"] == []
    assert any(finding.rule == "audit-registry" for finding in findings)


def test_cargo_req_satisfies_caret_and_exact() -> None:
    assert _cargo_req_satisfies("0.22", "0.22.1")
    assert not _cargo_req_satisfies("0.22", "0.23.0")
    assert _cargo_req_satisfies("^1.2.3", "1.9.0")
    assert not _cargo_req_satisfies("^1.2.3", "2.0.0")
    assert _cargo_req_satisfies("=3.0.3", "3.0.3")
    assert not _cargo_req_satisfies("=3.0.3", "3.0.9")
    assert _cargo_req_satisfies("*", "99.0.0")
    assert _cargo_req_satisfies(">=1.0, <2.0", "1.5.0")
    assert not _cargo_req_satisfies(">=1.0, <2.0", "2.0.0")


def test_manifest_blocked_upgrade_is_manual(tmp_path: Path) -> None:
    from repomod.rules.audit_upgrades import _outdated_findings

    root = repo_root(tmp_path)
    (root / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "0.1.0"\nedition = "2024"\n'
        '[dependencies]\nfoo = "=1.0.0"\nbar = "1.0"\n',
        encoding="utf-8",
    )
    (root / "Cargo.lock").write_text(
        'version = 4\n[[package]]\nname = "demo"\nversion = "0.1.0"\n'
        '[[package]]\nname = "foo"\nversion = "1.0.0"\n'
        '[[package]]\nname = "bar"\nversion = "1.0.0"\n',
        encoding="utf-8",
    )

    def latest(ecosystem: str, name: str) -> str | None:
        return {"foo": "2.0.0", "bar": "1.1.0"}[name]

    findings = _outdated_findings(
        root, repository("tool", "rust"), {"latest": {}, "osv": {}}, 1000.0, latest
    )
    by_name = {
        finding.message.split(":")[1].split()[0]: finding
        for finding in findings
        if finding.rule == "audit-upgrades"
    }
    assert by_name["foo"].fixable is False
    assert by_name["bar"].fixable is True


def test_multi_version_lock_attributes_direct_req(tmp_path: Path) -> None:
    from repomod.audit import _collect_locked

    root = repo_root(tmp_path)
    (root / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "0.1.0"\nedition = "2024"\n'
        '[dependencies]\nsyn = "=3.0.3"\n',
        encoding="utf-8",
    )
    (root / "Cargo.lock").write_text(
        'version = 4\n[[package]]\nname = "demo"\nversion = "0.1.0"\n'
        '[[package]]\nname = "syn"\nversion = "2.0.119"\n'
        '[[package]]\nname = "syn"\nversion = "3.0.3"\n',
        encoding="utf-8",
    )
    locked, _ = _collect_locked(root, repository("tool", "rust"))
    assert [(item.name, item.locked) for item in locked if item.name == "syn"] == [("syn", "3.0.3")]


NOW = 1_800_000_000.0
HOUR = 3600.0


def iso(seconds_ago: float) -> str:
    from datetime import UTC, datetime

    return datetime.fromtimestamp(NOW - seconds_ago, UTC).isoformat().replace("+00:00", "Z")


def effect_packument() -> dict[str, object]:
    return {
        "dist-tags": {"latest": "4.1.1"},
        "time": {
            "created": iso(900 * 24 * HOUR),
            "modified": iso(HOUR),
            "4.0.6": iso(72 * HOUR),
            "4.1.0": iso(30 * HOUR),
            "4.1.1": iso(HOUR),
            "4.2.0-beta.1": iso(48 * HOUR),
        },
    }


def write_pnpm_project(root: Path, workspace: str | None = None, npmrc: str | None = None) -> None:
    (root / "package.json").write_text(
        '{"name": "demo", "dependencies": {"effect": "4.0.6"}}', encoding="utf-8"
    )
    (root / "pnpm-lock.yaml").write_text("lockfileVersion: '9.0'\n", encoding="utf-8")
    if workspace is not None:
        (root / "pnpm-workspace.yaml").write_text(workspace, encoding="utf-8")
    if npmrc is not None:
        (root / ".npmrc").write_text(npmrc, encoding="utf-8")


def fake_registry(monkeypatch: pytest.MonkeyPatch, calls: list[str] | None = None) -> None:
    def http_json(url: str, headers: dict[str, str] | None = None) -> object:
        if calls is not None:
            calls.append(url)
        assert url == "https://registry.npmjs.org/effect"
        return effect_packument()

    monkeypatch.setattr("repomod.audit._http_json", http_json)


def effect_messages(root: Path, cache: dict[str, object] | None = None) -> list[str]:
    findings = _outdated_findings(
        root, repository("tool", "typescript"), cache if cache is not None else {}, NOW, None
    )
    return [finding.message for finding in findings]


def test_npm_latest_waits_for_default_release_age(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = repo_root(tmp_path)
    write_pnpm_project(root)
    fake_registry(monkeypatch)
    assert effect_messages(root) == ["npm:effect 4.0.6 is outdated; latest is 4.1.0"]


def test_npm_release_age_comes_from_workspace_then_npmrc(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = repo_root(tmp_path)
    fake_registry(monkeypatch)
    write_pnpm_project(root, workspace="minimumReleaseAge: 90 # minutes\n")
    assert effect_messages(root) == ["npm:effect 4.0.6 is outdated; latest is 4.1.0"]
    write_pnpm_project(root, workspace="minimumReleaseAge: 30\n")
    assert effect_messages(root) == ["npm:effect 4.0.6 is outdated; latest is 4.1.1"]
    (root / "pnpm-workspace.yaml").unlink()
    write_pnpm_project(root, npmrc="minimum-release-age=4320\n")
    assert effect_messages(root) == []


def test_npm_release_age_exclusions_admit_young_versions(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = repo_root(tmp_path)
    fake_registry(monkeypatch)
    for entry, expected in (
        ("effect@4.1.1", "4.1.1"),
        ("effect@4.1.0 || 4.1.1", "4.1.1"),
        ("effect", "4.1.1"),
        ("'effect@4.0.6'", "4.1.0"),
        ("other@4.1.1", "4.1.0"),
    ):
        write_pnpm_project(root, workspace=f"minimumReleaseAgeExclude:\n  - {entry}\n")
        assert effect_messages(root) == [
            f"npm:effect 4.0.6 is outdated; latest is {expected}"
        ]


def test_npm_release_age_zero_uses_registry_latest(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    write_pnpm_project(root, workspace="minimumReleaseAge: 0\n")
    findings = _outdated_findings(
        root, repository("tool", "typescript"), {}, NOW, lambda _e, _n: "4.1.1"
    )
    assert [finding.message for finding in findings] == [
        "npm:effect 4.0.6 is outdated; latest is 4.1.1"
    ]


def test_npm_release_times_are_cached(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    root = repo_root(tmp_path)
    write_pnpm_project(root)
    calls: list[str] = []
    fake_registry(monkeypatch, calls)
    cache: dict[str, object] = {}
    first = effect_messages(root, cache)
    second = effect_messages(root, cache)
    assert first == second
    assert len(calls) == 1


def test_invalid_release_age_fails_loudly(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    write_pnpm_project(root, workspace="minimumReleaseAge: soon\n")
    with pytest.raises(ValueError, match="minimumReleaseAge"):
        effect_messages(root)


def test_policy_pinned_python_tools_are_not_outdated(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    (root / "pyproject.toml").write_text(
        '[project]\nname = "demo"\nversion = "1.0.0"\n'
        '[dependency-groups]\ndev = ["ruff==0.17.2", "ty==0.0.91", "uv==0.13.5", "requests"]\n',
        encoding="utf-8",
    )
    (root / "uv.lock").write_text(
        'version = 1\n[[package]]\nname = "ruff"\nversion = "0.17.2"\n'
        '[[package]]\nname = "ty"\nversion = "0.0.91"\n'
        '[[package]]\nname = "uv"\nversion = "0.13.5"\n'
        '[[package]]\nname = "requests"\nversion = "2.0.0"\n',
        encoding="utf-8",
    )
    findings = _outdated_findings(
        root, repository("tool", "python"), {}, NOW, lambda _e, _n: "9.9.9"
    )
    assert [finding.message for finding in findings] == [
        "pypi:requests 2.0.0 is outdated; latest is 9.9.9"
    ]


@pytest.mark.parametrize(
    ("specifier", "version", "admitted"),
    [
        ("==0.17.2", "0.17.3", False),
        ("==0.17.2", "0.17.2", True),
        ("===0.17.2", "0.17.3", False),
        ("==0.17.*", "0.17.9", True),
        ("==0.17.*", "0.18.0", False),
        ("!=0.17.3", "0.17.3", False),
        (">=1.0,<2", "2.1.0", False),
        (">=1.0,<2", "1.9.0", True),
        ("<=2.0", "2.0.0", True),
        (">1", "1.0.0", False),
        ("~=1.4.2", "1.4.9", True),
        ("~=1.4.2", "1.5.0", False),
        ("~=1.4", "1.9", True),
        ("~=1.4", "2.0", False),
    ],
)
def test_python_specifiers_follow_pep_440(specifier: str, version: str, admitted: bool) -> None:
    from repomod.rules.audit_upgrades import _specifier_admits

    assert _specifier_admits(specifier, version) is admitted


def test_python_upgrade_blocked_by_specifier_is_manual(tmp_path: Path) -> None:
    root = repo_root(tmp_path)
    (root / "pyproject.toml").write_text(
        '[project]\nname = "demo"\nversion = "1.0.0"\n'
        'dependencies = ["pinned[extra]==1.0.0 ; python_version >= \'3.15\'", "ranged>=1,<2", "open"]\n',
        encoding="utf-8",
    )
    (root / "uv.lock").write_text(
        'version = 1\n[[package]]\nname = "pinned"\nversion = "1.0.0"\n'
        '[[package]]\nname = "ranged"\nversion = "1.0.0"\n'
        '[[package]]\nname = "open"\nversion = "1.0.0"\n',
        encoding="utf-8",
    )
    latest = {"pinned": "1.1.0", "ranged": "1.5.0", "open": "3.0.0"}
    findings = _outdated_findings(
        root, repository("tool", "python"), {}, NOW, lambda _e, name: latest[name]
    )
    fixable = {finding.message.split()[0]: finding.fixable for finding in findings}
    assert fixable == {"pypi:pinned": False, "pypi:ranged": True, "pypi:open": True}
