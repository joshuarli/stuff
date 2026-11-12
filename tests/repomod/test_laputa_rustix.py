from __future__ import annotations

from pathlib import Path
from unittest import mock

import pytest

from repomod.rules import laputa_rustix
from repomod.rules.laputa_rustix import _check, _fix
from repomod.types import RepoId, Repository


def repository(*tags: str) -> Repository:
    return Repository(RepoId("acme", "demo"), tuple(tags))


def write(root: Path, body: str, name: str = "Cargo.toml") -> None:
    (root / name).write_text(body, encoding="utf-8")


def write_member(root: Path, body: str) -> Path:
    member = root / "crates" / "demo"
    member.mkdir(parents=True)
    write(member, body)
    return member / "Cargo.toml"


def member_layout(root: Path, body: str) -> None:
    write(root, '[workspace]\nresolver = "2"\nmembers = ["crates/demo"]\n')
    write_member(root, body)


def test_flags_bare_string_dependency(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    write(root, '[package]\nname = "demo"\nversion = "0.1.0"\n\n[dependencies]\nrustix = "1"\n')
    findings = _check(root, repository("tool", "rust"))
    assert [(f.rule, f.line, f.column) for f in findings] == [("laputa-rustix", 6, 1)]
    assert findings[0].fixable


def test_flags_inline_table_dependency(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    write(
        root,
        '[package]\nname = "demo"\nversion = "0.1.0"\n\n[dependencies]\n'
        'rustix = { version = "1", features = ["fs"] }\n',
    )
    findings = _check(root, repository("tool", "rust"))
    assert [(f.line, f.column) for f in findings] == [(6, 1)]


def test_flags_workspace_and_target_dependencies(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    write(
        root,
        '[workspace]\nmembers = ["a", "b"]\n\n[workspace.dependencies]\nrustix = "1"\n'
        '[target.\'cfg(unix)\'.dependencies]\nrustix = { version = "1" }\n',
    )
    findings = _check(root, repository("tool", "rust"))
    assert sorted(f.line or 0 for f in findings) == [5, 7]


def test_skips_git_dependencies_and_local_path_overrides(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    write(
        root,
        '[package]\nname = "demo"\nversion = "0.1.0"\n\n[dependencies]\n'
        'rustix = { git = "https://example.com/rustix" }\n'
        'path-rustix = { path = "../rustix" }\n'
        'via-path = { path = "vendor/rustix", version = "1" }\n',
    )
    assert _check(root, repository("tool", "rust")) == []


def test_skips_already_migrated_and_other_package_renames(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    write(
        root,
        '[package]\nname = "demo"\nversion = "0.1.0"\n\n[dependencies]\n'
        'rustix = { version = "1", package = "laputa-rustix" }\n'
        'upstream = { version = "1", package = "rustix" }\n',
    )
    assert _check(root, repository("tool", "rust")) == []


def test_fixtures_and_vendor_trees_are_not_scanned(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    fixtures = root / "tests" / "repomod" / "fixtures" / "snapshot"
    fixtures.mkdir(parents=True)
    write(fixtures, '[package]\nname = "upstream"\n\n[dependencies]\nrustix = "1.1.4"\n')
    vendor = root / "third_party" / "vendored"
    vendor.mkdir(parents=True)
    write(vendor, '[package]\nname = "vendored"\n\n[dependencies]\nrustix = "1"\n')
    assert _check(root, repository("tool", "rust")) == []


def test_member_crate_is_scanned_and_fixed(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    write(root, '[workspace]\nmembers = ["crates/demo"]\n')
    write_member(
        root,
        '[package]\nname = "demo"\nversion = "0.1.0"\n\n[dependencies]\nrustix = "1"\n',
    )
    findings = _check(root, repository("tool", "rust"))
    assert [f.file for f in findings] == ["crates/demo/Cargo.toml"]


def test_multiline_features_list_is_rewritten(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    write(
        root,
        '[package]\nname = "demo"\nversion = "0.1.0"\n\n[dependencies]\n'
        'rustix = { version = "1.1.4", features = [\n'
        '    "event",\n'
        '    "fs",\n'
        '] }\n',
    )
    with mock.patch.object(laputa_rustix, "_refresh_locks"):
        plan = _fix(root, repository("tool", "rust"))
    body = plan["Cargo.toml"]
    assert body == (
        '[package]\nname = "demo"\nversion = "0.1.0"\n\n[dependencies]\n'
        'rustix = { package = "laputa-rustix", version = "1.1.4", features = [\n'
        '    "event",\n'
        '    "fs",\n'
        '] }\n'
    )


def test_fix_rewrites_bare_and_inline_specs(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    write(
        root,
        '[package]\nname = "demo"\nversion = "0.1.0"\n\n[dependencies]\n'
        'rustix = "1"\n'
        'serde = "1"\n'
        'tokio = { version = "1", features = ["rt"] }\n'
        'rustix_fs = { version = "1", package = "laputa-rustix" }\n',
    )
    with mock.patch.object(laputa_rustix, "_refresh_locks"):
        plan = _fix(root, repository("tool", "rust"))
    assert set(plan) == {"Cargo.toml"}
    assert plan["Cargo.toml"] == (
        '[package]\nname = "demo"\nversion = "0.1.0"\n\n[dependencies]\n'
        'rustix = { package = "laputa-rustix", version = "1" }\n'
        'serde = "1"\n'
        'tokio = { version = "1", features = ["rt"] }\n'
        'rustix_fs = { version = "1", package = "laputa-rustix" }\n'
    )


def test_fix_is_idempotent(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    write(root, '[package]\nname = "demo"\nversion = "0.1.0"\n\n[dependencies]\nrustix = "1"\n')
    with mock.patch.object(laputa_rustix, "_refresh_locks"):
        first = _fix(root, repository("tool", "rust"))
    write(root, first["Cargo.toml"])
    with mock.patch.object(laputa_rustix, "_refresh_locks"):
        second = _fix(root, repository("tool", "rust"))
    assert second == {}


def test_unparseable_spec_reports_operational(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    write(root, '[package]\nname = "demo"\nversion = "0.1.0"\n\n[dependencies]\nrustix = 1\n')
    findings = _check(root, repository("tool", "rust"))
    assert [f.kind for f in findings] == ["operational"]
    assert all(not f.fixable for f in findings)


def test_symlinked_manifest_reports_operational(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    write(tmp_path, '[package]\nname = "demo"\nversion = "0.1.0"\n\n[dependencies]\nrustix = "1"\n', "real.toml")
    (root / "Cargo.toml").symlink_to(tmp_path / "real.toml")
    findings = _check(root, repository("tool", "rust"))
    assert [f.kind for f in findings] == ["operational"]


def test_invalid_toml_reports_operational(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    write(root, "[dependencies\nrustix = ")
    findings = _check(root, repository("tool", "rust"))
    assert [f.kind for f in findings] == ["operational"]


def test_fixer_refreshes_lockfile(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    write(root, '[package]\nname = "demo"\nversion = "0.1.0"\n\n[dependencies]\nrustix = "1"\n')
    (root / "Cargo.lock").write_text(
        'version = 4\n[[package]]\nname = "demo"\nversion = "0.1.0"\n',
        encoding="utf-8",
    )

    def fake_run(cmd, **kwargs):
        class Result:
            returncode = 0
            stderr = ""
            stdout = ""

        project = Path(kwargs["cwd"])
        text = (project / "Cargo.lock").read_text(encoding="utf-8")
        (project / "Cargo.lock").write_text(text.replace("version = 4", "version = 4"), encoding="utf-8")
        return Result()

    with mock.patch.object(laputa_rustix.subprocess, "run", fake_run):
        plan = _fix(root, repository("tool", "rust"))
    assert set(plan) == {"Cargo.toml", "Cargo.lock"}


def test_fixer_reports_lock_refresh_failure(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    write(root, '[package]\nname = "demo"\nversion = "0.1.0"\n\n[dependencies]\nrustix = "1"\n')
    (root / "Cargo.lock").write_text("version = 4\n", encoding="utf-8")

    def fake_run(cmd, **kwargs):
        class Result:
            returncode = 101
            stderr = "error: no matching package named `laputa-rustix` found"
            stdout = ""

        return Result()

    with (
        mock.patch.object(laputa_rustix.subprocess, "run", fake_run),
        pytest.raises(RuntimeError, match="laputa-rustix"),
    ):
        _fix(root, repository("tool", "rust"))


def test_rule_is_registered() -> None:
    from repomod.rules import rules

    registered = {rule.id for rule in rules()}
    assert "laputa-rustix" in registered
    rule = next(rule for rule in rules() if rule.id == "laputa-rustix")
    assert rule.fix is not None
    assert rule.allowed_paths == ("Cargo.toml", "*/Cargo.toml", "Cargo.lock", "*/Cargo.lock")
