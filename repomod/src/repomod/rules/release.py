from __future__ import annotations

import glob
import json
import re
import tomllib
from pathlib import Path
from typing import Any

from repomod.types import Finding, Repository, Rule

_POLICY_PATH = Path(__file__).resolve().parents[3] / "release-policy.toml"


def _load_policy() -> dict[str, Any]:
    try:
        value = tomllib.loads(_POLICY_PATH.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise RuntimeError(f"cannot read release policy at {_POLICY_PATH}: {error}") from error
    if set(value) != {"cargo", "pypi", "npm"}:
        raise RuntimeError("release-policy.toml must define cargo, pypi, and npm")
    if (
        not isinstance(value["cargo"].get("version_fields"), list)
        or not isinstance(value["cargo"].get("required_fields"), list)
        or not isinstance(value["cargo"].get("bump_command"), str)
        or not isinstance(value["pypi"].get("version_field"), str)
        or not isinstance(value["pypi"].get("build_backend"), str)
        or not isinstance(value["pypi"].get("bump_command"), str)
        or not isinstance(value["npm"].get("version_field"), str)
        or not isinstance(value["npm"].get("lockfiles"), list)
        or not isinstance(value["npm"].get("bump_command"), str)
    ):
        raise RuntimeError("release-policy.toml has an invalid ecosystem contract")
    return value


_POLICY = _load_policy()


def _manifest(path: Path) -> dict[str, Any] | None:
    try:
        value = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError):
        return None
    return value


def _cargo_paths(root: Path) -> list[Path]:
    manifest_path = root / "Cargo.toml"
    if manifest_path.is_symlink():
        return []
    manifest = _manifest(manifest_path)
    if manifest is None:
        return []
    paths = [manifest_path] if isinstance(manifest.get("package"), dict) else []
    workspace = manifest.get("workspace")
    members = workspace.get("members", []) if isinstance(workspace, dict) else []
    if not isinstance(members, list):
        return paths
    for member in members:
        if not isinstance(member, str):
            continue
        for match in glob.glob(str(root / member / "Cargo.toml")):
            path = Path(match)
            if path.is_file() and not path.is_symlink() and path.resolve().is_relative_to(root.resolve()):
                package_manifest = _manifest(path)
                if package_manifest is not None and isinstance(package_manifest.get("package"), dict):
                    paths.append(path)
    return sorted(set(paths), key=lambda item: item.relative_to(root).as_posix().casefold())


def _cargo_findings(root: Path, repository: Repository) -> list[Finding]:
    findings: list[Finding] = []
    workspace = _manifest(root / "Cargo.toml") or {}
    workspace_package = workspace.get("workspace", {})
    workspace_package = workspace_package.get("package", {}) if isinstance(workspace_package, dict) else {}
    workspace_package = workspace_package if isinstance(workspace_package, dict) else {}
    for path in _cargo_paths(root):
        relative = path.relative_to(root).as_posix()
        manifest = _manifest(path) or {}
        package = manifest.get("package", {})
        if not isinstance(package, dict):
            continue
        required_fields = set(_POLICY["cargo"]["required_fields"])
        if "name" in required_fields and not isinstance(package.get("name"), str):
            findings.append(Finding(str(repository.id), "cargo-release", relative, "declare the published package name"))
        inherited_version = package.get("version") == {"workspace": True}
        version = package.get("version")
        if inherited_version and "version" not in workspace_package:
            findings.append(Finding(str(repository.id), "cargo-release", relative, "version.workspace = true has no workspace version"))
        elif not inherited_version and "version" in required_fields and (not isinstance(version, str) or not version):
            findings.append(Finding(str(repository.id), "cargo-release", relative, "set a package version or explicit workspace inheritance"))
        inherited_license = package.get("license") == {"workspace": True}
        license_value = package.get("license")
        if inherited_license and "license" in required_fields and not ("license" in workspace_package or "license-file" in workspace_package):
            findings.append(Finding(str(repository.id), "cargo-release", relative, "license.workspace = true has no workspace license"))
        elif not inherited_license and "license" in required_fields and not (
            isinstance(license_value, str)
            and license_value
            or isinstance(package.get("license-file"), str)
            and package["license-file"]
        ):
            findings.append(Finding(str(repository.id), "cargo-release", relative, "declare the crate license explicitly"))
        inherited_repository = package.get("repository") == {"workspace": True}
        if inherited_repository and "repository" in required_fields and "repository" not in workspace_package:
            findings.append(Finding(str(repository.id), "cargo-release", relative, "repository.workspace = true has no workspace repository"))
        elif not inherited_repository and "repository" in required_fields and not isinstance(package.get("repository"), str):
            findings.append(
                Finding(
                    str(repository.id),
                    "cargo-release",
                    relative,
                    "repository metadata is missing",
                    fixable=True,
                )
            )
        if "readme" in required_fields:
            readme = package.get("readme")
            if readme == {"workspace": True}:
                readme = workspace_package.get("readme")
                readme_path = root / readme if isinstance(readme, str) else None
            elif isinstance(readme, str):
                readme_path = path.parent / readme
            else:
                readme_path = path.parent / "README.md"
            if readme_path is None or not readme_path.is_file() or readme_path.is_symlink():
                findings.append(Finding(str(repository.id), "cargo-release", relative, "declare an existing package README or add the Cargo default README.md"))
    return findings


def _toml_set_value(source: str, section: str, key: str, value: str) -> str:
    header = f"[{section}]"
    lines = source.splitlines(keepends=True)
    section_start = next((index for index, line in enumerate(lines) if line.strip() == header), None)
    assignment = f'{key} = "{value}"\n'
    if section_start is None:
        return f"{source.rstrip()}\n\n{header}\n{assignment}"
    section_end = next(
        (index for index in range(section_start + 1, len(lines)) if lines[index].lstrip().startswith("[")),
        len(lines),
    )
    for index in range(section_start + 1, section_end):
        if lines[index].lstrip().startswith(f"{key} "):
            return source
    lines.insert(section_end, assignment)
    return "".join(lines)


def _cargo_fix(root: Path, repository: Repository) -> dict[str, str]:
    github = f"https://github.com/{repository.id.owner}/{repository.id.name}"
    changes: dict[str, str] = {}
    for path in _cargo_paths(root):
        manifest = _manifest(path) or {}
        package = manifest.get("package", {})
        if not isinstance(package, dict):
            continue
        if "repository" in package:
            continue
        relative = path.relative_to(root).as_posix()
        updated = _toml_set_value(path.read_text(encoding="utf-8"), "package", "repository", github)
        if updated != path.read_text(encoding="utf-8"):
            changes[relative] = updated
    return changes


def _python_package(root: Path) -> tuple[Path | None, dict[str, Any] | None]:
    path = root / "pyproject.toml"
    return (path, _manifest(path)) if path.is_file() and not path.is_symlink() else (None, None)


def _python_findings(root: Path, repository: Repository) -> list[Finding]:
    path, document = _python_package(root)
    if path is None or document is None:
        return [Finding(str(repository.id), "python-release", "pyproject.toml", "PyPI tag requires readable project metadata")]
    raw_project = document.get("project", {})
    project = raw_project if isinstance(raw_project, dict) else {}
    build = document.get("build-system", {})
    findings: list[Finding] = []
    dynamic = project.get("dynamic", []) if isinstance(project, dict) else []
    if isinstance(project, dict) and isinstance(project.get("version"), str):
        pass
    elif isinstance(dynamic, list) and "version" in dynamic:
        findings.append(Finding(str(repository.id), "python-release", "pyproject.toml", "version is dynamic; document its authoritative source and bump procedure"))
    else:
        findings.append(Finding(str(repository.id), "python-release", "pyproject.toml", "set project.version or explicitly declare dynamic versioning"))
    expected_backend = _POLICY["pypi"].get("build_backend")
    if not isinstance(build, dict) or build.get("build-backend") != expected_backend:
        findings.append(Finding(str(repository.id), "python-release", "pyproject.toml", f"canonical PyPI policy uses {expected_backend}; backend migration needs review"))
    if "readme" not in project and (root / "README.md").is_file():
        findings.append(Finding(str(repository.id), "python-release", "pyproject.toml", "declare the existing README as distribution metadata", fixable=True))
    scripts = [root / ".github" / "workflows" / name for name in ("publish.yml", "release.yml", "pypi.yml")]
    if not any(path.is_file() for path in scripts):
        findings.append(Finding(str(repository.id), "python-release", None, "no release workflow found; verify the intended publishing path"))
    return findings


def _python_fix(root: Path, repository: Repository) -> dict[str, str]:
    path, document = _python_package(root)
    if path is None or document is None or not (root / "README.md").is_file():
        return {}
    project = document.get("project", {})
    if not isinstance(project, dict) or "readme" in project:
        return {}
    source = path.read_text(encoding="utf-8")
    lines = source.splitlines(keepends=True)
    section = next((index for index, line in enumerate(lines) if line.strip() == "[project]"), None)
    if section is None:
        return {}
    end = next((index for index in range(section + 1, len(lines)) if lines[index].lstrip().startswith("[")), len(lines))
    lines.insert(end, 'readme = "README.md"\n')
    return {"pyproject.toml": "".join(lines)}


def _json_load(path: Path) -> dict[str, Any] | None:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError):
        return None
    if not isinstance(value, dict) or any(not isinstance(key, str) for key in value):
        return None
    return value


def _npm_findings(root: Path, repository: Repository) -> list[Finding]:
    package = _json_load(root / "package.json")
    if package is None:
        return [Finding(str(repository.id), "npm-release", "package.json", "npm tag requires readable package metadata")]
    version = package.get("version")
    if not isinstance(version, str):
        return [Finding(str(repository.id), "npm-release", "package.json", "declare one authoritative package version")]
    findings: list[Finding] = []
    lock_names = _POLICY["npm"].get("lockfiles")
    if not isinstance(lock_names, list) or any(not isinstance(name, str) for name in lock_names):
        raise RuntimeError("release-policy.toml npm.lockfiles must be an array of strings")
    present = [name for name in lock_names if (root / name).is_file()]
    if len(present) > 1:
        findings.append(Finding(str(repository.id), "npm-release", "package.json", f"multiple package manager lockfiles are present: {', '.join(sorted(present))}"))
        return findings
    if not present:
        findings.append(Finding(str(repository.id), "npm-release", None, "commit one lockfile for the package manager used to publish"))
        return findings
    lock_name = present[0]
    manager_by_lock = {
        "package-lock.json": "npm",
        "npm-shrinkwrap.json": "npm",
        "pnpm-lock.yaml": "pnpm",
        "yarn.lock": "yarn",
        "bun.lock": "bun",
        "bun.lockb": "bun",
    }
    package_manager = package.get("packageManager")
    selected_manager = manager_by_lock[lock_name]
    if isinstance(package_manager, str) and package_manager.split("@", 1)[0] != selected_manager:
        findings.append(Finding(str(repository.id), "npm-release", "package.json", f"packageManager selects {package_manager}, but {lock_name} belongs to {selected_manager}"))
    if lock_name in {"package-lock.json", "npm-shrinkwrap.json"}:
        lock = root / lock_name
        if lock.is_symlink():
            findings.append(Finding(str(repository.id), "npm-release", lock_name, "lockfile is a symlink and cannot be repaired safely"))
            return findings
        lock_data = _json_load(lock)
        packages = lock_data.get("packages") if isinstance(lock_data, dict) else None
        root_package = packages.get("") if isinstance(packages, dict) else None
        top_version = lock_data.get("version") if isinstance(lock_data, dict) else None
        root_version = root_package.get("version") if isinstance(root_package, dict) else None
        if top_version != version or (isinstance(root_package, dict) and root_version != version):
            findings.append(Finding(str(repository.id), "npm-release", lock_name, f"lockfile versions must match package.json ({version})", fixable=True))
    elif lock_name == "bun.lockb":
        findings.append(Finding(str(repository.id), "npm-release", lock_name, "binary lockfile version metadata cannot be checked without the Bun CLI"))
    return findings


def _npm_fix(root: Path, repository: Repository) -> dict[str, str]:
    package = _json_load(root / "package.json")
    lock_names = _POLICY["npm"].get("lockfiles", [])
    present = [name for name in lock_names if isinstance(name, str) and (root / name).is_file()]
    if len(present) != 1 or present[0] not in {"package-lock.json", "npm-shrinkwrap.json"}:
        return {}
    lock_path = root / present[0]
    lock = _json_load(lock_path) if not lock_path.is_symlink() else None
    version = package.get("version") if package is not None else None
    packages = lock.get("packages") if lock is not None else None
    root_package = packages.get("") if isinstance(packages, dict) else None
    if not isinstance(version, str) or not isinstance(packages, dict) or not isinstance(root_package, dict):
        return {}
    if root_package.get("version") == version:
        return {}
    source = lock_path.read_text(encoding="utf-8")
    top_version = _json_string_value_span(source, "version")
    if top_version is None:
        return {}
    updated = source[: top_version[0]] + json.dumps(version) + source[top_version[1] :]
    package_object = re.search(r'"packages"\s*:\s*\{\s*""\s*:\s*\{', updated)
    if package_object is not None:
        package_start = package_object.end() - 1
        package_end = _json_object_end(updated, package_start)
        package_object_text = updated[package_start : package_end + 1]
        package_version = _json_string_value_span(package_object_text, "version")
        if package_version is None:
            return {}
        package_object_text = package_object_text[: package_version[0]] + json.dumps(version) + package_object_text[package_version[1] :]
        updated = updated[:package_start] + package_object_text + updated[package_end + 1 :]
    elif lock.get("lockfileVersion") != 1:
        return {}
    if updated == source:
        return {}
    return {"package-lock.json": updated}


def _json_object_end(source: str, start: int) -> int:
    depth = 0
    quote = False
    escaped = False
    for index in range(start, len(source)):
        character = source[index]
        if quote:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                quote = False
        elif character == '"':
            quote = True
        elif character == "{":
            depth += 1
        elif character == "}":
            depth -= 1
            if depth == 0:
                return index
    return len(source) - 1


def _json_string_value_span(source: str, wanted: str) -> tuple[int, int] | None:
    depth = 0
    index = 0
    while index < len(source):
        character = source[index]
        if character == '"':
            end = index + 1
            escaped = False
            while end < len(source):
                current = source[end]
                if escaped:
                    escaped = False
                elif current == "\\":
                    escaped = True
                elif current == '"':
                    break
                end += 1
            if depth == 1:
                try:
                    key = json.loads(source[index : end + 1])
                except json.JSONDecodeError:
                    return None
                cursor = end + 1
                while cursor < len(source) and source[cursor].isspace():
                    cursor += 1
                if key == wanted and cursor < len(source) and source[cursor] == ":":
                    cursor += 1
                    while cursor < len(source) and source[cursor].isspace():
                        cursor += 1
                    if cursor < len(source) and source[cursor] == '"':
                        value_end = cursor + 1
                        escaped = False
                        while value_end < len(source):
                            current = source[value_end]
                            if escaped:
                                escaped = False
                            elif current == "\\":
                                escaped = True
                            elif current == '"':
                                return cursor, value_end + 1
                            value_end += 1
                        return None
            index = end + 1
            continue
        if character == "{":
            depth += 1
        elif character == "}":
            depth -= 1
        index += 1
    return None


def _release_rule(tag: str, rule_id: str, description: str, check, fix, paths: tuple[str, ...]) -> Rule:
    return Rule(rule_id, description, frozenset({tag}), check, fix, f"chore(repomod): adopt {rule_id} policy", paths)


def release_rules() -> tuple[Rule, ...]:
    return (
        _release_rule("crates-io", "cargo-release", "Check crate metadata and version bump procedure", _cargo_findings, _cargo_fix, ("**/Cargo.toml",)),
        _release_rule("npm", "npm-release", "Check npm package and lockfile version ownership", _npm_findings, _npm_fix, ("package-lock.json",)),
        _release_rule("pypi", "python-release", "Check PyPI build, version and release conventions", _python_findings, _python_fix, ("pyproject.toml",)),
    )
