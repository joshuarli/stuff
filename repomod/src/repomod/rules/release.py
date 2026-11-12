from __future__ import annotations

import fnmatch
import glob
import json
import re
import tomllib
from pathlib import Path
from typing import Any

from repomod.git import expected_origin
from repomod.io import safe_relative
from repomod.types import Finding, Repository, Rule, TagPredicate, violation

_POLICY_PATH = Path(__file__).resolve().parents[3] / "release-policy.toml"


def _load_policy() -> dict[str, Any]:
    try:
        value = tomllib.loads(_POLICY_PATH.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise RuntimeError(
            f"cannot read release policy at {_POLICY_PATH}: {error}"
        ) from error
    if set(value) != {"cargo", "pypi", "npm", "binary"}:
        raise RuntimeError("release-policy.toml must define cargo, pypi, npm, and binary")
    if any(not isinstance(value[name], dict) for name in ("cargo", "pypi", "npm", "binary")):
        raise RuntimeError("release-policy.toml ecosystem contracts must be tables")
    cargo = value["cargo"]
    pypi = value["pypi"]
    npm = value["npm"]
    binary = value["binary"]
    if (
        not isinstance(cargo.get("version_fields"), list)
        or any(not isinstance(field, str) for field in cargo["version_fields"])
        or not isinstance(cargo.get("required_fields"), list)
        or any(not isinstance(field, str) for field in cargo["required_fields"])
        or not isinstance(cargo.get("bump_command"), str)
        or not cargo["bump_command"].strip()
        or not isinstance(cargo.get("bump_scripts"), list)
        or any(not isinstance(path, str) or not path for path in cargo["bump_scripts"])
        or len(cargo["bump_scripts"]) != len(set(cargo["bump_scripts"]))
        or not isinstance(cargo.get("bump_requirements"), list)
        or any(not isinstance(item, str) or not item for item in cargo["bump_requirements"])
        or len(cargo["bump_requirements"]) != len(set(cargo["bump_requirements"]))
        or not isinstance(pypi.get("version_field"), str)
        or not isinstance(pypi.get("build_backend"), str)
        or not isinstance(pypi.get("bump_command"), str)
        or not pypi["bump_command"].strip()
        or not isinstance(npm.get("version_field"), str)
        or not isinstance(npm.get("lockfiles"), list)
        or any(not isinstance(name, str) for name in npm["lockfiles"])
        or len(npm["lockfiles"]) != len(set(npm["lockfiles"]))
        or not isinstance(npm.get("bump_command"), str)
        or not npm["bump_command"].strip()
    ):
        raise RuntimeError("release-policy.toml has an invalid ecosystem contract")
    if set(cargo["required_fields"]) - {
        "name",
        "version",
        "license",
        "repository",
        "readme",
    }:
        raise RuntimeError(
            "release-policy.toml cargo.required_fields contains an unsupported field"
        )
    if set(cargo["version_fields"]) - {"package.version", "workspace.package.version"}:
        raise RuntimeError(
            "release-policy.toml cargo.version_fields contains an unsupported source"
        )
    if any(Path(path).is_absolute() or ".." in Path(path).parts for path in cargo["bump_scripts"]):
        raise RuntimeError("release-policy.toml cargo.bump_scripts must stay within the repository")
    if set(npm["lockfiles"]) - {
        "package-lock.json",
        "npm-shrinkwrap.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "bun.lock",
        "bun.lockb",
    }:
        raise RuntimeError(
            "release-policy.toml npm.lockfiles contains an unsupported file"
        )
    if (
        not isinstance(binary.get("portable_targets"), list)
        or any(not isinstance(target, str) for target in binary["portable_targets"])
        or len(binary["portable_targets"]) != len(set(binary["portable_targets"]))
        or not isinstance(binary.get("macos_targets"), list)
        or any(not isinstance(target, str) for target in binary["macos_targets"])
        or len(binary["macos_targets"]) != len(set(binary["macos_targets"]))
        or not isinstance(binary.get("linux_arches"), list)
        or any(not isinstance(arch, str) for arch in binary["linux_arches"])
        or not isinstance(binary.get("linux_musl_suffix"), str)
        or not binary["linux_musl_suffix"]
        or not isinstance(binary.get("forbidden_target_markers"), list)
        or any(not isinstance(marker, str) for marker in binary["forbidden_target_markers"])
    ):
        raise RuntimeError("release-policy.toml has an invalid binary contract")
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
            if (
                path.is_file()
                and not path.is_symlink()
                and path.resolve().is_relative_to(root.resolve())
            ):
                package_manifest = _manifest(path)
                if package_manifest is not None and isinstance(
                    package_manifest.get("package"), dict
                ):
                    paths.append(path)
    return sorted(
        set(paths), key=lambda item: item.relative_to(root).as_posix().casefold()
    )


def _cargo_publishable(
    package: dict[str, Any], workspace_package: dict[str, Any]
) -> bool:
    publish = package.get("publish", True)
    if publish == {"workspace": True}:
        publish = workspace_package.get("publish", True)
    return publish is not False and not (isinstance(publish, list) and not publish)


def _cargo_bump_findings(root: Path, repository: Repository) -> list[Finding]:
    candidates = []
    for relative in _POLICY["cargo"]["bump_scripts"]:
        path = _safe_repository_file(root, root, relative)
        if path is not None and path.is_file():
            candidates.append((relative, path))
    if not candidates:
        return [
            violation(
                repository,
                "cargo-release",
                None,
                "provide a version-controlled Cargo registry bump procedure: "
                + _POLICY["cargo"]["bump_command"],
            )
        ]
    if len(candidates) > 1:
        return [
            violation(
                repository,
                "cargo-release",
                None,
                "multiple Cargo release bump scripts make the version procedure ambiguous: "
                + ", ".join(path for path, _ in candidates),
            )
        ]
    relative, path = candidates[0]
    source = "\n".join(
        line for line in path.read_text(encoding="utf-8").splitlines() if not line.lstrip().startswith("#")
    ).casefold()
    missing = [
        requirement
        for requirement in _POLICY["cargo"]["bump_requirements"]
        if requirement.casefold() not in source
    ]
    if missing:
        return [
            violation(
                repository,
                "cargo-release",
                relative,
                "release bump procedure is missing " + ", ".join(missing),
            )
        ]
    return []


def _cargo_findings(root: Path, repository: Repository) -> list[Finding]:
    findings: list[Finding] = []
    workspace = _manifest(root / "Cargo.toml") or {}
    workspace_package = workspace.get("workspace", {})
    workspace_package = (
        workspace_package.get("package", {})
        if isinstance(workspace_package, dict)
        else {}
    )
    workspace_package = workspace_package if isinstance(workspace_package, dict) else {}
    publishable_found = False
    for path in _cargo_paths(root):
        relative = path.relative_to(root).as_posix()
        manifest = _manifest(path) or {}
        package = manifest.get("package", {})
        if not isinstance(package, dict):
            continue
        if not _cargo_publishable(package, workspace_package):
            continue
        publishable_found = True
        required_fields = set(_POLICY["cargo"]["required_fields"])
        if "name" in required_fields and not isinstance(package.get("name"), str):
            findings.append(
                violation(
                    repository,
                    "cargo-release",
                    relative,
                    "declare the published package name",
                )
            )
        inherited_version = package.get("version") == {"workspace": True}
        version = package.get("version")
        version_source = (
            "workspace.package.version" if inherited_version else "package.version"
        )
        version_fields = set(_POLICY["cargo"]["version_fields"])
        if version_source not in version_fields:
            findings.append(
                violation(
                    repository,
                    "cargo-release",
                    relative,
                    f"{version_source} is not an approved version source",
                )
            )
        elif inherited_version and "version" not in workspace_package:
            findings.append(
                violation(
                    repository,
                    "cargo-release",
                    relative,
                    "version.workspace = true has no workspace version",
                )
            )
        elif (
            not inherited_version
            and "version" in required_fields
            and (not isinstance(version, str) or not version)
        ):
            findings.append(
                violation(
                    repository,
                    "cargo-release",
                    relative,
                    "set a package version or explicit workspace inheritance",
                )
            )
        inherited_license = package.get("license") == {"workspace": True}
        license_value = package.get("license")
        if (
            inherited_license
            and "license" in required_fields
            and not (
                "license" in workspace_package
                or "license-file" in workspace_package
            )
        ):
            findings.append(
                violation(
                    repository,
                    "cargo-release",
                    relative,
                    "license.workspace = true has no workspace license",
                )
            )
        elif not inherited_license and "license" in required_fields:
            package_license = isinstance(license_value, str) and bool(license_value)
            package_license_file = (
                isinstance(package.get("license-file"), str)
                and bool(package["license-file"])
            )
            if not package_license and not package_license_file:
                findings.append(
                    violation(
                        repository,
                        "cargo-release",
                        relative,
                        "declare the crate license explicitly",
                    )
                )
        inherited_repository = package.get("repository") == {"workspace": True}
        if (
            inherited_repository
            and "repository" in required_fields
            and "repository" not in workspace_package
        ):
            findings.append(
                violation(
                    repository,
                    "cargo-release",
                    relative,
                    "repository.workspace = true has no workspace repository",
                )
            )
        elif "repository" in required_fields:
            repository_value = (
                workspace_package.get("repository")
                if inherited_repository
                else package.get("repository")
            )
            if not isinstance(repository_value, str) or not repository_value:
                findings.append(
                    violation(
                        repository,
                        "cargo-release",
                        relative,
                        "repository metadata is missing",
                        fixable=not inherited_repository,
                    )
                )
            elif not expected_origin(repository.id, repository_value):
                findings.append(
                    violation(
                        repository,
                        "cargo-release",
                        relative,
                        f"repository metadata points outside {repository.id}; "
                        "review the published package origin",
                    )
                )
        if "readme" in required_fields:
            readme = package.get("readme")
            if readme == {"workspace": True}:
                readme = workspace_package.get("readme")
                readme_path = (
                    _safe_repository_file(root, root, readme)
                    if isinstance(readme, str)
                    else None
                )
            elif isinstance(readme, str):
                readme_path = _safe_repository_file(root, path.parent, readme)
            else:
                readme_path = _safe_repository_file(root, path.parent, "README.md")
            if readme_path is None or not readme_path.is_file():
                findings.append(
                    violation(
                        repository,
                        "cargo-release",
                        relative,
                        "declare an existing package README or add the Cargo "
                        "default README.md",
                    )
                )
    if publishable_found:
        findings.extend(_cargo_bump_findings(root, repository))
    return findings


def _safe_repository_file(root: Path, directory: Path, value: str) -> Path | None:
    relative = Path(value)
    if relative.is_absolute():
        return None
    candidate = directory / relative
    return candidate if safe_relative(root, candidate) is not None else None


def _toml_set_value(source: str, section: str, key: str, value: str) -> str:
    header = f"[{section}]"
    lines = source.splitlines(keepends=True)
    section_start = next(
        (index for index, line in enumerate(lines) if line.strip() == header), None
    )
    assignment = f'{key} = "{value}"\n'
    if section_start is None:
        return f"{source.rstrip()}\n\n{header}\n{assignment}"
    section_end = next(
        (
            index
            for index in range(section_start + 1, len(lines))
            if lines[index].lstrip().startswith("[")
        ),
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
    workspace = _manifest(root / "Cargo.toml") or {}
    workspace = workspace.get("workspace", {})
    workspace_package = workspace.get("package", {}) if isinstance(workspace, dict) else {}
    workspace_package = workspace_package if isinstance(workspace_package, dict) else {}
    for path in _cargo_paths(root):
        manifest = _manifest(path) or {}
        package = manifest.get("package", {})
        if not isinstance(package, dict):
            continue
        if not _cargo_publishable(package, workspace_package):
            continue
        if "repository" in package:
            continue
        relative = path.relative_to(root).as_posix()
        updated = _toml_set_value(
            path.read_text(encoding="utf-8"), "package", "repository", github
        )
        if updated != path.read_text(encoding="utf-8"):
            changes[relative] = updated
    return changes


def _python_package(root: Path) -> tuple[Path | None, dict[str, Any] | None]:
    path = root / "pyproject.toml"
    return (
        (path, _manifest(path))
        if path.is_file() and not path.is_symlink()
        else (None, None)
    )


def _python_findings(root: Path, repository: Repository) -> list[Finding]:
    path, document = _python_package(root)
    if path is None or document is None:
        return [
            violation(
                repository,
                "python-release",
                "pyproject.toml",
                "PyPI tag requires readable project metadata",
            )
        ]
    raw_project = document.get("project", {})
    project = raw_project if isinstance(raw_project, dict) else {}
    build = document.get("build-system", {})
    findings: list[Finding] = []
    dynamic = project.get("dynamic", []) if isinstance(project, dict) else []
    if not isinstance(project.get("name"), str) or not project["name"]:
        findings.append(
            violation(
                repository,
                "python-release",
                "pyproject.toml",
                "declare the distribution name in project.name",
            )
        )
    if not isinstance(project.get("version"), str) or not project["version"]:
        if isinstance(dynamic, list) and "version" in dynamic:
            findings.append(
                violation(
                    repository,
                    "python-release",
                    "pyproject.toml",
                    "version is dynamic; document its authoritative source "
                    "and bump procedure",
                )
            )
        else:
            findings.append(
                violation(
                    repository,
                    "python-release",
                    "pyproject.toml",
                    "set project.version or explicitly declare dynamic versioning",
                )
            )
    expected_backend = _POLICY["pypi"].get("build_backend")
    if not isinstance(build, dict) or build.get("build-backend") != expected_backend:
        findings.append(
            violation(
                repository,
                "python-release",
                "pyproject.toml",
                    f"canonical PyPI policy uses {expected_backend}; "
                    "backend migration needs review",
            )
        )
    readme = _safe_repository_file(root, root, "README.md")
    if "readme" not in project and readme is not None and readme.is_file():
        findings.append(
            violation(
                repository,
                "python-release",
                "pyproject.toml",
                "declare the existing README as distribution metadata",
                fixable=True,
            )
        )
    return findings


def _python_fix(root: Path, repository: Repository) -> dict[str, str]:
    path, document = _python_package(root)
    readme = _safe_repository_file(root, root, "README.md")
    if path is None or document is None or readme is None or not readme.is_file():
        return {}
    project = document.get("project", {})
    if not isinstance(project, dict) or "readme" in project:
        return {}
    source = path.read_text(encoding="utf-8")
    lines = source.splitlines(keepends=True)
    section = next(
        (index for index, line in enumerate(lines) if line.strip() == "[project]"),
        None,
    )
    if section is None:
        return {}
    end = next(
        (
            index
            for index in range(section + 1, len(lines))
            if lines[index].lstrip().startswith("[")
        ),
        len(lines),
    )
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


def _npm_workspace_patterns(package: dict[str, Any]) -> tuple[str, ...] | None:
    raw_workspaces = package.get("workspaces", [])
    if isinstance(raw_workspaces, dict):
        patterns = raw_workspaces.get("packages")
    else:
        patterns = raw_workspaces
    if not isinstance(patterns, list):
        return None
    normalized: list[str] = []
    for pattern in patterns:
        if not isinstance(pattern, str):
            return None
        normalized.append(pattern.removeprefix("./"))
    if any(
        not pattern.removeprefix("!")
        or Path(pattern.removeprefix("!")).is_absolute()
        or ".." in Path(pattern.removeprefix("!")).parts
        for pattern in normalized
    ):
        return None
    return tuple(normalized)


def _npm_workspace_packages(
    root: Path,
    patterns: tuple[str, ...],
) -> tuple[list[tuple[Path, dict[str, Any]]], list[tuple[str, str]]]:
    packages: dict[Path, dict[str, Any]] = {}
    problems = []
    root_path = root.resolve()
    includes = tuple(pattern for pattern in patterns if not pattern.startswith("!"))
    excludes = tuple(pattern[1:] for pattern in patterns if pattern.startswith("!"))
    for pattern in includes:
        for name in glob.glob(str(root / pattern / "package.json"), recursive=True):
            path = Path(name)
            relative = path.relative_to(root).as_posix()
            if any(
                fnmatch.fnmatchcase(path.parent.relative_to(root).as_posix(), exclude)
                for exclude in excludes
            ):
                continue
            try:
                if path.is_symlink():
                    problems.append((relative, "workspace package manifest is a symlink"))
                    continue
                if not path.is_file() or not path.resolve().is_relative_to(root_path):
                    continue
            except OSError:
                problems.append((relative, "cannot resolve workspace package manifest"))
                continue
            document = _json_load(path)
            if document is None:
                problems.append((relative, "workspace package manifest is invalid JSON"))
            elif document.get("private") is not True:
                packages[path] = document
    ordered = sorted(packages.items(), key=lambda item: item[0].relative_to(root).as_posix().casefold())
    return [(path, document) for path, document in ordered], problems


def _npm_findings(root: Path, repository: Repository) -> list[Finding]:
    package_path = root / "package.json"
    if package_path.is_symlink():
        return [
            violation(
                repository,
                "npm-release",
                "package.json",
                "package manifest is a symlink and cannot be repaired safely",
            )
        ]
    package = _json_load(package_path)
    if package is None:
        return [
            violation(
                repository,
                "npm-release",
                "package.json",
                "npm tag requires readable package metadata",
            )
        ]
    patterns = _npm_workspace_patterns(package)
    if patterns is None:
        return [
            violation(
                repository,
                "npm-release",
                "package.json",
                "workspaces must be an array of repository-relative package globs",
            )
        ]
    private_root = package.get("private") is True
    workspace_packages, workspace_problems = _npm_workspace_packages(root, patterns)
    findings = [
        violation(repository, "npm-release", path, message)
        for path, message in workspace_problems
    ]
    if findings:
        return findings
    if private_root and not workspace_packages:
        return []
    public_packages = ([] if private_root else [(package_path, package)]) + workspace_packages
    for path, published in public_packages:
        if not isinstance(published.get("version"), str) or not published["version"]:
            findings.append(
                violation(
                    repository,
                    "npm-release",
                    path.relative_to(root).as_posix(),
                    "declare one authoritative version for the public package",
                )
            )
    if findings:
        return findings
    version = package.get("version")
    if not private_root and not isinstance(version, str):
        return [
            violation(
                repository,
                "npm-release",
                "package.json",
                "declare one authoritative package version",
            )
        ]
    lock_names = _POLICY["npm"].get("lockfiles")
    if not isinstance(lock_names, list) or any(
        not isinstance(name, str) for name in lock_names
    ):
        raise RuntimeError(
            "release-policy.toml npm.lockfiles must be an array of strings"
        )
    present = [
        name
        for name in lock_names
        if (root / name).is_file() or (root / name).is_symlink()
    ]
    if len(present) > 1:
        findings.append(
            violation(
                repository,
                "npm-release",
                "package.json",
                "multiple package manager lockfiles are present: "
                + ", ".join(sorted(present)),
            )
        )
        return findings
    if not present:
        findings.append(
            violation(
                repository,
                "npm-release",
                None,
                "commit one lockfile for the package manager used to publish",
            )
        )
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
    if (
        isinstance(package_manager, str)
        and package_manager.split("@", 1)[0] != selected_manager
    ):
        findings.append(
            violation(
                repository,
                "npm-release",
                "package.json",
                f"packageManager selects {package_manager}, but {lock_name} "
                f"belongs to {selected_manager}",
            )
        )
    if lock_name in {"package-lock.json", "npm-shrinkwrap.json"}:
        lock = root / lock_name
        if lock.is_symlink():
            findings.append(
                violation(
                    repository,
                    "npm-release",
                    lock_name,
                    "lockfile is a symlink and cannot be repaired safely",
                )
            )
            return findings
        lock_data = _json_load(lock)
        packages = lock_data.get("packages") if isinstance(lock_data, dict) else None
        root_package = packages.get("") if isinstance(packages, dict) else None
        top_version = lock_data.get("version") if isinstance(lock_data, dict) else None
        root_version = (
            root_package.get("version") if isinstance(root_package, dict) else None
        )
        lockfile_version = (
            lock_data.get("lockfileVersion") if isinstance(lock_data, dict) else None
        )
        if workspace_packages and lockfile_version == 1:
            findings.append(
                violation(
                    repository,
                    "npm-release",
                    lock_name,
                    "npm lockfile v1 cannot verify workspace package versions; regenerate it with npm",
                )
            )
        if not isinstance(lock_data, dict) or not isinstance(lockfile_version, int):
            findings.append(
                violation(
                    repository,
                    "npm-release",
                    lock_name,
                    "lockfile package metadata is unreadable or incomplete",
                )
            )
        elif lockfile_version != 1 and not isinstance(root_package, dict):
            findings.append(
                violation(
                    repository,
                    "npm-release",
                    lock_name,
                    "lockfile root package metadata is missing; regenerate it with npm",
                )
            )
        elif (
            lockfile_version != 1
            and not private_root
            and not isinstance(root_version, str)
        ):
            findings.append(
                violation(
                    repository,
                    "npm-release",
                    lock_name,
                    "lockfile root version is missing; regenerate it with npm",
                )
            )
        elif not private_root and (
            not isinstance(top_version, str)
            or top_version != version
            or (isinstance(root_package, dict) and root_version != version)
        ):
            findings.append(
                violation(
                    repository,
                    "npm-release",
                    lock_name,
                    f"lockfile versions must match package.json ({version})",
                    fixable=True,
                )
            )
        if isinstance(lock_data, dict) and isinstance(packages, dict) and lockfile_version != 1:
            for workspace_path, workspace_package in workspace_packages:
                relative = workspace_path.parent.relative_to(root).as_posix()
                lock_package = packages.get(relative)
                if not isinstance(lock_package, dict):
                    findings.append(
                        violation(
                            repository,
                            "npm-release",
                            lock_name,
                            f"lockfile has no entry for public workspace package {relative}",
                        )
                    )
                elif lock_package.get("version") != workspace_package.get("version"):
                    findings.append(
                        violation(
                            repository,
                            "npm-release",
                            lock_name,
                            f"lockfile version for {relative} must match its package.json",
                        )
                    )
    elif lock_name == "bun.lockb":
        findings.append(
            violation(
                repository,
                "npm-release",
                lock_name,
                "binary lockfile version metadata cannot be checked "
                "without the Bun CLI",
            )
        )
    return findings


def _npm_fix(root: Path, repository: Repository) -> dict[str, str]:
    package_path = root / "package.json"
    if package_path.is_symlink():
        return {}
    package = _json_load(package_path)
    lock_names = _POLICY["npm"].get("lockfiles", [])
    present = [
        name
        for name in lock_names
        if isinstance(name, str)
        and ((root / name).is_file() or (root / name).is_symlink())
    ]
    if len(present) != 1 or present[0] not in {
        "package-lock.json",
        "npm-shrinkwrap.json",
    }:
        return {}
    lock_path = root / present[0]
    lock = _json_load(lock_path) if not lock_path.is_symlink() else None
    version = package.get("version") if package is not None else None
    packages = lock.get("packages") if lock is not None else None
    root_package = packages.get("") if isinstance(packages, dict) else None
    lock_version = lock.get("lockfileVersion") if lock is not None else None
    if (
        not isinstance(version, str)
        or lock is None
        or not isinstance(lock.get("version"), str)
    ):
        return {}
    if lock_version != 1 and (
        not isinstance(packages, dict) or not isinstance(root_package, dict)
    ):
        return {}
    source = lock_path.read_text(encoding="utf-8")
    top_version = _json_string_value_span(source, "version")
    if top_version is None:
        return {}
    updated = (
        source[: top_version[0]] + json.dumps(version) + source[top_version[1] :]
    )
    package_object = re.search(r'"packages"\s*:\s*\{\s*""\s*:\s*\{', updated)
    if package_object is not None:
        package_start = package_object.end() - 1
        package_end = _json_object_end(updated, package_start)
        package_object_text = updated[package_start : package_end + 1]
        package_version = _json_string_value_span(package_object_text, "version")
        if package_version is None:
            return {}
        package_object_text = (
            package_object_text[: package_version[0]]
            + json.dumps(version)
            + package_object_text[package_version[1] :]
        )
        updated = (
            updated[:package_start]
            + package_object_text
            + updated[package_end + 1 :]
        )
    elif lock_version != 1:
        return {}
    if updated == source:
        return {}
    return {lock_path.name: updated}


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


def _release_rule(
    tag: str,
    rule_id: str,
    description: str,
    check,
    fix,
    paths: tuple[str, ...],
) -> Rule:
    return Rule(
        rule_id,
        description,
        TagPredicate(all_of=frozenset({tag})),
        check,
        fix,
        f"chore(repomod): adopt {rule_id} policy",
        paths,
    )


def release_rules() -> tuple[Rule, ...]:
    return (
        _release_rule(
            "crates-io",
            "cargo-release",
            "Check crate metadata and version bump procedure",
            _cargo_findings,
            _cargo_fix,
            ("**/Cargo.toml",),
        ),
        _release_rule(
            "npm",
            "npm-release",
            "Check npm package and lockfile version ownership",
            _npm_findings,
            _npm_fix,
            ("package-lock.json", "npm-shrinkwrap.json"),
        ),
        _release_rule(
            "pypi",
            "python-release",
            "Check PyPI build, version and release conventions",
            _python_findings,
            _python_fix,
            ("pyproject.toml",),
        ),
    )
