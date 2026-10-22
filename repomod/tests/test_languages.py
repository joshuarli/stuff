from __future__ import annotations

import shutil
from pathlib import Path

from repomod.rules.languages import _javascript_findings, _python_findings, _rust_findings
from repomod.types import RepoId, Repository


def repository(*tags: str) -> Repository:
    return Repository(RepoId("acme", "demo"), tuple(tags))


def test_rust_binary_requires_lock_without_a_language_tag_but_library_does_not(
    tmp_path: Path,
) -> None:
    (tmp_path / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "1.0.0"\nedition = "2024"\n',
        encoding="utf-8",
    )
    source = tmp_path / "src" / "main.rs"
    source.parent.mkdir()
    source.write_text("fn main() {}\n", encoding="utf-8")

    tool_findings = _rust_findings(tmp_path, repository("tool"))
    (source.parent / "main.rs").unlink()
    (source.parent / "lib.rs").write_text("pub fn api() {}\n", encoding="utf-8")
    library_findings = _rust_findings(tmp_path, repository("library", "rust"))

    assert any(finding.file == "Cargo.lock" for finding in tool_findings)
    assert not any(finding.file == "Cargo.lock" for finding in library_findings)


def test_rust_workspace_inheritance_is_checked_without_building(tmp_path: Path) -> None:
    member = tmp_path / "crates" / "api"
    member.mkdir(parents=True)
    (tmp_path / "Cargo.toml").write_text(
        '[workspace]\nmembers = ["crates/*"]\n[workspace.package]\nedition = "2024"\n',
        encoding="utf-8",
    )
    (member / "Cargo.toml").write_text(
        '[package]\nname = "api"\nversion = "1.0.0"\nedition.workspace = true\n',
        encoding="utf-8",
    )

    assert _rust_findings(tmp_path, repository("library")) == []


def test_typescript_lockfile_selection_and_declared_manager_are_consistent(
    tmp_path: Path,
) -> None:
    (tmp_path / "package.json").write_text('{"packageManager":"pnpm@10.0.0"}\n', encoding="utf-8")
    (tmp_path / "pnpm-lock.yaml").write_text("lockfileVersion: '9.0'\n", encoding="utf-8")
    (tmp_path / "tsconfig.json").write_text('{"compilerOptions":{"strict":true}}\n', encoding="utf-8")
    (tmp_path / "package-lock.json").write_text("{}\n", encoding="utf-8")

    findings = _javascript_findings(tmp_path, repository("app", "typescript"))

    assert len(findings) == 1
    assert "multiple package-manager lockfiles" in findings[0].message


def test_typescript_configuration_accepts_jsonc_comments_and_trailing_commas(
    tmp_path: Path,
) -> None:
    (tmp_path / "package.json").write_text('{"name":"demo"}\n', encoding="utf-8")
    (tmp_path / "tsconfig.json").write_text(
        '{\n  // TypeScript permits JSONC configuration.\n'
        '  "compilerOptions": { "strict": true, },\n'
        '  /* block comments are valid too */\n}\n',
        encoding="utf-8",
    )

    assert _javascript_findings(tmp_path, repository("app", "typescript")) == []


def test_python_uv_lock_python_range_drift_is_reported(tmp_path: Path) -> None:
    (tmp_path / "pyproject.toml").write_text(
        '[project]\nname = "demo"\nversion = "1.0.0"\nrequires-python = ">=3.13"\n',
        encoding="utf-8",
    )
    (tmp_path / "uv.lock").write_text('version = 1\nrequires-python = ">=3.12"\n', encoding="utf-8")

    findings = _python_findings(tmp_path, repository("tool", "python"))

    assert len(findings) == 1
    assert "requires-python differs" in findings[0].message


def test_language_tags_without_required_manifests_get_actionable_findings(
    tmp_path: Path,
) -> None:
    rust = _rust_findings(tmp_path, repository("tool", "rust"))
    javascript = _javascript_findings(tmp_path, repository("extension", "typescript"))
    python = _python_findings(tmp_path, repository("tool", "python"))

    assert "rust language tag has no Cargo.toml" in rust[0].message
    assert "has no package.json" in javascript[0].message
    assert "python language tag has no pyproject.toml" in python[0].message


def test_python_build_nested_first_party_sources_need_no_root_package_manifests(
    tmp_path: Path,
) -> None:
    fixture = Path(__file__).parent / "fixtures" / "languages" / "python_build"
    root = tmp_path / "python-build"
    shutil.copytree(fixture, root)
    repository = Repository(
        RepoId("joshuarli", "python-build"), ("build", "python", "rust")
    )

    assert not (root / "Cargo.toml").exists()
    assert not (root / "pyproject.toml").exists()
    assert (root / "buildsys" / "targets.py").is_file()
    assert _rust_findings(root, repository) == []
    assert _python_findings(root, repository) == []
