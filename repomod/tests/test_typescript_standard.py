from __future__ import annotations

import json
from io import BytesIO
from pathlib import Path
from typing import Any

import pytest

from repomod import cli
from repomod.rules import actions, typescript_standard
from repomod.rules.typescript_standard import (
    _fix_typescript_stack,
    _package_script_edits,
    _range_has_minimum,
    _range_has_version_floor,
    _typescript_stack_findings,
    _validate_typescript_stack,
    load_typescript_tool_pins,
)
from repomod.types import RepoId, Repository


def repository(*tags: str) -> Repository:
    return Repository(RepoId("acme", "demo"), tuple(tags))


def package_data(*, dependencies: dict[str, str] | None = None) -> dict[str, Any]:
    pins = load_typescript_tool_pins()
    versions = {
        "typescript": "^7.0.0",
        "oxlint": "^1.0.0",
        "oxlint-tsgolint": "^7.0.0",
        "vitest": "^5.0.0",
    }
    versions.update(dependencies or {})
    return {
        "name": "demo",
        "private": True,
        "type": "module",
        "packageManager": f"pnpm@{pins.pnpm}",
        "engines": {"node": f">={pins.node}"},
        "scripts": {
            "lint": "oxlint src",
            "typecheck": "tsc --noEmit",
            "test": "vitest run",
            "check": "pnpm run lint && pnpm run typecheck && pnpm run test",
        },
        "devDependencies": versions,
    }


def make_project(root: Path, package: dict[str, object] | None = None) -> None:
    (root / "package.json").write_text(
        json.dumps(package or package_data(), indent=2) + "\n",
        encoding="utf-8",
    )
    (root / "pnpm-lock.yaml").write_text("lockfileVersion: '9.0'\n", encoding="utf-8")
    (root / "tsconfig.json").write_text(
        '{"compilerOptions":{"strict":true}}\n',
        encoding="utf-8",
    )


def write_tool_pins(path: Path, node: str, pnpm: str) -> None:
    path.write_text(
        f'[node]\nversion = "{node}"\n\n[pnpm]\nversion = "{pnpm}"\n',
        encoding="utf-8",
    )


def test_typescript_tool_pins_reject_symlinked_source(tmp_path: Path) -> None:
    source = tmp_path / "source.toml"
    source.write_text('[node]\nversion = "26.12.1"\n\n[pnpm]\nversion = "12.15.0"\n')
    path = tmp_path / "typescript-tools.toml"
    path.symlink_to(source)

    with pytest.raises(ValueError, match="approved TypeScript tool pins are a symlink"):
        load_typescript_tool_pins(path)


def test_typescript_rule_reports_symlinked_tool_pins_as_operational(
    tmp_path: Path,
    monkeypatch,
) -> None:
    source = tmp_path / "source.toml"
    write_tool_pins(source, "26.12.1", "12.15.0")
    package = package_data()
    path = tmp_path / "typescript-tools.toml"
    path.symlink_to(source)
    monkeypatch.setattr(typescript_standard, "typescript_tools_file", lambda: path)
    make_project(tmp_path, package)

    findings = _typescript_stack_findings(tmp_path, repository("library", "typescript"))

    assert any(
        finding.file == "typescript-tools.toml"
        and finding.kind == "operational"
        and "symlink" in finding.message
        for finding in findings
    )


def test_type_range_requires_every_alternative_to_start_at_seven() -> None:
    assert _range_has_minimum("^7.0.0", 7)
    assert _range_has_minimum(">=7.0.0 <8", 7)
    assert _range_has_minimum("npm:typescript@7.0.2", 7)
    assert not _range_has_minimum("^6.9.0", 7)
    assert not _range_has_minimum(">=6 <8", 7)
    assert not _range_has_minimum("^6.0.0 || ^7.0.0", 7)
    assert not _range_has_minimum("latest", 7)


def test_node_engine_range_must_include_the_approved_current_release() -> None:
    assert _range_has_version_floor(">=26.12.1", (26, 12, 1))
    assert _range_has_version_floor(">=26.12.1 <27", (26, 12, 1))
    assert _range_has_version_floor(">=27 || >=26.12.1", (26, 12, 1))
    assert not _range_has_version_floor(">=26.12.0", (26, 12, 1))
    assert not _range_has_version_floor(">=26.12.1 || >=26.0.0", (26, 12, 1))


def test_typescript_rule_checks_the_universal_pnpm_and_typescript_contract(tmp_path: Path) -> None:
    package = package_data()
    dependencies = package["devDependencies"]
    assert isinstance(dependencies, dict)
    dependencies["typescript"] = "^6.9.0"
    package["packageManager"] = "npm@11.0.2"
    make_project(tmp_path, package)
    (tmp_path / "package-lock.json").write_text("{}\n", encoding="utf-8")

    findings = _typescript_stack_findings(tmp_path, repository("library", "typescript"))
    messages = [finding.message for finding in findings]

    assert "declare a pinned pnpm release in packageManager" in messages
    assert any("lower bound of 7.0.0" in message for message in messages)
    assert any("package-lock.json" in message for message in messages)
    assert all(finding.rule == "typescript-stack" for finding in findings)


def test_typescript_rule_rejects_nested_lock_authorities(tmp_path: Path) -> None:
    make_project(tmp_path)
    nested_lock = tmp_path / "packages" / "legacy" / "yarn.lock"
    nested_lock.parent.mkdir(parents=True)
    nested_lock.write_text("left-pad@1:\n", encoding="utf-8")

    findings = _typescript_stack_findings(tmp_path, repository("library", "typescript"))

    assert any(finding.file == "packages/legacy/yarn.lock" for finding in findings)


def test_typescript_rule_flags_offline_node_and_pnpm_pin_drift(tmp_path: Path) -> None:
    package = package_data()
    package["packageManager"] = "pnpm@11.0.2"
    package["engines"] = {"node": ">=24.0.0"}
    make_project(tmp_path, package)
    (tmp_path / ".node-version").write_text("24.0.0\n", encoding="utf-8")
    (tmp_path / ".tool-versions").write_text("nodejs 24.0.0\npnpm 11.0.2\n", encoding="utf-8")

    findings = _typescript_stack_findings(tmp_path, repository("library", "typescript"))
    messages = [finding.message for finding in findings]

    assert any("packageManager pin pnpm@11.0.2 is stale" in message for message in messages)
    assert any("engines.node must require Node.js >=" in message for message in messages)
    assert any("Node.js pin 24.0.0 is stale" in message for message in messages)
    assert any("pnpm pin 11.0.2 is stale" in message for message in messages)


def test_independent_package_roots_each_declare_typescript(tmp_path: Path) -> None:
    for name, dependencies in (
        ("api", {"typescript": "^7.0.0"}),
        ("scripts", {}),
    ):
        package_root = tmp_path / name
        package_root.mkdir()
        package = package_data(dependencies=dependencies)
        if name == "scripts":
            package_dependencies = package["devDependencies"]
            assert isinstance(package_dependencies, dict)
            package_dependencies.pop("typescript")
        (package_root / "package.json").write_text(json.dumps(package), encoding="utf-8")
        (package_root / "pnpm-lock.yaml").write_text("lockfileVersion: '9.0'\n", encoding="utf-8")
        (package_root / "tsconfig.json").write_text(
            '{"compilerOptions":{"strict":true}}\n',
            encoding="utf-8",
        )

    findings = _typescript_stack_findings(tmp_path, repository("library", "typescript"))

    assert any(
        finding.file == "scripts/package.json" and "declare TypeScript >=7" in finding.message
        for finding in findings
    )


def test_library_stack_does_not_require_app_frameworks(tmp_path: Path) -> None:
    package = package_data()
    pins = load_typescript_tool_pins()
    package["packageManager"] = f"pnpm@{pins.pnpm}+sha512.ABC/=="
    make_project(tmp_path, package)

    findings = _typescript_stack_findings(tmp_path, repository("library", "typescript"))

    assert findings == []


def test_server_app_does_not_require_frontend_frameworks(tmp_path: Path) -> None:
    package = package_data(dependencies={"effect": "^4.0.0"})
    make_project(tmp_path, package)

    findings = _typescript_stack_findings(tmp_path, repository("app", "typescript"))

    assert not any("vite" in finding.message or "solid" in finding.message for finding in findings)


def test_frontend_app_requires_current_app_stack(tmp_path: Path) -> None:
    package = package_data(
        dependencies={
            "effect": "^4.0.0",
            "vite": "^8.0.0",
            "solid-js": "^2.0.0-rc.1",
            "@solidjs/web": "^2.0.0-rc.1",
            "@solidjs/vite-plugin": "^3.0.0-next.1",
        }
    )
    make_project(tmp_path, package)
    (tmp_path / "index.html").write_text("<main></main>\n", encoding="utf-8")

    assert _typescript_stack_findings(tmp_path, repository("app", "typescript")) == []


def test_frontend_app_reports_missing_solid_and_effect_without_affecting_library(
    tmp_path: Path,
) -> None:
    make_project(tmp_path)
    (tmp_path / "index.html").write_text("<main></main>\n", encoding="utf-8")

    app_messages = [
        finding.message
        for finding in _typescript_stack_findings(tmp_path, repository("app", "typescript"))
    ]
    library_messages = [
        finding.message
        for finding in _typescript_stack_findings(tmp_path, repository("library", "typescript"))
    ]

    assert any("effect" in message for message in app_messages)
    assert any("solid-js" in message for message in app_messages)
    assert any("@solidjs/web" in message for message in app_messages)
    assert not any("effect" in message or "solid-js" in message or "vite" in message for message in library_messages)


def test_strict_mode_can_be_inherited_from_local_jsonc_config(tmp_path: Path) -> None:
    package = package_data()
    (tmp_path / "package.json").write_text(json.dumps(package), encoding="utf-8")
    (tmp_path / "pnpm-lock.yaml").write_text("lockfileVersion: '9.0'\n", encoding="utf-8")
    config = tmp_path / "config"
    config.mkdir()
    (config / "strict-base.json").write_text(
        '{\n  // Base settings shared with child packages.\n'
        '  "compilerOptions": { "strict": true, },\n}\n',
        encoding="utf-8",
    )
    (tmp_path / "tsconfig.json").write_text(
        '{"extends":"./config/strict-base"}\n',
        encoding="utf-8",
    )

    assert _typescript_stack_findings(tmp_path, repository("library", "typescript")) == []


def test_tsconfig_symlink_is_reported_without_following_it(tmp_path: Path) -> None:
    outside = tmp_path.parent / "outside-tsconfig.json"
    outside.write_text('{"compilerOptions":{"strict":true}}\n', encoding="utf-8")
    make_project(tmp_path)
    (tmp_path / "tsconfig.json").unlink()
    (tmp_path / "tsconfig.json").symlink_to(outside)

    findings = _typescript_stack_findings(tmp_path, repository("library", "typescript"))

    assert any(finding.message == "TypeScript configuration is a symlink and is not followed" for finding in findings)


def test_script_fixer_changes_only_npm_run_values_and_validates_the_plan(tmp_path: Path) -> None:
    source = (
        '{\n  "name": "demo",\n  "description": "npm run is mentioned here",\n'
        '  "scripts": {"check": "npm run lint && npm run typecheck", '
        '"message": "echo \'npm run lint\'"}\n}\n'
    )
    expected = source.replace(
        '"npm run lint && npm run typecheck"',
        '"pnpm run lint && pnpm run typecheck"',
    )
    assert _package_script_edits(source) == expected
    (tmp_path / "package.json").write_text(source, encoding="utf-8")

    plan = _fix_typescript_stack(tmp_path, repository("app", "typescript"))
    (tmp_path / "package.json").write_text(plan["package.json"], encoding="utf-8")

    assert plan == {"package.json": expected}
    assert _validate_typescript_stack(tmp_path, repository("app", "typescript"), ("package.json",)) == []


def test_tool_pin_refresh_uses_official_current_stable_release_sources(
    tmp_path: Path,
    monkeypatch,
) -> None:
    pins_path = tmp_path / "typescript-tools.toml"
    write_tool_pins(pins_path, "26.12.1", "12.14.0")
    node_index = json.dumps(
        [
            {"version": "v27.0.0-rc.1"},
            {"version": "v26.12.1"},
            {"version": "v27.0.0"},
        ]
    ).encode()
    monkeypatch.setattr(
        typescript_standard.urllib.request,
        "urlopen",
        lambda _request, timeout: BytesIO(node_index),
    )
    monkeypatch.setattr(
        typescript_standard,
        "action_tags_many",
        lambda repositories, _prefix: {"pnpm/pnpm": ("v12.15.0", "v12.16.0-rc.0", "v11.28.8")},
    )

    assert typescript_standard.refresh_typescript_tool_pins(pins_path)
    assert pins_path.read_text(encoding="utf-8") == (
        '[node]\nversion = "27.0.0"\n\n[pnpm]\nversion = "12.15.0"\n'
    )
    assert not typescript_standard.refresh_typescript_tool_pins(pins_path)


def test_pins_refresh_updates_action_and_typescript_tool_sources(
    monkeypatch,
    capsys,
) -> None:
    refreshed = []
    monkeypatch.setattr(actions, "refresh_pins", lambda: refreshed.append("actions") or True)
    monkeypatch.setattr(
        "repomod.rules.python_standard.refresh_python_tool_versions",
        lambda: False,
    )
    monkeypatch.setattr(
        typescript_standard,
        "refresh_typescript_tool_pins",
        lambda: refreshed.append("typescript-tools") or True,
    )

    assert cli.main(["pins", "refresh"]) == 0

    assert refreshed == ["actions", "typescript-tools"]
    assert capsys.readouterr().out == "approved pins updated: GitHub Actions, TypeScript tools\n"
