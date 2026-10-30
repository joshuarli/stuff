from __future__ import annotations

import json
import os
import re
import shlex
import tomllib
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Literal

from repomod.catalog import write_if_changed
from repomod.github import action_tags_many
from repomod.io import read_json_object, safe_relative
from repomod.types import Finding, Repository, Rule, TagPredicate, operational, violation

_IGNORED_DIRS = {
    ".git",
    ".work",
    "build",
    "dist",
    "fixtures",
    "generated",
    "node_modules",
    "test",
    "tests",
    "third_party",
    "vendor",
}
_LOCKFILES = {
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
    "deno.lock",
    "pnpm-lock.yaml",
}
_NPM_RUN = re.compile(r"(^|[;&|()]\s*)((?:[A-Za-z_][A-Za-z0-9_]*=\S+\s+)*)npm\s+run\b")
_FORBIDDEN_COMMANDS = {"npm", "npx", "bun", "bunx", "deno", "vp"}
_VERSION_TOKEN = re.compile(
    r"^(?:>=|>|=|\^|~)?v?(?P<major>[0-9]+)(?:\.(?:[0-9]+|x|\*))?(?:\.(?:[0-9]+|x|\*))?(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$",
    re.IGNORECASE,
)
_RELEASE_VERSION = re.compile(r"v?(?P<major>0|[1-9][0-9]*)\.(?P<minor>0|[1-9][0-9]*)\.(?P<patch>0|[1-9][0-9]*)\Z")
_RANGE_VERSION = re.compile(
    r"(?P<operator>>=|>|<=|<|=|\^|~)?v?(?P<major>0|[1-9][0-9]*)"
    r"(?:\.(?P<minor>0|[1-9][0-9]*|x|\*))?"
    r"(?:\.(?P<patch>0|[1-9][0-9]*|x|\*))?\Z",
    re.IGNORECASE,
)
_PNPM_PACKAGE_MANAGER_VERSION = re.compile(
    r"pnpm@(?P<version>v?(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*))"
    r"(?:\+[0-9A-Za-z./+=-]+)?\Z"
)
_NODE_RELEASE_INDEX = "https://nodejs.org/dist/index.json"
_APP_STACK = {
    "effect": 4,
}
_TOOLING_STACK = {"oxlint": 1, "oxlint-tsgolint": 1}
_FRONTEND_STACK = {
    "vite": 8,
    "solid-js": 2,
    "@solidjs/web": 2,
}


@dataclass(frozen=True, slots=True)
class TypeScriptToolPins:
    node: str
    pnpm: str


def typescript_tools_file() -> Path:
    return Path(__file__).resolve().parents[3] / "typescript-tools.toml"


def _release_version(value: str) -> tuple[int, int, int] | None:
    match = _RELEASE_VERSION.fullmatch(value)
    if match is None:
        return None
    return (
        int(match.group("major")),
        int(match.group("minor")),
        int(match.group("patch")),
    )


def load_typescript_tool_pins(path: Path | None = None) -> TypeScriptToolPins:
    target = typescript_tools_file() if path is None else path
    if target.is_symlink():
        raise ValueError(f"approved TypeScript tool pins are a symlink: {target}")
    try:
        parsed = tomllib.loads(target.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise ValueError(f"cannot read approved TypeScript tool pins at {target}: {error}") from error
    if set(parsed) != {"node", "pnpm"}:
        raise ValueError("typescript-tools.toml must contain exactly [node] and [pnpm] tables")
    versions = {}
    for name in ("node", "pnpm"):
        details = parsed.get(name)
        version = details.get("version") if isinstance(details, dict) else None
        if (
            not isinstance(details, dict)
            or set(details) != {"version"}
            or not isinstance(version, str)
            or _release_version(version) is None
        ):
            raise ValueError(f"typescript-tools.toml [{name}] must contain one exact stable version")
        versions[name] = version.removeprefix("v")
    return TypeScriptToolPins(versions["node"], versions["pnpm"])


def _latest_node_release() -> str:
    request = urllib.request.Request(_NODE_RELEASE_INDEX, headers={"User-Agent": "repomod"})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            releases = json.loads(response.read().decode("utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read official Node.js releases: {error}") from error
    if not isinstance(releases, list):
        raise ValueError("official Node.js release index must be a list")
    candidates: list[tuple[tuple[int, int, int], str]] = []
    for record in releases:
        if not isinstance(record, dict):
            continue
        version_text = record.get("version")
        if not isinstance(version_text, str):
            continue
        version = _release_version(version_text)
        if version is not None:
            candidates.append((version, version_text))
    if not candidates:
        raise ValueError("official Node.js release index has no stable versions")
    return max(candidates)[1].removeprefix("v")


def _latest_pnpm_release() -> str:
    tags = action_tags_many(("pnpm/pnpm",), "v").get("pnpm/pnpm", ())
    candidates = [
        (version, tag.removeprefix("v"))
        for tag in tags
        if (version := _release_version(tag)) is not None
    ]
    if not candidates:
        raise ValueError("GitHub did not report a stable pnpm release")
    return max(candidates)[1]


def _render_typescript_tool_pins(pins: TypeScriptToolPins) -> str:
    return f'[node]\nversion = "{pins.node}"\n\n[pnpm]\nversion = "{pins.pnpm}"\n'


def refresh_typescript_tool_pins(path: Path | None = None) -> bool:
    target = typescript_tools_file() if path is None else path
    current = load_typescript_tool_pins(target)
    node = _latest_node_release()
    pnpm = _latest_pnpm_release()
    for name, previous, latest in (
        ("Node.js", current.node, node),
        ("pnpm", current.pnpm, pnpm),
    ):
        previous_version = _release_version(previous)
        latest_version = _release_version(latest)
        if previous_version is None or latest_version is None:
            raise ValueError(f"invalid exact {name} release in refresh result")
        if latest_version < previous_version:
            raise ValueError(f"upstream reported an older {name} release; refusing to replace {previous} with {latest}")
    serialized = _render_typescript_tool_pins(TypeScriptToolPins(node, pnpm))
    return write_if_changed(target, serialized)


def _finding(
    repository: Repository,
    path: str | None,
    message: str,
    *,
    fixable: bool = False,
    kind: Literal["violation", "operational"] = "violation",
) -> Finding:
    if kind == "operational":
        return operational(repository, "typescript-stack", path, message)
    return violation(repository, "typescript-stack", path, message, fixable=fixable)


def _manifest_paths(root: Path) -> tuple[Path, ...]:
    candidates = [root / "package.json"]
    candidates.extend(
        child / "package.json"
        for child in sorted(root.iterdir(), key=lambda item: item.name.casefold())
        if not child.is_symlink() and child.is_dir() and child.name not in _IGNORED_DIRS
    )
    return tuple(
        path
        for path in candidates
        if path.is_symlink() or path.is_file()
    )


def _package_roots(root: Path, manifests: tuple[Path, ...]) -> tuple[Path, ...]:
    root_manifest = root / "package.json"
    if root_manifest in manifests:
        return (root,)
    roots = tuple(path.parent for path in manifests)
    return roots or (root,)


def _typescript_manifests(
    root: Path,
    packages: list[tuple[Path, dict[str, Any]]],
) -> list[tuple[Path, dict[str, Any]]]:
    selected = []
    for manifest, package in packages:
        if manifest.parent == root:
            selected.append((manifest, package))
            continue
        if "typescript" in _dependency_versions(package) or (
            not (manifest.parent / "tsconfig.json").is_symlink()
            and (manifest.parent / "tsconfig.json").is_file()
        ):
            selected.append((manifest, package))
            continue
        for directory in ("src", "app", "web", "client"):
            base = manifest.parent / directory
            if base.is_symlink() or not base.is_dir():
                continue
            if any(
                _safe_repository_path(root, path) and path.is_file()
                for pattern in ("*.ts", "*.tsx", "*/*.ts", "*/*.tsx")
                for path in base.glob(pattern)
            ):
                selected.append((manifest, package))
                break
    return selected


def _dependency_versions(package: dict[str, Any]) -> dict[str, str]:
    dependencies: dict[str, str] = {}
    for field in ("dependencies", "devDependencies", "peerDependencies", "optionalDependencies"):
        values = package.get(field)
        if isinstance(values, dict):
            dependencies.update(
                (name, version)
                for name, version in values.items()
                if isinstance(name, str) and isinstance(version, str)
            )
    return dependencies


def _range_has_minimum(specifier: str, minimum: int) -> bool:
    value = specifier.strip()
    if value.startswith("npm:") and "@" in value:
        value = value.rsplit("@", 1)[1]
    if value.startswith("workspace:"):
        value = value.removeprefix("workspace:")
    branches = value.split("||")
    if not branches:
        return False
    for branch in branches:
        lower_bounds: list[int] = []
        upper_bounds: list[int] = []
        for token in branch.strip().split():
            if token.startswith("<"):
                match = _VERSION_TOKEN.fullmatch(token.removeprefix("<").removeprefix("="))
                if match is not None:
                    upper_bounds.append(int(match.group("major")))
                continue
            if token.startswith("!"):
                continue
            match = _VERSION_TOKEN.fullmatch(token)
            if match is not None:
                lower_bounds.append(int(match.group("major")))
        if (
            not lower_bounds
            or min(lower_bounds) < minimum
            or (upper_bounds and min(upper_bounds) < minimum)
        ):
            return False
    return True


def _range_has_version_floor(specifier: str, minimum: tuple[int, int, int]) -> bool:
    for branch in specifier.strip().split("||"):
        lower_bounds = []
        upper_bounds = []
        for token in branch.strip().split():
            match = _RANGE_VERSION.fullmatch(token)
            if match is None:
                continue
            parts = (
                int(match.group("major")),
                int(match.group("minor")) if match.group("minor") not in {None, "x", "*"} else 0,
                int(match.group("patch")) if match.group("patch") not in {None, "x", "*"} else 0,
            )
            operator = match.group("operator")
            if operator in {"<", "<="}:
                upper_bounds.append((parts, operator))
            else:
                lower_bounds.append(
                    (parts[0], parts[1], parts[2] + 1) if operator == ">" else parts
                )
        if not lower_bounds or max(lower_bounds) < minimum:
            return False
        if any(parts < minimum or (parts == minimum and operator == "<") for parts, operator in upper_bounds):
            return False
    return True


def _lockfiles(path: Path) -> tuple[Path, ...]:
    files = tuple(path.iterdir())
    return tuple(
        sorted(
            (
                candidate
                for candidate in files
                if (candidate.is_symlink() or candidate.is_file())
                and (candidate.name in _LOCKFILES or candidate.name.startswith("bun.lock"))
            ),
            key=lambda candidate: candidate.name.casefold(),
        )
    )


def _root_package(path: Path) -> tuple[dict[str, Any] | None, str | None]:
    manifest = path / "package.json"
    if manifest.is_symlink():
        return None, "package manifest is a symlink and is not followed"
    return read_json_object(manifest)


def _check_package_authority(
    root: Path,
    package_root: Path,
    package: dict[str, Any],
    repository: Repository,
    pins: TypeScriptToolPins,
) -> list[Finding]:
    findings = []
    relative_manifest = (package_root / "package.json").relative_to(root).as_posix()
    manager = package.get("packageManager")
    manager_match = _PNPM_PACKAGE_MANAGER_VERSION.fullmatch(manager) if isinstance(manager, str) else None
    if manager_match is None:
        findings.append(
            _finding(repository, relative_manifest, "declare a pinned pnpm release in packageManager")
        )
    elif manager_match.group("version").removeprefix("v") != pins.pnpm:
        findings.append(
            _finding(
                repository,
                relative_manifest,
                f"pnpm packageManager pin {manager} is stale; use pnpm@{pins.pnpm}",
            )
        )
    locks = _lockfiles(package_root)
    pnpm_lock = package_root / "pnpm-lock.yaml"
    if pnpm_lock.is_symlink():
        findings.append(
            _finding(
                repository,
                pnpm_lock.relative_to(root).as_posix(),
                "pnpm-lock.yaml is a symlink and is not followed",
            )
        )
    elif not pnpm_lock.is_file():
        findings.append(_finding(repository, relative_manifest, "commit pnpm-lock.yaml as the package lockfile"))
    alternatives = [path for path in locks if path.name != "pnpm-lock.yaml"]
    findings.extend(
        _finding(
            repository,
            path.relative_to(root).as_posix(),
            f"remove {path.name}; pnpm-lock.yaml is the only package lock authority",
        )
        for path in alternatives
    )
    nested_locks = []
    for directory, child_names, filenames in os.walk(
        package_root,
        onerror=_raise_walk_error,
        followlinks=False,
    ):
        current = Path(directory)
        child_names[:] = sorted(
            name
            for name in child_names
            if name not in _IGNORED_DIRS and not (current / name).is_symlink()
        )
        if current == package_root:
            continue
        nested_locks.extend(
            current / name
            for name in filenames
            if name in _LOCKFILES or name.startswith("bun.lock")
        )
    findings.extend(
        _finding(
            repository,
            path.relative_to(root).as_posix(),
            f"remove nested lockfile {path.name}; keep one pnpm package lock authority",
        )
        for path in sorted(nested_locks, key=lambda item: item.as_posix().casefold())
    )
    return findings


def _raise_walk_error(error: OSError) -> None:
    raise error


def _manifest_data(
    root: Path,
    manifests: tuple[Path, ...],
    repository: Repository,
) -> tuple[list[tuple[Path, dict[str, Any]]], list[Finding]]:
    packages = []
    findings = []
    for path in manifests:
        relative = path.relative_to(root).as_posix()
        if path.is_symlink():
            findings.append(_finding(repository, relative, "package manifest is a symlink and is not followed"))
            continue
        package, error = read_json_object(path)
        if package is None:
            findings.append(
                _finding(repository, relative, f"package manifest is invalid JSON: {error}", kind="operational")
            )
        else:
            packages.append((path, package))
    return packages, findings


def _strict_setting(root: Path, path: Path, seen: frozenset[Path] = frozenset()) -> bool | None:
    if not _safe_repository_path(root, path):
        return None
    path = Path(os.path.normpath(path))
    if path in seen:
        return None
    config, _error = read_json_object(path, allow_jsonc=True)
    if config is None:
        return None
    compiler_options = config.get("compilerOptions")
    strict = compiler_options.get("strict") if isinstance(compiler_options, dict) else None
    if isinstance(strict, bool):
        return strict
    parent = config.get("extends")
    if not isinstance(parent, str) or not parent.startswith(("./", "../")):
        return False if parent is None else None
    parent_path = path.parent / parent
    if parent_path.suffix != ".json":
        parent_path = parent_path.with_suffix(".json")
    if not _safe_repository_path(root, parent_path):
        return None
    if not parent_path.exists():
        parent_directory = parent_path.with_suffix("")
        if _safe_repository_path(root, parent_directory) and parent_directory.is_dir():
            parent_path = parent_directory / "tsconfig.json"
    if not _safe_repository_path(root, parent_path) or not parent_path.is_file():
        return None
    return _strict_setting(root, parent_path, seen | {path})


def _safe_repository_path(root: Path, path: Path) -> bool:
    return safe_relative(root, path) is not None


def _typescript_config_findings(
    root: Path,
    package_root: Path,
    repository: Repository,
) -> list[Finding]:
    config_path = package_root / "tsconfig.json"
    relative = config_path.relative_to(root).as_posix()
    if config_path.is_symlink():
        return [_finding(repository, relative, "TypeScript configuration is a symlink and is not followed")]
    if not config_path.is_file():
        return [_finding(repository, relative, "commit a tsconfig.json with strict type checking enabled")]
    config, error = read_json_object(config_path, allow_jsonc=True)
    if config is None:
        return [
            _finding(
                repository,
                relative,
                f"TypeScript configuration is invalid JSON: {error}",
                kind="operational",
            )
        ]
    strict = _strict_setting(root, config_path)
    if strict is False:
        return [_finding(repository, relative, "set compilerOptions.strict to true")]
    if strict is None:
        parent = config.get("extends")
        if isinstance(parent, str):
            return [
                _finding(
                    repository,
                    relative,
                    "cannot verify strict mode inherited from an external tsconfig",
                    kind="operational",
                )
            ]
        return [_finding(repository, relative, "set compilerOptions.strict to true")]
    return []


def _exact_tool_pin_finding(
    repository: Repository,
    path: str,
    tool: str,
    actual: str,
    expected: str,
) -> Finding | None:
    parsed = _release_version(actual.strip())
    if parsed is None:
        return _finding(repository, path, f"{tool} pin must be an exact stable version")
    if actual.strip().removeprefix("v") != expected:
        return _finding(repository, path, f"{tool} pin {actual.strip()} is stale; use {expected}")
    return None


def _version_file_findings(
    root: Path,
    directory: Path,
    names: tuple[str, ...],
    tool: str,
    expected: str,
    repository: Repository,
) -> list[Finding]:
    findings = []
    for name in names:
        path = directory / name
        relative = path.relative_to(root).as_posix()
        if path.is_symlink():
            findings.append(_finding(repository, relative, f"{name} is a symlink and is not followed"))
        elif not path.exists():
            continue
        elif not path.is_file():
            findings.append(_finding(repository, relative, f"{name} must be a regular version pin file"))
        else:
            try:
                value = path.read_text(encoding="utf-8").strip()
            except (OSError, UnicodeError) as error:
                findings.append(
                    _finding(repository, relative, f"cannot read {name}: {error}", kind="operational")
                )
                continue
            finding = _exact_tool_pin_finding(repository, relative, tool, value, expected)
            if finding is not None:
                findings.append(finding)
    return findings


def _tool_versions_findings(
    root: Path,
    package_root: Path,
    pins: TypeScriptToolPins,
    repository: Repository,
) -> list[Finding]:
    path = package_root / ".tool-versions"
    relative = path.relative_to(root).as_posix()
    if path.is_symlink():
        return [_finding(repository, relative, ".tool-versions is a symlink and is not followed")]
    if not path.exists():
        return []
    if not path.is_file():
        return [_finding(repository, relative, ".tool-versions must be a regular version pin file")]
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError) as error:
        return [_finding(repository, relative, f"cannot read .tool-versions: {error}", kind="operational")]
    findings = []
    expected = {"node": pins.node, "nodejs": pins.node, "pnpm": pins.pnpm}
    for line in lines:
        fields = line.split("#", 1)[0].split()
        if len(fields) < 2 or fields[0] not in expected:
            continue
        findings.extend(
            finding
            for version in fields[1:]
            if (finding := _exact_tool_pin_finding(
                repository,
                relative,
                "Node.js" if fields[0] in {"node", "nodejs"} else "pnpm",
                version,
                expected[fields[0]],
            )) is not None
        )
    return findings


def _mise_tool_findings(
    root: Path,
    package_root: Path,
    pins: TypeScriptToolPins,
    repository: Repository,
) -> list[Finding]:
    path = package_root / "mise.toml"
    relative = path.relative_to(root).as_posix()
    if path.is_symlink():
        return [_finding(repository, relative, "mise.toml is a symlink and is not followed")]
    if not path.exists():
        return []
    try:
        document = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        return [_finding(repository, relative, f"mise.toml is invalid: {error}", kind="operational")]
    tools = document.get("tools")
    if not isinstance(tools, dict):
        return []
    findings = []
    for name, expected in (("node", pins.node), ("pnpm", pins.pnpm)):
        if name in tools:
            value = tools[name]
            finding = _exact_tool_pin_finding(
                repository,
                relative,
                "Node.js" if name == "node" else "pnpm",
                value if isinstance(value, str) else "",
                expected,
            )
            if finding is not None:
                findings.append(finding)
    return findings


def _runtime_pin_findings(
    root: Path,
    package_roots: tuple[Path, ...],
    pins: TypeScriptToolPins,
    repository: Repository,
) -> list[Finding]:
    minimum_node = _release_version(pins.node)
    if minimum_node is None:
        raise ValueError("approved Node.js pin must be an exact stable version")
    findings = []
    for package_root in package_roots:
        package, error = _root_package(package_root)
        if package is None:
            findings.append(
                _finding(
                    repository,
                    (package_root / "package.json").relative_to(root).as_posix(),
                    f"cannot read package authority: {error}",
                    kind="operational",
                )
            )
            continue
        manifest = (package_root / "package.json").relative_to(root).as_posix()
        engines = package.get("engines")
        node_range = engines.get("node") if isinstance(engines, dict) else None
        if not isinstance(node_range, str) or not _range_has_version_floor(node_range, minimum_node):
            findings.append(
                _finding(
                    repository,
                    manifest,
                    f"engines.node must require Node.js >= {pins.node}",
                )
            )
        volta = package.get("volta")
        if isinstance(volta, dict):
            for name, expected, label in (("node", pins.node, "Node.js"), ("pnpm", pins.pnpm, "pnpm")):
                if name in volta:
                    value = volta[name]
                    finding = _exact_tool_pin_finding(
                        repository,
                        manifest,
                        label,
                        value if isinstance(value, str) else "",
                        expected,
                    )
                    if finding is not None:
                        findings.append(finding)
        findings.extend(
            _version_file_findings(
                root,
                package_root,
                (".node-version", ".nvmrc"),
                "Node.js",
                pins.node,
                repository,
            )
        )
        findings.extend(_tool_versions_findings(root, package_root, pins, repository))
        findings.extend(_mise_tool_findings(root, package_root, pins, repository))
    return findings


def _package_typescript_findings(
    root: Path,
    packages: list[tuple[Path, dict[str, Any]]],
    package_roots: tuple[Path, ...],
    repository: Repository,
) -> list[Finding]:
    findings = []
    declarations: list[tuple[Path, str | None]]
    split_projects = (root / "package.json") not in (path for path, _package in packages)
    if split_projects:
        declarations = []
        for package_root in package_roots:
            manifest = package_root / "package.json"
            package = next((value for path, value in packages if path == manifest), None)
            specifier = _dependency_versions(package).get("typescript") if package is not None else None
            if specifier is None:
                findings.append(
                    _finding(
                        repository,
                        manifest.relative_to(root).as_posix(),
                        "declare TypeScript >=7 in package.json dependencies",
                    )
                )
            else:
                declarations.append((manifest, specifier))
    else:
        declarations = [
            (path, _dependency_versions(package).get("typescript"))
            for path, package in packages
            if "typescript" in _dependency_versions(package)
        ]
        if not declarations:
            findings.append(
                _finding(repository, "package.json", "declare TypeScript >=7 in package.json dependencies")
            )
    for manifest, specifier in declarations:
        relative = manifest.relative_to(root).as_posix()
        if specifier is not None and not _range_has_minimum(specifier, 7):
            findings.append(
                _finding(
                    repository,
                    relative,
                    f"TypeScript dependency {specifier} must have a lower bound of 7.0.0 or newer",
                )
            )
    for package_root in package_roots:
        findings.extend(_typescript_config_findings(root, package_root, repository))
    return findings


def _is_frontend(root: Path, package: dict[str, Any]) -> bool:
    dependencies = _dependency_versions(package)
    if any(
        name in dependencies
        for name in ("react", "react-dom", "solid-js", "@solidjs/web", "vite-plugin-solid")
    ):
        return True
    markers = (
        "src/main.tsx",
        "src/index.tsx",
        "app/entry-client.tsx",
        "web/main.tsx",
        "client/main.tsx",
    )
    if any(
        _safe_repository_path(root, root / marker) and (root / marker).is_file()
        for marker in markers
    ):
        return True
    for directory in ("src", "app", "web", "client"):
        base = root / directory
        if base.is_symlink() or not base.is_dir():
            continue
        for pattern in ("*.tsx", "*/*.tsx"):
            if any(_safe_repository_path(root, path) and path.is_file() for path in base.glob(pattern)):
                return True
    return False


def _app_stack_findings(
    root: Path,
    package_root: Path,
    package: dict[str, Any],
    repository: Repository,
) -> list[Finding]:
    dependencies = _dependency_versions(package)
    manifest = (package_root / "package.json").relative_to(root).as_posix()
    findings = []
    for name, minimum in _APP_STACK.items():
        version = dependencies.get(name)
        if version is None:
            findings.append(_finding(repository, manifest, f"declare {name} as a package dependency"))
        elif not _range_has_minimum(version, minimum):
            findings.append(
                _finding(repository, manifest, f"{name} dependency {version} must have a lower bound of {minimum}")
            )
    if _is_frontend(package_root, package):
        for name, minimum in _FRONTEND_STACK.items():
            version = dependencies.get(name)
            if version is None:
                findings.append(_finding(repository, manifest, f"frontend apps must use {name} >= {minimum}"))
            elif not _range_has_minimum(version, minimum):
                findings.append(
                    _finding(repository, manifest, f"{name} dependency {version} must have a lower bound of {minimum}")
                )
        if "@solidjs/vite-plugin" not in dependencies:
            findings.append(_finding(repository, manifest, "frontend apps must use @solidjs/vite-plugin"))
        findings.extend(
            _finding(repository, manifest, f"remove obsolete frontend dependency {name}")
            for name in ("@solidjs/start", "vite-plugin-solid", "next", "react", "react-dom")
            if name in dependencies
        )
    return findings


def _script_command_findings(
    root: Path,
    packages: list[tuple[Path, dict[str, Any]]],
    repository: Repository,
) -> list[Finding]:
    findings = []
    for manifest, package in _typescript_manifests(root, packages):
        scripts = package.get("scripts")
        if not isinstance(scripts, dict):
            continue
        relative = manifest.relative_to(root).as_posix()
        for name, command in scripts.items():
            if not isinstance(name, str) or not isinstance(command, str):
                continue
            for tokens in _shell_commands(command):
                command_index = 0
                while command_index < len(tokens) and (
                    tokens[command_index] in {"!", "if", "then", "else", "elif", "do", "while", "until"}
                    or re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*=.*", tokens[command_index]) is not None
                ):
                    command_index += 1
                if command_index >= len(tokens):
                    continue
                executable = tokens[command_index]
                if executable == "env":
                    command_index += 1
                    while command_index < len(tokens) and tokens[command_index].startswith("-"):
                        command_index += 1
                    while command_index < len(tokens) and re.fullmatch(
                        r"[A-Za-z_][A-Za-z0-9_]*=.*", tokens[command_index]
                    ):
                        command_index += 1
                    executable = tokens[command_index] if command_index < len(tokens) else ""
                if executable not in _FORBIDDEN_COMMANDS:
                    continue
                fixable = (
                    executable == "npm"
                    and command_index + 1 < len(tokens)
                    and tokens[command_index + 1] == "run"
                    and _NPM_RUN.search(command) is not None
                )
                message = (
                    f"script {name} invokes npm; invoke package scripts through pnpm"
                    if fixable
                    else f"script {name} invokes forbidden runtime or package manager {executable}"
                )
                findings.append(_finding(repository, relative, message, fixable=fixable))
    return findings


def _shell_commands(command: str) -> tuple[tuple[str, ...], ...]:
    lexer = shlex.shlex(command, posix=True, punctuation_chars=";&|()")
    lexer.whitespace_split = True
    tokens = tuple(str(token) for token in lexer)
    commands: list[tuple[str, ...]] = []
    current: list[str] = []
    for token in tokens:
        if token and all(character in ";&|()" for character in token):
            if current:
                commands.append(tuple(current))
                current = []
        else:
            current.append(token)
    if current:
        commands.append(tuple(current))
    return tuple(commands)


def _typescript_stack_findings(root: Path, repository: Repository) -> list[Finding]:
    manifests = _manifest_paths(root)
    if not manifests:
        return [_finding(repository, None, "TypeScript tag requires a pnpm-managed package.json")]
    packages, findings = _manifest_data(root, manifests, repository)
    if not packages:
        return findings
    try:
        pins = load_typescript_tool_pins()
    except ValueError as error:
        findings.append(
            _finding(repository, "typescript-tools.toml", str(error), kind="operational")
        )
        return findings
    package_roots = _package_roots(root, tuple(path for path, _package in packages))
    for package_root in package_roots:
        package, error = _root_package(package_root)
        if package is None:
            findings.append(
                _finding(
                    repository,
                    (package_root / "package.json").relative_to(root).as_posix(),
                    f"cannot read package authority: {error}",
                    kind="operational",
                )
            )
            continue
        findings.extend(_check_package_authority(root, package_root, package, repository, pins))
        for name in ("deno.json", "deno.jsonc", "deno.lock"):
            path = package_root / name
            if path.is_file() or path.is_symlink():
                findings.append(
                    _finding(
                        repository,
                        path.relative_to(root).as_posix(),
                        f"remove legacy Deno configuration {name}",
                    )
                )
    findings.extend(_package_typescript_findings(root, packages, package_roots, repository))
    findings.extend(_runtime_pin_findings(root, package_roots, pins, repository))
    for manifest, package in _typescript_manifests(root, packages):
        relative = manifest.relative_to(root).as_posix()
        dependencies = _dependency_versions(package)
        for name, minimum in _TOOLING_STACK.items():
            version = dependencies.get(name)
            if version is None:
                findings.append(_finding(repository, relative, f"declare {name} >= {minimum} as a package dependency"))
            elif not _range_has_minimum(version, minimum):
                findings.append(
                    _finding(repository, relative, f"{name} dependency {version} must have a lower bound of {minimum}")
                )
        if package.get("type") != "module":
            findings.append(_finding(repository, relative, "set package.json type to module for Node ESM"))
        scripts = package.get("scripts")
        scripts = scripts if isinstance(scripts, dict) else {}
        if not isinstance(scripts.get("lint"), str) or "oxlint" not in scripts["lint"]:
            findings.append(_finding(repository, relative, "define a lint script that runs Oxlint"))
        typecheck = scripts.get("typecheck")
        if not isinstance(typecheck, str) or "tsc" not in typecheck or "--noEmit" not in typecheck:
            findings.append(
                _finding(
                    repository,
                    relative,
                    "define typecheck with tsc --noEmit as the authoritative type check",
                )
            )
        check = scripts.get("check")
        if not isinstance(check, str) or any(required not in check for required in ("lint", "typecheck")):
            findings.append(_finding(repository, relative, "make the check script run lint and type checking"))
        if isinstance(scripts.get("test"), str):
            version = dependencies.get("vitest")
            if version is None:
                findings.append(_finding(repository, relative, "use Vitest 5 for the package test script"))
            elif not _range_has_minimum(version, 5):
                findings.append(
                    _finding(repository, relative, f"Vitest dependency {version} must have a lower bound of 5")
                )
            if "vitest" not in scripts["test"]:
                findings.append(_finding(repository, relative, "run the package test script with Vitest"))
            if not isinstance(check, str) or "test" not in check:
                findings.append(_finding(repository, relative, "make the check script run tests"))
    if "app" in repository.tags and "library" not in repository.tags:
        for package_root in package_roots:
            package, error = _root_package(package_root)
            if package is not None:
                findings.extend(_app_stack_findings(root, package_root, package, repository))
    findings.extend(_script_command_findings(root, packages, repository))
    return findings


def _parse_json_string(source: str, start: int) -> tuple[str | None, int]:
    try:
        value, end = json.JSONDecoder().raw_decode(source, start)
    except json.JSONDecodeError:
        return None, start
    return (value if isinstance(value, str) else None), end


def _package_script_edits(source: str) -> str:
    edits: list[tuple[int, int, str]] = []

    def skip_space(index: int) -> int:
        while index < len(source) and source[index].isspace():
            index += 1
        return index

    def parse_value(index: int, path: tuple[str, ...]) -> int:
        index = skip_space(index)
        if index >= len(source):
            return index
        if source[index] == '"':
            value, end = _parse_json_string(source, index)
            if value is not None and len(path) == 2 and path[0] == "scripts":
                replaced = _NPM_RUN.sub(
                    lambda match: f"{match.group(1)}{match.group(2)}pnpm run",
                    value,
                )
                if replaced != value:
                    edits.append((index, end, json.dumps(replaced, ensure_ascii=False)))
            return end
        if source[index] == "{":
            index = skip_space(index + 1)
            while index < len(source) and source[index] != "}":
                key, end = _parse_json_string(source, index)
                if key is None:
                    return len(source)
                index = skip_space(end)
                if index >= len(source) or source[index] != ":":
                    return len(source)
                index = parse_value(index + 1, (*path, key))
                index = skip_space(index)
                if index < len(source) and source[index] == ",":
                    index = skip_space(index + 1)
                else:
                    break
            return min(index + 1, len(source))
        if source[index] == "[":
            index = skip_space(index + 1)
            while index < len(source) and source[index] != "]":
                index = parse_value(index, path)
                index = skip_space(index)
                if index < len(source) and source[index] == ",":
                    index = skip_space(index + 1)
                else:
                    break
            return min(index + 1, len(source))
        while index < len(source) and source[index] not in ",]}":
            index += 1
        return index

    parse_value(0, ())
    for start, end, replacement in reversed(edits):
        source = source[:start] + replacement + source[end:]
    return source


def _fix_typescript_stack(root: Path, _repository: Repository) -> dict[str, str]:
    plan = {}
    for manifest in _manifest_paths(root):
        if manifest.is_symlink():
            continue
        source = manifest.read_text(encoding="utf-8")
        fixed = _package_script_edits(source)
        if fixed != source:
            plan[manifest.relative_to(root).as_posix()] = fixed
    return plan


def _validate_typescript_stack(
    root: Path,
    _repository: Repository,
    changed: tuple[str, ...],
) -> list[str]:
    errors = []
    for relative in changed:
        package, error = read_json_object(root / relative)
        if package is None:
            errors.append(f"{relative} is invalid JSON after the pnpm script repair: {error}")
            continue
        scripts = package.get("scripts")
        if isinstance(scripts, dict) and any(
            isinstance(command, str)
            and any(
                len(tokens) > 1 and tokens[0] == "npm" and tokens[1] == "run"
                for tokens in _shell_commands(command)
            )
            for command in scripts.values()
        ):
            errors.append(f"{relative} still invokes npm run")
    return errors


def typescript_standard_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            "typescript-stack",
            "Check TypeScript projects against the pnpm, Node, and TypeScript stack",
            TagPredicate(all_of=frozenset({"typescript"})),
            _typescript_stack_findings,
            _fix_typescript_stack,
            "chore(repomod): adopt TypeScript stack policy",
            ("package.json", "*/package.json"),
            _validate_typescript_stack,
        ),
    )
