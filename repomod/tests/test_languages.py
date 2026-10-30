from __future__ import annotations

import shutil
from pathlib import Path

from repomod.rules.languages import (
    _javascript_findings,
    _language_sources,
    _python_findings,
    _rust_findings,
)
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


def test_typescript_source_is_found_in_nested_package_directories(tmp_path: Path) -> None:
    package = tmp_path / "app-ts"
    (package / "src").mkdir(parents=True)
    (package / "package.json").write_text("{}\n", encoding="utf-8")
    (package / "src" / "index.ts").write_text("export {};\n", encoding="utf-8")

    assert "typescript" in _language_sources(tmp_path)
    assert "javascript" not in _language_sources(tmp_path)


def _write(path: Path, text: str = "") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def test_python_source_is_found_beside_nested_pyproject_manifests(tmp_path: Path) -> None:
    _write(tmp_path / "services" / "api" / "pyproject.toml", "[project]\n")
    _write(tmp_path / "services" / "api" / "src" / "api" / "app.py")

    assert "python" in _language_sources(tmp_path)


def test_python_source_without_a_manifest_in_unknown_directories_is_not_detected(
    tmp_path: Path,
) -> None:
    _write(tmp_path / "services" / "api" / "src" / "api" / "app.py")

    assert "python" not in _language_sources(tmp_path)


def test_go_source_is_found_beside_nested_go_mod_only(tmp_path: Path) -> None:
    _write(tmp_path / "services" / "edge" / "go.mod", "module edge\n")
    _write(tmp_path / "services" / "edge" / "cmd" / "edge" / "main.go")
    assert "go" in _language_sources(tmp_path)

    other = tmp_path / "other"
    _write(other / "cmd" / "tool" / "main.go")
    assert "go" not in _language_sources(other)


def test_rust_source_is_found_beside_nested_cargo_manifests(tmp_path: Path) -> None:
    _write(tmp_path / "components" / "engine" / "Cargo.toml", '[package]\nname = "engine"\n')
    _write(tmp_path / "components" / "engine" / "src" / "lib.rs")

    assert "rust" in _language_sources(tmp_path)


def test_manifests_in_vendored_and_fixture_trees_are_ignored(tmp_path: Path) -> None:
    for ignored in ("vendor", "third_party", "tests", "fixtures", "node_modules"):
        _write(tmp_path / ignored / "dep" / "pyproject.toml", "[project]\n")
        _write(tmp_path / ignored / "dep" / "src" / "dep" / "mod.py")
        _write(tmp_path / ignored / "dep" / "go.mod", "module dep\n")
        _write(tmp_path / ignored / "dep" / "main.go")
        _write(tmp_path / ignored / "dep" / "Cargo.toml", '[package]\nname = "dep"\n')
        _write(tmp_path / ignored / "dep" / "src" / "lib.rs")

    assert _language_sources(tmp_path) == set()


def test_symlinked_directories_are_not_searched_for_manifests(tmp_path: Path) -> None:
    outside = tmp_path / "outside"
    _write(outside / "pyproject.toml", "[project]\n")
    _write(outside / "src" / "pkg" / "mod.py")
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / "linked").symlink_to(outside, target_is_directory=True)

    assert _language_sources(repo) == set()
