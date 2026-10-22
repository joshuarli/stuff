from __future__ import annotations

import glob
import json
import tomllib
from pathlib import Path
from typing import Any

from repomod.types import Finding, Repository, Rule, TagPredicate

_LOCKFILES = {
    "package-lock.json": "npm",
    "npm-shrinkwrap.json": "npm",
    "pnpm-lock.yaml": "pnpm",
    "yarn.lock": "yarn",
    "bun.lock": "bun",
    "bun.lockb": "bun",
}
_IGNORED_PACKAGES = {".git", ".work", "node_modules", "vendor", "dist", "build", ".venv"}


def _finding(repository: Repository, rule: str, path: str | None, message: str) -> Finding:
    return Finding(str(repository.id), rule, path, message)


def _read_toml(path: Path) -> tuple[dict[str, Any] | None, str | None]:
    try:
        value = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        return None, str(error)
    return value, None


def _cargo_candidates(root: Path) -> list[Path]:
    patterns = (
        "Cargo.toml",
        "*/Cargo.toml",
        "crates/*/Cargo.toml",
        "*/crates/*/Cargo.toml",
        "packages/*/Cargo.toml",
        "rust/*/Cargo.toml",
        "rust-cpython/overlay/Cargo.toml",
        "rust-cpython/overlay/Modules/*/Cargo.toml",
    )
    found = set()
    for pattern in patterns:
        found.update(Path(path) for path in glob.glob(str(root / pattern)))
    return sorted(found, key=lambda path: path.relative_to(root).as_posix().casefold())


def _safe_manifest(root: Path, path: Path) -> bool:
    if any(part in {".git", ".work", "node_modules", "vendor", "dist", ".venv", "target"}
        for part in path.relative_to(root).parts
    ):
        return False
    current = root
    for part in path.relative_to(root).parts:
        current = current / part
        if current.is_symlink():
            return False
    try:
        return path.is_file() and path.resolve().is_relative_to(root.resolve())
    except OSError:
        return False


def _rust_manifests(
    root: Path,
) -> tuple[list[tuple[Path, dict[str, Any], dict[str, Any]]], list[tuple[str, str]]]:
    candidates = _cargo_candidates(root)
    documents: dict[Path, dict[str, Any]] = {}
    problems: list[tuple[str, str]] = []
    for path in candidates:
        relative = path.relative_to(root).as_posix()
        if path.is_symlink():
            problems.append((relative, "Cargo manifest is a symlink and is not followed"))
            continue
        if not _safe_manifest(root, path):
            continue
        document, error = _read_toml(path)
        if document is None:
            problems.append((relative, f"Cargo manifest is unreadable: {error}"))
            continue
        documents[path] = document
    workspaces = {
        path: document
        for path, document in documents.items()
        if isinstance(document.get("workspace"), dict)
    }
    for workspace_path, document in tuple(workspaces.items()):
        workspace = document["workspace"]
        members = workspace.get("members", [])
        if not isinstance(members, list):
            problems.append(
                (workspace_path.relative_to(root).as_posix(), "workspace.members must be an array")
            )
            continue
        for pattern in members:
            if not isinstance(pattern, str):
                problems.append(
                    (workspace_path.relative_to(root).as_posix(), "workspace.members contains a non-string path")
                )
                continue
            for member_name in glob.glob(str(workspace_path.parent / pattern / "Cargo.toml")):
                member = Path(member_name)
                if not _safe_manifest(root, member) or member in documents:
                    continue
                member_document, error = _read_toml(member)
                if member_document is None:
                    problems.append(
                        (member.relative_to(root).as_posix(), f"Cargo manifest is unreadable: {error}")
                    )
                else:
                    documents[member] = member_document
    manifests = []
    for path, document in documents.items():
        parents = [
            workspace_path
            for workspace_path in workspaces
            if path == workspace_path or workspace_path.parent in path.parents
        ]
        workspace_path = max(parents, key=lambda item: len(item.parts)) if parents else path
        manifests.append((path, document, workspaces.get(workspace_path, document)))
    manifests.sort(key=lambda item: item[0].relative_to(root).as_posix().casefold())
    return manifests, sorted(problems)


def _rust_toolchain(root: Path, repository: Repository) -> list[Finding]:
    findings = []
    path = root / "rust-toolchain.toml"
    if path.is_symlink():
        return [_finding(repository, "rust-project", path.name, "rust-toolchain.toml is a symlink")]
    if path.is_file():
        document, error = _read_toml(path)
        toolchain = document.get("toolchain") if isinstance(document, dict) else None
        valid = document is not None and isinstance(toolchain, dict) and isinstance(toolchain.get("channel"), str)
        if not valid:
            detail = error or "missing channel"
            findings.append(
                _finding(repository, "rust-project", path.name, f"declare a readable toolchain.channel: {detail}")
            )
    for name in ("rustfmt.toml", ".rustfmt.toml", "clippy.toml", ".clippy.toml"):
        config = root / name
        if not config.exists() and not config.is_symlink():
            continue
        if config.is_symlink():
            findings.append(_finding(repository, "rust-project", name, "Rust tool configuration is a symlink"))
            continue
        document, error = _read_toml(config)
        if document is None:
            message = f"Rust tool configuration is invalid TOML: {error}"
            findings.append(_finding(repository, "rust-project", name, message))
    return findings


def _rust_binary_target(path: Path, manifest: dict[str, Any]) -> bool:
    package_root = path.parent
    if (package_root / "src" / "main.rs").is_file():
        return True
    src_bin = package_root / "src" / "bin"
    if not src_bin.is_symlink() and src_bin.is_dir() and any(
        child.is_file() and not child.is_symlink() and child.suffix == ".rs" for child in src_bin.iterdir()
    ):
        return True
    targets = manifest.get("bin", [])
    if isinstance(targets, list):
        for target in targets:
            if not isinstance(target, dict):
                continue
            path_value = target.get("path")
            if isinstance(path_value, str):
                try:
                    if (package_root / path_value).resolve().is_relative_to((package_root / "src").resolve()):
                        return True
                except OSError:
                    continue
    return False


def _rust_findings(root: Path, repository: Repository) -> list[Finding]:
    manifests, problems = _rust_manifests(root)
    findings = [_finding(repository, "rust-project", path, message) for path, message in problems]
    if not manifests:
        if "rust" in repository.tags:
            findings.append(_finding(repository, "rust-project", None, "rust language tag has no Cargo.toml"))
        return findings
    has_binary = False
    for path, document, workspace_document in manifests:
        package = document.get("package")
        if not isinstance(package, dict):
            continue
        workspace = workspace_document.get("workspace")
        workspace_package = workspace.get("package", {}) if isinstance(workspace, dict) else {}
        workspace_package = workspace_package if isinstance(workspace_package, dict) else {}
        workspace_lints = workspace.get("lints") if isinstance(workspace, dict) else None
        relative = path.relative_to(root).as_posix()
        edition = package.get("edition")
        if edition == {"workspace": True}:
            if not isinstance(workspace_package.get("edition"), (str, int)):
                findings.append(
                    _finding(repository, "rust-project", relative, "edition.workspace = true has no workspace edition")
                )
        elif edition is None:
            findings.append(
                _finding(
                    repository,
                    "rust-project",
                    relative,
                    "declare package.edition or inherit workspace.package.edition",
                )
            )
        findings.extend(
            _finding(
                repository,
                "rust-project",
                relative,
                f"{field}.workspace = true has no workspace.package.{field}",
            )
            for field in ("rust-version", "license", "repository", "readme", "version", "publish")
            if package.get(field) == {"workspace": True} and field not in workspace_package
        )
        package_lints = document.get("lints")
        if (
            isinstance(package_lints, dict)
            and package_lints.get("workspace") is True
            and not isinstance(workspace_lints, dict)
        ):
            findings.append(
                _finding(repository, "rust-project", relative, "lints.workspace = true has no workspace.lints")
            )
        has_binary |= _rust_binary_target(path, document)
    if has_binary and not (root / "Cargo.lock").is_file():
        findings.append(
            _finding(repository, "rust-project", "Cargo.lock", "commit Cargo.lock for the Rust executable")
        )
    lockfile = root / "Cargo.lock"
    if lockfile.is_symlink():
        findings.append(
            _finding(repository, "rust-project", "Cargo.lock", "Cargo.lock is a symlink and is not followed")
        )
    elif lockfile.is_file():
        _, error = _read_toml(lockfile)
        if error is not None:
            findings.append(
                _finding(repository, "rust-project", "Cargo.lock", f"Cargo.lock is invalid TOML: {error}")
            )
    findings.extend(_rust_toolchain(root, repository))
    return findings


def _jsonc_source(source: str) -> tuple[str | None, str | None]:
    characters = list(source)
    in_string = False
    escaped = False
    index = 0
    while index < len(source):
        character = source[index]
        if in_string:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                in_string = False
        elif character == '"':
            in_string = True
        elif source.startswith("//", index):
            while index < len(source) and source[index] not in "\r\n":
                characters[index] = " "
                index += 1
            continue
        elif source.startswith("/*", index):
            start = index
            end = source.find("*/", index + 2)
            if end < 0:
                return None, f"unterminated block comment at offset {start}"
            for comment_index in range(index, end + 2):
                if source[comment_index] not in "\r\n":
                    characters[comment_index] = " "
            index = end + 2
            continue
        index += 1
    cleaned = "".join(characters)
    characters = list(cleaned)
    in_string = False
    escaped = False
    for index, character in enumerate(cleaned):
        if in_string:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                in_string = False
        elif character == '"':
            in_string = True
        elif character == ",":
            following = index + 1
            while following < len(cleaned) and cleaned[following].isspace():
                following += 1
            if following < len(cleaned) and cleaned[following] in "]}":
                characters[index] = " "
    return "".join(characters), None


def _json_object(
    path: Path, *, allow_jsonc: bool = False
) -> tuple[dict[str, Any] | None, str | None]:
    try:
        source = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        return None, str(error)
    if allow_jsonc:
        source, error = _jsonc_source(source)
        if source is None:
            return None, error
    try:
        value = json.loads(source)
    except json.JSONDecodeError as error:
        return None, str(error)
    if not isinstance(value, dict):
        return None, "top-level value must be an object"
    return value, None


def _js_manifests(root: Path) -> list[Path]:
    paths = []
    for child in (root, *sorted(root.iterdir(), key=lambda item: item.name.casefold())):
        if child != root and (
            child.is_symlink() or not child.is_dir() or child.name in _IGNORED_PACKAGES
        ):
            continue
        manifest = child / "package.json"
        if manifest.is_file() and not manifest.is_symlink():
            paths.append(manifest)
    return paths


def _javascript_findings(root: Path, repository: Repository) -> list[Finding]:
    manifests = _js_manifests(root)
    findings = []
    if not manifests:
        if set(repository.tags) & {"javascript", "typescript"}:
            findings.append(
                _finding(repository, "javascript-project", None, "JavaScript or TypeScript tag has no package.json")
            )
        return findings
    root_package = None
    for path in manifests:
        relative = path.relative_to(root).as_posix()
        package, error = _json_object(path)
        if package is None:
            findings.append(
                _finding(repository, "javascript-project", relative, f"package manifest is invalid JSON: {error}")
            )
            continue
        if path == root / "package.json":
            root_package = package
        for name in ("tsconfig.json",):
            config = path.parent / name
            if config.is_symlink():
                relative_config = config.relative_to(root).as_posix()
                findings.append(
                    _finding(repository, "javascript-project", relative_config, "TypeScript configuration is a symlink")
                )
            elif config.is_file():
                _, config_error = _json_object(config, allow_jsonc=True)
                if config_error is not None:
                    relative_config = config.relative_to(root).as_posix()
                    findings.append(
                        _finding(
                            repository,
                            "javascript-project",
                            relative_config,
                            f"TypeScript configuration is invalid JSON: {config_error}",
                        )
                    )
    locks = [name for name in _LOCKFILES if (root / name).is_file() or (root / name).is_symlink()]
    managers = {_LOCKFILES[name] for name in locks}
    if len(locks) > 1:
        lock_names = ", ".join(sorted(locks))
        findings.append(
            _finding(
                repository,
                "javascript-project",
                None,
                "multiple package-manager lockfiles are present: " + lock_names,
            )
        )
    if isinstance(root_package, dict):
        manager = root_package.get("packageManager")
        if isinstance(manager, str) and len(managers) == 1 and manager.split("@", 1)[0] not in managers:
            lock_manager = next(iter(managers))
            message = f"packageManager {manager} conflicts with lockfile manager {lock_manager}"
            findings.append(_finding(repository, "javascript-project", "package.json", message))
        elif isinstance(manager, str) and not managers:
            message = f"packageManager {manager} is declared without a matching lockfile"
            findings.append(_finding(repository, "javascript-project", "package.json", message))
    return findings


def _python_source_present(root: Path) -> bool:
    patterns = (
        "*.py",
        "build/*.py",
        "buildsys/*.py",
        "src/*.py",
        "python/*.py",
        "scripts/*.py",
        "tools/*.py",
        ".github/scripts/*.py",
        "packages/*/src/*.py",
    )
    for pattern in patterns:
        for candidate_name in glob.glob(str(root / pattern)):
            candidate = Path(candidate_name)
            if candidate.is_symlink():
                continue
            try:
                if candidate.is_file() and candidate.resolve().is_relative_to(root.resolve()):
                    return True
            except OSError:
                continue
    return False


def _python_findings(root: Path, repository: Repository) -> list[Finding]:
    path = root / "pyproject.toml"
    if path.is_symlink():
        return [_finding(repository, "python-project", path.name, "pyproject.toml is a symlink and is not followed")]
    if not path.is_file():
        if "python" in repository.tags and not _python_source_present(root):
            return [_finding(repository, "python-project", None, "python language tag has no pyproject.toml")]
        return []
    document, error = _read_toml(path)
    if document is None:
        return [_finding(repository, "python-project", path.name, f"pyproject.toml is invalid TOML: {error}")]
    project = document.get("project")
    uv_lock = root / "uv.lock"
    findings = []
    if uv_lock.is_symlink():
        findings.append(_finding(repository, "python-project", "uv.lock", "uv.lock is a symlink and is not followed"))
    elif uv_lock.is_file():
        lock, lock_error = _read_toml(uv_lock)
        if lock is None:
            findings.append(_finding(repository, "python-project", "uv.lock", f"uv.lock is invalid TOML: {lock_error}"))
        elif isinstance(project, dict):
            project_python = project.get("requires-python")
            lock_python = lock.get("requires-python")
            if isinstance(project_python, str) and isinstance(lock_python, str) and project_python != lock_python:
                findings.append(
                    _finding(
                        repository,
                        "python-project",
                        "uv.lock",
                        "uv.lock requires-python differs from project.requires-python; run uv lock",
                    )
                )
    return findings


def language_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            "rust-project",
            "Check Cargo manifests, workspace inheritance, toolchain config, and binary lockfiles",
            TagPredicate(),
            _rust_findings,
            None,
            None,
            (),
        ),
        Rule(
            "javascript-project",
            "Check JavaScript and TypeScript package-manager metadata and configuration",
            TagPredicate(),
            _javascript_findings,
            None,
            None,
            (),
        ),
        Rule(
            "python-project",
            "Check Python metadata and uv lock coherence",
            TagPredicate(),
            _python_findings,
            None,
            None,
            (),
        ),
    )
