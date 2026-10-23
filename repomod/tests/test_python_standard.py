import json
import sys
from pathlib import Path

import pytest

from repomod import cli
from repomod.rules.python_standard import (
    _latest_version,
    _python_standard_findings,
    _refreshed_lock,
    load_approved_python_tool_versions,
    python_standard_rules,
    refresh_python_standard,
    refresh_python_tool_versions,
)
from repomod.types import RepoId, Repository


def _repository() -> Repository:
    return Repository(RepoId("acme", "demo"), ("tool", "python"))


def _write_approvals(
    root: Path,
    monkeypatch: pytest.MonkeyPatch,
    *,
    uv: str = "0.9.8",
    ruff: str = "0.15.15",
    ty: str = "0.0.89",
) -> Path:
    path = root / "python-tools.toml"
    path.write_text(
        "[tools]\n"
        f'uv = "{uv}"\n'
        f'ruff = "{ruff}"\n'
        f'ty = "{ty}"\n',
        encoding="utf-8",
    )
    monkeypatch.setattr("repomod.rules.python_standard.python_tools_file", lambda: path)
    return path


def _write_project(
    root: Path,
    *,
    python: str = ">=3.15",
    uv: str = "0.9.8",
    ruff: str = "0.15.15",
    ty: str = "0.0.89",
) -> None:
    (root / "pyproject.toml").write_text(
        f'[project]\nname = "demo"\nversion = "1.0.0"\nrequires-python = "{python}"\n\n'
        '[dependency-groups]\n'
        'dev = [\n'
        '    "pytest>=9",\n'
        f'    "uv=={uv}",\n'
        f'    "ruff=={ruff}",\n'
        f'    "ty=={ty}",\n'
        ']\n\n'
        '[tool.ruff]\n'
        'target-version = "py315"\n\n'
        '[tool.ty.environment]\n'
        'python-version = "3.15"\n',
        encoding="utf-8",
    )
    _write_lock(root, uv=uv, ruff=ruff, ty=ty, python=python)


def _write_lock(
    root: Path,
    *,
    uv: str = "0.9.8",
    ruff: str = "0.15.15",
    ty: str = "0.0.89",
    python: str = ">=3.15",
) -> None:
    (root / "uv.lock").write_text(
        f'version = 1\nrequires-python = "{python}"\n\n'
        f'[[package]]\nname = "uv"\nversion = "{uv}"\n\n'
        f'[[package]]\nname = "ruff"\nversion = "{ruff}"\n\n'
        f'[[package]]\nname = "ty"\nversion = "{ty}"\n',
        encoding="utf-8",
    )


def _write_workflow(root: Path, source: str) -> Path:
    path = root / ".github" / "workflows" / "ci.yml"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(source, encoding="utf-8")
    return path


def test_python_standard_accepts_python_315_and_locked_tool_versions(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _write_approvals(tmp_path, monkeypatch)
    _write_project(tmp_path)

    assert _python_standard_findings(tmp_path, _repository()) == []


def test_python_standard_accepts_python_315_with_an_upper_bound(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _write_approvals(tmp_path, monkeypatch)
    _write_project(tmp_path, python=">=3.15,<3.16")
    _write_lock(tmp_path, python="==3.15.*")

    assert _python_standard_findings(tmp_path, _repository()) == []


def test_python_standard_uses_central_approved_versions_offline(tmp_path: Path) -> None:
    versions = load_approved_python_tool_versions()
    _write_project(tmp_path, **versions)

    assert _python_standard_findings(tmp_path, _repository()) == []


def test_python_standard_reports_python_tool_and_lock_drift(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _write_approvals(tmp_path, monkeypatch)
    _write_project(tmp_path, python=">=3.14")
    project = tmp_path / "pyproject.toml"
    source = project.read_text(encoding="utf-8").replace('"py315"', '"py314"').replace(
        '"3.15"', '"3.14"'
    )
    source = source.replace('"ruff==0.15.15"', '"ruff>=0.15"')
    project.write_text(source, encoding="utf-8")
    _write_lock(tmp_path, ruff="0.15.14", python=">=3.14")

    findings = _python_standard_findings(tmp_path, _repository())

    assert [(finding.file, finding.message, finding.fixable) for finding in findings] == [
        ("pyproject.toml", "project.requires-python must be >=3.15", True),
        ("pyproject.toml", "tool.ruff.target-version must be py315", True),
        ("pyproject.toml", "tool.ty.environment.python-version must be 3.15", True),
        ("pyproject.toml", "dev dependency ruff must be pinned as ruff==0.15.15", True),
        ("uv.lock", "uv.lock ruff version must match approved ruff==0.15.15", True),
    ]


def test_python_standard_reports_lock_metadata_and_tool_version_drift(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _write_approvals(tmp_path, monkeypatch)
    _write_project(tmp_path)
    _write_lock(tmp_path, ruff="0.15.14", python=">=3.14")

    findings = _python_standard_findings(tmp_path, _repository())

    assert [(finding.file, finding.message) for finding in findings] == [
        ("uv.lock", "uv.lock requires-python must match project.requires-python"),
        ("uv.lock", "uv.lock ruff version must match approved ruff==0.15.15"),
    ]


def test_python_standard_compares_exact_dependencies_with_approved_versions(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _write_approvals(tmp_path, monkeypatch, uv="0.9.9")
    _write_project(tmp_path)

    findings = _python_standard_findings(tmp_path, _repository())

    assert [(finding.file, finding.message) for finding in findings] == [
        ("pyproject.toml", "dev dependency uv must match approved uv==0.9.9"),
        ("uv.lock", "uv.lock uv version must match approved uv==0.9.9"),
    ]


def test_python_standard_reports_missing_approvals_as_operational(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _write_project(tmp_path)
    monkeypatch.setattr(
        "repomod.rules.python_standard.python_tools_file",
        lambda: tmp_path / "missing-python-tools.toml",
    )

    findings = _python_standard_findings(tmp_path, _repository())

    assert findings[0].kind == "operational"
    assert findings[0].file == "python-tools.toml"
    assert "cannot read approved Python tool versions" in findings[0].message


def test_python_standard_reports_unreadable_manifests_as_operational(tmp_path: Path) -> None:
    (tmp_path / "pyproject.toml").write_text("[project\n", encoding="utf-8")

    findings = _python_standard_findings(tmp_path, _repository())

    assert len(findings) == 1
    assert findings[0].kind == "operational"
    assert findings[0].file == "pyproject.toml"
    assert "invalid TOML" in findings[0].message


def test_python_standard_fixes_explicit_matrix_and_setup_python_versions(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _write_approvals(tmp_path, monkeypatch)
    _write_project(tmp_path)
    _write_workflow(
        tmp_path,
        "jobs:\n"
        "  test:\n"
        "    strategy:\n"
        "      matrix:\n"
        "        include:\n"
        "          - os: ubuntu-latest\n"
        '            python-version: "3.13"\n'
        "          - os: ubuntu-latest\n"
        '            python-version: "3.14"\n'
        "          - os: macos-latest\n"
        '            python-version: "3.13"\n'
        "          - os: macos-latest\n"
        '            python-version: "3.14"\n'
        "    steps:\n"
        "      - uses: actions/setup-python@v5\n"
        "        with:\n"
        "          python-version: ${{ matrix.python-version }}\n"
        "  pytest-nightly:\n"
        "    steps:\n"
        "      - uses: actions/setup-python@v5\n"
        "        with:\n"
        '          python-version: "3.13" # prerelease test\n',
    )
    monkeypatch.setattr(
        "repomod.rules.python_standard._refreshed_lock",
        lambda root, pyproject, python_version=None: (root / "uv.lock").read_text(encoding="utf-8"),
    )

    findings = _python_standard_findings(tmp_path, _repository())
    plan = refresh_python_standard(tmp_path, _repository())

    ci_findings = [finding for finding in findings if finding.file == ".github/workflows/ci.yml"]
    assert len(ci_findings) == 5
    assert all(finding.fixable for finding in ci_findings)
    assert ".github/workflows/*.yml" in python_standard_rules()[0].allowed_paths
    assert ".github/workflows/*.yaml" in python_standard_rules()[0].allowed_paths
    updated = plan[".github/workflows/ci.yml"]
    assert updated.count("- os:") == 2
    assert updated.count('python-version: "3.15"') == 3
    assert "python-version: ${{ matrix.python-version }}" in updated
    for relative, contents in plan.items():
        (tmp_path / relative).write_text(contents, encoding="utf-8")
    assert _python_standard_findings(tmp_path, _repository()) == []


def test_python_standard_marks_block_style_ci_version_lists_for_manual_update(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _write_approvals(tmp_path, monkeypatch)
    _write_project(tmp_path)
    _write_workflow(
        tmp_path,
        "jobs:\n"
        "  test:\n"
        "    strategy:\n"
        "      matrix:\n"
        "        python-version:\n"
        '          - "3.13"\n'
        '          - "3.14"\n',
    )

    findings = _python_standard_findings(tmp_path, _repository())

    ci_findings = [finding for finding in findings if finding.file == ".github/workflows/ci.yml"]
    assert len(ci_findings) == 1
    assert not ci_findings[0].fixable
    assert ci_findings[0].message == (
        "line 5: Python CI versions below 3.15: 3.13, 3.14; update this workflow manually"
    )


def test_python_standard_updates_python_version_file_before_refreshing_lock(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _write_approvals(tmp_path, monkeypatch)
    _write_project(tmp_path)
    python_version = tmp_path / ".python-version"
    python_version.write_text("3.14.7\n", encoding="utf-8")
    lock = (tmp_path / "uv.lock").read_text(encoding="utf-8")
    refreshed_versions = []

    def refreshed_lock(root: Path, pyproject: str, version: str | None = None) -> str:
        refreshed_versions.append(version)
        return lock

    monkeypatch.setattr("repomod.rules.python_standard._refreshed_lock", refreshed_lock)

    findings = _python_standard_findings(tmp_path, _repository())
    plan = refresh_python_standard(tmp_path, _repository())

    assert [(finding.file, finding.message, finding.fixable) for finding in findings] == [
        (".python-version", ".python-version must be at least 3.15 (found 3.14.7)", True)
    ]
    assert refreshed_versions == ["3.15\n"]
    assert plan[".python-version"] == "3.15\n"
    assert ".python-version" in python_standard_rules()[0].allowed_paths
    python_version.write_text(plan[".python-version"], encoding="utf-8")
    assert _python_standard_findings(tmp_path, _repository()) == []


def test_python_standard_does_not_follow_python_version_symlink(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _write_approvals(tmp_path, monkeypatch)
    _write_project(tmp_path)
    target = tmp_path / "outside-python-version"
    target.write_text("3.14.7\n", encoding="utf-8")
    (tmp_path / ".python-version").symlink_to(target)

    findings = _python_standard_findings(tmp_path, _repository())

    assert len(findings) == 1
    assert findings[0].kind == "operational"
    assert findings[0].message == ".python-version is a symlink and is not followed"


def test_python_standard_fixes_inline_dev_group_arrays(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _write_approvals(tmp_path, monkeypatch, uv="0.9.9", ruff="0.15.16", ty="0.0.90")
    _write_project(
        tmp_path,
        python=">=3.14,<3.16",
        uv="0.9.0",
        ruff="0.15.0",
        ty="0.0.80",
    )
    project = tmp_path / "pyproject.toml"
    source = project.read_text(encoding="utf-8")
    source = source.replace(
        'dev = [\n    "pytest>=9",\n    "uv==0.9.0",\n    "ruff==0.15.0",\n    "ty==0.0.80",\n]',
        'dev = ["pytest[time]>=9", "uv==0.9.0", "ruff==0.15.0", "ty==0.0.80"] # keep [group]',
    )
    project.write_text(source, encoding="utf-8")
    _write_lock(
        tmp_path,
        uv="0.9.9",
        ruff="0.15.16",
        ty="0.0.90",
        python=">=3.15,<3.16",
    )
    lock = (tmp_path / "uv.lock").read_text(encoding="utf-8")
    monkeypatch.setattr(
        "repomod.rules.python_standard._refreshed_lock",
        lambda root, pyproject, python_version=None: lock,
    )

    plan = refresh_python_standard(tmp_path, _repository())

    assert (
        'dev = ["pytest[time]>=9", "uv==0.9.9", "ruff==0.15.16", "ty==0.0.90"] # keep [group]'
        in plan["pyproject.toml"]
    )


def test_python_standard_reports_a_missing_lock_as_fixable(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _write_approvals(tmp_path, monkeypatch)
    _write_project(tmp_path)
    (tmp_path / "uv.lock").unlink()

    findings = _python_standard_findings(tmp_path, _repository())

    assert [(finding.file, finding.message, finding.fixable) for finding in findings] == [
        ("uv.lock", "uv.lock is required for Python standards", True)
    ]


def test_python_standard_does_not_follow_a_pyproject_symlink(tmp_path: Path) -> None:
    target = tmp_path / "outside.toml"
    target.write_text('[project]\nrequires-python = ">=3.15"\n', encoding="utf-8")
    (tmp_path / "pyproject.toml").symlink_to(target)

    findings = _python_standard_findings(tmp_path, _repository())

    assert len(findings) == 1
    assert findings[0].kind == "operational"
    assert findings[0].message == "pyproject.toml is a symlink and is not followed"


def test_python_standard_fixer_uses_approved_versions_and_returns_a_coherent_plan(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _write_approvals(tmp_path, monkeypatch, uv="0.9.9", ruff="0.15.16", ty="0.0.90")
    _write_project(
        tmp_path,
        python=">=3.14,<3.16",
        uv="0.9.0",
        ruff="0.15.0",
        ty="0.0.80",
    )

    def refreshed_lock(root: Path, pyproject: str, python_version: str | None = None) -> str:
        assert root == tmp_path
        assert '"uv==0.9.9"' in pyproject
        assert '"ruff==0.15.16"' in pyproject
        assert '"ty==0.0.90"' in pyproject
        return (
            'version = 1\nrequires-python = "==3.15.*"\n\n'
            '[[package]]\nname = "uv"\nversion = "0.9.9"\n\n'
            '[[package]]\nname = "ruff"\nversion = "0.15.16"\n\n'
            '[[package]]\nname = "ty"\nversion = "0.0.90"\n'
        )

    monkeypatch.setattr("repomod.rules.python_standard._refreshed_lock", refreshed_lock)

    plan = refresh_python_standard(tmp_path, _repository())

    assert set(plan) == {"pyproject.toml", "uv.lock"}
    assert 'requires-python = ">=3.15,<3.16"' in plan["pyproject.toml"]
    assert 'target-version = "py315"' in plan["pyproject.toml"]
    assert 'python-version = "3.15"' in plan["pyproject.toml"]
    for relative, contents in plan.items():
        (tmp_path / relative).write_text(contents, encoding="utf-8")
    assert _python_standard_findings(tmp_path, _repository()) == []


def test_python_standard_rule_has_a_stable_blacklistable_id() -> None:
    rule, = python_standard_rules()

    assert rule.id == "python-standard"
    assert rule.applies_to.matches(("python",))
    assert rule.fix is not None


def test_pins_refresh_updates_action_and_python_tool_approvals(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    from repomod.rules import actions

    monkeypatch.setattr(actions, "refresh_pins", lambda: True)
    monkeypatch.setattr(
        "repomod.rules.python_standard.refresh_python_tool_versions",
        lambda: True,
    )
    monkeypatch.setattr(
        "repomod.rules.typescript_standard.refresh_typescript_tool_pins",
        lambda: True,
    )

    assert cli.main(["pins", "refresh"]) == 0
    assert capsys.readouterr().out == (
        "approved pins updated: GitHub Actions, Python tools, TypeScript tools\n"
    )


def test_pins_refresh_attempts_python_approvals_when_github_refresh_fails(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    from repomod.rules import actions

    python_refresh_called = False

    def fail_github() -> bool:
        raise ValueError("offline")

    def refresh_python() -> bool:
        nonlocal python_refresh_called
        python_refresh_called = True
        return False

    monkeypatch.setattr(actions, "refresh_pins", fail_github)
    monkeypatch.setattr("repomod.rules.python_standard.refresh_python_tool_versions", refresh_python)
    monkeypatch.setattr(
        "repomod.rules.typescript_standard.refresh_typescript_tool_pins",
        lambda: False,
    )

    assert cli.main(["pins", "refresh"]) == 2
    assert python_refresh_called
    assert "pin refresh failed: GitHub Actions: offline" in capsys.readouterr().err


def test_latest_version_refresh_reports_unavailable_metadata(monkeypatch: pytest.MonkeyPatch) -> None:
    def fail(url: str, *, timeout: float):
        raise OSError("offline")

    monkeypatch.setattr("repomod.rules.python_standard.urllib.request.urlopen", fail)

    with pytest.raises(ValueError, match="cannot refresh uv from PyPI: offline"):
        _latest_version("uv")


def test_latest_version_refresh_reads_the_named_stable_release(monkeypatch: pytest.MonkeyPatch) -> None:
    class Response:
        def __enter__(self) -> Response:
            return self

        def __exit__(self, *_: object) -> None:
            return None

        def read(self) -> bytes:
            return json.dumps({"info": {"name": "uv", "version": "0.9.9"}}).encode()

    def respond(url: str, *, timeout: float) -> Response:
        assert url == "https://pypi.org/pypi/uv/json"
        assert timeout == 20
        return Response()

    monkeypatch.setattr("repomod.rules.python_standard.urllib.request.urlopen", respond)

    assert _latest_version("uv") == "0.9.9"


def test_python_tool_refresh_updates_the_approved_file_deterministically(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    path = _write_approvals(tmp_path, monkeypatch)
    latest = {"uv": "0.9.9", "ruff": "0.15.16", "ty": "0.0.90"}
    monkeypatch.setattr("repomod.rules.python_standard._latest_version", lambda name: latest[name])

    assert refresh_python_tool_versions(path)
    assert path.read_text(encoding="utf-8") == (
        '[tools]\nuv = "0.9.9"\nruff = "0.15.16"\nty = "0.0.90"\n'
    )
    assert not refresh_python_tool_versions(path)


def test_python_tool_refresh_does_not_write_when_metadata_fails(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    path = _write_approvals(tmp_path, monkeypatch)
    original = path.read_text(encoding="utf-8")

    def fail(name: str) -> str:
        if name == "ruff":
            raise ValueError("offline")
        return "0.9.9"

    monkeypatch.setattr("repomod.rules.python_standard._latest_version", fail)

    with pytest.raises(ValueError, match="offline"):
        refresh_python_tool_versions(path)

    assert path.read_text(encoding="utf-8") == original


def test_python_tool_refresh_refuses_to_downgrade_approvals(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    path = _write_approvals(tmp_path, monkeypatch)
    original = path.read_text(encoding="utf-8")
    latest = {"uv": "0.9.7", "ruff": "0.15.16", "ty": "0.0.90"}
    monkeypatch.setattr("repomod.rules.python_standard._latest_version", lambda name: latest[name])

    with pytest.raises(ValueError, match=r"refusing to replace 0\.9\.8 with 0\.9\.7"):
        refresh_python_tool_versions(path)

    assert path.read_text(encoding="utf-8") == original


def test_refreshed_lock_runs_uv_in_a_disposable_copy_and_cleans_it_up(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    root = tmp_path / "project"
    root.mkdir()
    temporary_root = root / ".work" / "tmp"
    (root / "pyproject.toml").write_text('[project]\nname = "demo"\n', encoding="utf-8")
    (root / "uv.lock").write_text("old lock\n", encoding="utf-8")
    (root / ".python-version").write_text("3.14.7\n", encoding="utf-8")
    executable = tmp_path / "uv"
    executable.write_text(
        f"#!{sys.executable}\n"
        "from pathlib import Path\n"
        "Path('uv.lock').write_text(Path('.python-version').read_text())\n",
        encoding="utf-8",
    )
    executable.chmod(0o755)
    monkeypatch.setattr("repomod.rules.python_standard.shutil.which", lambda _: str(executable))

    refreshed = _refreshed_lock(
        root,
        '[project]\nname = "demo"\nrequires-python = ">=3.15"\n',
        "3.15\n",
    )

    assert refreshed == "3.15\n"
    assert (root / ".python-version").read_text(encoding="utf-8") == "3.14.7\n"
    assert temporary_root.is_dir()
    assert list(temporary_root.iterdir()) == []


def test_refreshed_lock_rejects_symlinked_scratch_root(tmp_path: Path) -> None:
    root = tmp_path / "project"
    root.mkdir()
    outside = tmp_path / "outside"
    outside.mkdir()
    (root / ".work").symlink_to(outside, target_is_directory=True)

    with pytest.raises(RuntimeError, match="temporary path is a symlink"):
        _refreshed_lock(root, "")


def test_refreshed_lock_reports_uv_failure_and_cleans_its_copy(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    root = tmp_path / "project"
    root.mkdir()
    temporary_root = root / ".work" / "tmp"
    temporary_root.mkdir(parents=True)
    (root / "pyproject.toml").write_text('[project]\nname = "demo"\n', encoding="utf-8")
    executable = tmp_path / "uv"
    executable.write_text(f"#!{sys.executable}\nraise SystemExit('lock failed')\n", encoding="utf-8")
    executable.chmod(0o755)
    monkeypatch.setattr("repomod.rules.python_standard.shutil.which", lambda _: str(executable))

    with pytest.raises(RuntimeError, match="uv lock failed while refreshing Python standards: lock failed"):
        _refreshed_lock(root, '[project]\nname = "demo"\n')

    assert list(temporary_root.iterdir()) == []
