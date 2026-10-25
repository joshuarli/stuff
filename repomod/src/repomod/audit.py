from __future__ import annotations

import contextlib
import json
import math
import os
import re
import shutil
import subprocess
import tempfile
import time
import urllib.parse
import urllib.request
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from repomod.catalog import Catalog, atomic_write, repository_path
from repomod.git import GitError, status
from repomod.github import UpstreamStatus
from repomod.io import cache_file, read_toml, safe_relative
from repomod.types import Finding, Repository
from repomod.types import operational as _operational
from repomod.types import violation as _violation

CACHE_TTL_SECONDS = 86400
_OSV_URL = "https://api.osv.dev/v1/querybatch"
_OSV_BATCH_SIZE = 100
_NETWORK_TIMEOUT = 30

_BANNED_RUST = ("clap", "tokio")
_BANNED_RUST_HTTP = frozenset(
    {
        "reqwest",
        "tiny-http",
        "hyper",
        "h2",
        "ureq",
        "attohttpc",
        "surf",
        "isahc",
        "curl",
        "awc",
    }
)
_BANNED_RUST_ALL = _BANNED_RUST + tuple(sorted(_BANNED_RUST_HTTP))
_IGNORED_DIRS = frozenset(
    {
        ".git",
        ".work",
        ".venv",
        "__pycache__",
        "node_modules",
        "target",
        "dist",
        "build",
        "vendor",
    }
)

_JS_FILESYSTEM_PREFIXES = (
    "file:",
    "link:",
    "portal:",
    "workspace:",
    "github:",
    "gitlab:",
    "bitbucket:",
    "git+",
    "git:",
    "ssh:",
    "http:",
    "https:",
)


def _finding(
    repository: Repository,
    rule: str,
    path: str | None,
    message: str,
    *,
    operational: bool = False,
) -> Finding:
    if operational:
        return _operational(repository, rule, path, message)
    return _violation(repository, rule, path, message)


def _normalize_python_name(value: str) -> str:
    return re.sub(r"[-_.]+", "-", value).casefold()


def _normalize_rust_name(value: str) -> str:
    return value.replace("_", "-").casefold()


def _is_banned_rust(name: str) -> bool:
    normalized = _normalize_rust_name(name)
    return any(
        normalized == banned or normalized.startswith(f"{banned}-")
        for banned in _BANNED_RUST_ALL
    )


def _banned_rust_reason(name: str) -> str:
    normalized = _normalize_rust_name(name)
    for banned in _BANNED_RUST_HTTP:
        if normalized == banned or normalized.startswith(f"{banned}-"):
            return "http"
    return "bloated"


def _version_key(version: str) -> tuple[int, ...]:
    cleaned = version.strip().removeprefix("v").removeprefix("V")
    cleaned = cleaned.split("+", 1)[0].split("-", 1)[0]
    parts: list[int] = []
    for piece in re.split(r"[.]", cleaned):
        match = re.match(r"([0-9]+)", piece)
        if match is None:
            break
        parts.append(int(match.group(1)))
    return tuple(parts) if parts else (0,)


def _is_outdated(locked: str, latest: str) -> bool:
    if locked.strip() == latest.strip():
        return False
    left = locked.strip().removeprefix("v").removeprefix("V")
    right = latest.strip().removeprefix("v").removeprefix("V")
    if left == right:
        return False
    return _version_key(locked) < _version_key(latest)


def _pep508_name(spec: str) -> str | None:
    text = spec.strip()
    if not text or text.startswith("#"):
        return None
    direct = re.split(r"\s*;\s*", text, maxsplit=1)[0]
    if "@" in direct and re.match(r"^[A-Za-z0-9_.-]+\s*[<>=!~]", direct) is None:
        name, _, _url = direct.partition("@")
        name = name.strip()
        return _normalize_python_name(name) if name else None
    match = re.match(r"^\s*([A-Za-z0-9_.-]+)", direct)
    return _normalize_python_name(match.group(1)) if match else None


def _pep508_is_filesystem(spec: str) -> bool:
    text = spec.strip()
    if "@" not in text:
        return False
    _, _, url = text.partition("@")
    url = url.strip().split(";")[0].strip().strip("\"'")
    lowered = url.casefold()
    prefixes = ("file:", "git+", "./", "../", "/", "ssh:", "http:", "https:")
    return lowered.startswith(prefixes) or "://" in lowered


def _js_spec_is_filesystem(spec: str) -> bool:
    value = spec.strip()
    if value.casefold().startswith(_JS_FILESYSTEM_PREFIXES):
        return True
    if value.startswith(("./", "../", "/", "~/", ".")):
        return True
    return re.match(r"^[A-Za-z]:[\\/]", value) is not None


def _read_json(path: Path) -> tuple[Any | None, str | None]:
    try:
        return json.loads(path.read_text(encoding="utf-8")), None
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        return None, str(error)


def _iter_python_files(root: Path, name: str) -> list[Path]:
    found: list[Path] = []
    for directory, child_names, filenames in os.walk(root, followlinks=False):
        current = Path(directory)
        child_names[:] = sorted(
            child
            for child in child_names
            if child not in _IGNORED_DIRS and not (current / child).is_symlink()
        )
        if name in filenames:
            candidate = current / name
            if not candidate.is_symlink():
                found.append(candidate)
    return sorted(found)


@dataclass(frozen=True, slots=True)
class RustDependency:
    name: str
    manifest: str
    has_path: bool
    has_git: bool
    has_version: bool = False
    path_value: str | None = None
    git_value: str | None = None


def _cargo_manifests(root: Path) -> list[Path]:
    patterns = (
        "Cargo.toml",
        "*/Cargo.toml",
        "crates/*/Cargo.toml",
        "packages/*/Cargo.toml",
    )
    found: set[Path] = set()
    for pattern in patterns:
        for match in root.glob(pattern):
            is_candidate = match.is_file() and not match.is_symlink()
            if is_candidate and safe_relative(root, match) is not None:
                found.add(match)
    for manifest in sorted(found):
        document, _ = read_toml(manifest)
        if not isinstance(document, dict):
            continue
        workspace = document.get("workspace")
        if not isinstance(workspace, dict):
            continue
        members = workspace.get("members", [])
        if not isinstance(members, list):
            continue
        for member in members:
            if not isinstance(member, str):
                continue
            for candidate in workspace_parent_candidates(manifest, member):
                is_candidate = candidate.is_file() and not candidate.is_symlink()
                is_new = candidate not in found
                is_safe = safe_relative(root, candidate) is not None
                if is_candidate and is_new and is_safe:
                    found.add(candidate)
    return sorted(found)


def workspace_parent_candidates(manifest: Path, member: str) -> list[Path]:
    return list(manifest.parent.glob(f"{member}/Cargo.toml"))


def _rust_manifest_dependencies(
    document: dict[str, Any], relative: str
) -> list[RustDependency]:
    tables: list[dict[str, Any]] = []
    for key in ("dependencies", "dev-dependencies", "build-dependencies"):
        value = document.get(key)
        if isinstance(value, dict):
            tables.append(value)
    workspace = document.get("workspace")
    if isinstance(workspace, dict):
        value = workspace.get("dependencies")
        if isinstance(value, dict):
            tables.append(value)
    targets = document.get("target")
    if isinstance(targets, dict):
        for target in targets.values():
            if not isinstance(target, dict):
                continue
            for key in ("dependencies", "dev-dependencies", "build-dependencies"):
                value = target.get(key)
                if isinstance(value, dict):
                    tables.append(value)
    dependencies: list[RustDependency] = []
    for table in tables:
        for name, spec in table.items():
            if name == "package":
                continue
            if isinstance(spec, dict):
                real = spec.get("package", name)
                real_name = real if isinstance(real, str) else name
                raw_path = spec.get("path")
                raw_git = spec.get("git")
                dependencies.append(
                    RustDependency(
                        name=real_name,
                        manifest=relative,
                        has_path="path" in spec,
                        has_git="git" in spec,
                        has_version="version" in spec,
                        path_value=raw_path if isinstance(raw_path, str) else None,
                        git_value=raw_git if isinstance(raw_git, str) else None,
                    )
                )
            elif isinstance(spec, str):
                dependencies.append(RustDependency(name, relative, False, False, True))
    return dependencies


def _rust_direct_dependencies(root: Path) -> tuple[list[RustDependency], list[str]]:
    manifests = _cargo_manifests(root)
    dependencies: list[RustDependency] = []
    problems: list[str] = []
    for manifest in manifests:
        relative = manifest.relative_to(root).as_posix()
        document, error = read_toml(manifest)
        if document is None:
            problems.append(f"{relative}: {error}")
            continue
        dependencies.extend(_rust_manifest_dependencies(document, relative))
    return dependencies, problems


def _locked_versions(lock: Path, normalize: Callable[[str], str]) -> tuple[dict[str, str], str | None]:
    if not lock.is_file() or lock.is_symlink():
        return {}, None
    document, error = read_toml(lock)
    if document is None:
        return {}, error
    locked: dict[str, str] = {}
    packages = document.get("package")
    if isinstance(packages, list):
        for package in packages:
            if not isinstance(package, dict):
                continue
            name = package.get("name")
            version = package.get("version")
            if isinstance(name, str) and isinstance(version, str):
                locked.setdefault(normalize(name), version)
    return locked, None


def _cargo_locked_versions(root: Path) -> tuple[dict[str, str], str | None]:
    return _locked_versions(root / "Cargo.lock", _normalize_rust_name)


def _uv_locked_versions(root: Path) -> tuple[dict[str, str], str | None]:
    return _locked_versions(root / "uv.lock", _normalize_python_name)


def _python_direct_specs(root: Path) -> tuple[list[str], dict[str, Any] | None, str | None]:
    manifest = root / "pyproject.toml"
    if not manifest.is_file() or manifest.is_symlink():
        return [], None, None
    document, error = read_toml(manifest)
    if document is None:
        return [], None, error
    specs: list[str] = []
    project = document.get("project")
    if isinstance(project, dict):
        dependencies = project.get("dependencies")
        if isinstance(dependencies, list):
            specs.extend(item for item in dependencies if isinstance(item, str))
        optional = project.get("optional-dependencies")
        if isinstance(optional, dict):
            for values in optional.values():
                if isinstance(values, list):
                    specs.extend(item for item in values if isinstance(item, str))
    groups = document.get("dependency-groups")
    if isinstance(groups, dict):
        for values in groups.values():
            if isinstance(values, list):
                specs.extend(item for item in values if isinstance(item, str))
    return specs, document, None


def _js_direct_dependencies(root: Path) -> dict[str, str]:
    manifest = root / "package.json"
    if not manifest.is_file() or manifest.is_symlink():
        return {}
    package, _ = _read_json(manifest)
    if not isinstance(package, dict):
        return {}
    sections = ("dependencies", "devDependencies", "peerDependencies", "optionalDependencies")
    return {
        name: spec
        for section in sections
        if isinstance(package.get(section), dict)
        for name, spec in package.get(section).items()
        if isinstance(name, str) and isinstance(spec, str)
    }


def _js_project_roots(root: Path) -> list[Path]:
    projects: list[Path] = []
    manifest = root / "package.json"
    if manifest.is_file() and not manifest.is_symlink():
        projects.append(root)
    try:
        children = sorted(root.iterdir(), key=lambda item: item.name)
    except OSError:
        return projects
    for child in children:
        if child.is_symlink() or not child.is_dir() or child.name in _IGNORED_DIRS:
            continue
        nested = child / "package.json"
        is_manifest = nested.is_file() and not nested.is_symlink()
        if is_manifest and safe_relative(root, nested) is not None:
            projects.append(child)
    return projects


def _read_text_if_regular(path: Path) -> str | None:
    if not path.is_file() or path.is_symlink():
        return None
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeError):
        return None


def _pnpm_inline_versions(source: str) -> dict[str, str]:
    locked: dict[str, str] = {}
    for line in source.splitlines():
        key_match = re.match(r"^  '?([^'\":\s][^':]*?)@([0-9][^':\s]*?)'?:\s*$", line)
        if key_match is None:
            continue
        name = key_match.group(1).strip().strip("\"'")
        version = re.split(r"[()]", key_match.group(2).strip(), maxsplit=1)[0]
        locked.setdefault(name, version)
    return locked


def _pnpm_all_versions(root: Path) -> dict[str, str]:
    source = _read_text_if_regular(root / "pnpm-lock.yaml")
    if source is None:
        return {}
    return _pnpm_inline_versions(source)


def _pnpm_locked_versions(root: Path) -> dict[str, str]:
    source = _read_text_if_regular(root / "pnpm-lock.yaml")
    if source is None:
        return {}
    locked: dict[str, str] = {}
    in_importers = False
    active = False
    current: str | None = None
    for line in source.splitlines():
        if line == "importers:":
            in_importers = True
            active = False
            current = None
            continue
        if line in {"packages:", "snapshots:", "settings:"}:
            in_importers = False
            active = False
            current = None
            continue
        if not in_importers:
            continue
        section = re.match(r"^    ([^ :]+):\s*$", line)
        if section is not None:
            active = section.group(1) in {
                "dependencies",
                "devDependencies",
                "optionalDependencies",
                "peerDependencies",
            }
            current = None
            continue
        package_match = re.match(r"^      (.+):\s*$", line)
        if package_match is not None and active:
            raw_name = package_match.group(1).strip().strip("\"'")
            current = raw_name if isinstance(raw_name, str) else None
            continue
        version_match = re.match(r"^        version:\s*(.+?)\s*$", line)
        if version_match is not None and current is not None and active:
            raw = version_match.group(1).strip().strip("\"'")
            assert isinstance(raw, str)
            base = re.split(r"[()]", raw, maxsplit=1)[0].strip()
            full = re.match(r"(\d+\.\d+\.\d+[^()\s]*)", base)
            simple = re.match(r"(\d+\.[0-9]+(?:\.[0-9]+)?)", base)
            if full is not None:
                locked.setdefault(current, full.group(1))
            elif simple is not None:
                locked.setdefault(current, simple.group(1))
    if locked:
        return locked
    return _pnpm_inline_versions(source)


def _go_requirements(root: Path) -> tuple[dict[str, str], dict[str, bool]]:
    source = _read_text_if_regular(root / "go.mod")
    if source is None:
        return {}, {}
    locked: dict[str, str] = {}
    direct: dict[str, bool] = {}
    in_require = False
    for line in source.splitlines():
        code = line.split("//", 1)[0].strip()
        if not code:
            continue
        if code.startswith("require ("):
            in_require = True
            continue
        if in_require and code == ")":
            in_require = False
            continue
        match = re.match(r"^require\s+(\S+)\s+(\S+)", code)
        if match is not None:
            locked[match.group(1)] = match.group(2)
            direct[match.group(1)] = "// indirect" not in line
            continue
        if in_require:
            parts = code.split()
            if len(parts) >= 2:
                locked[parts[0]] = parts[1]
                direct[parts[0]] = "// indirect" not in line
    return locked, direct


def _go_replace_filesystem(root: Path) -> list[str]:
    source = _read_text_if_regular(root / "go.mod")
    if source is None:
        return []
    offenders: list[str] = []
    for line in source.splitlines():
        code = line.split("//", 1)[0]
        if "=>" not in code:
            continue
        _, _, target = code.partition("=>")
        fields = target.strip().split()
        if not fields:
            continue
        candidate = fields[0].strip().strip("\"'")
        if candidate.startswith(("./", "../", "/")) or candidate in {".", ".."}:
            offenders.append(line.strip())
    return offenders


def _audit_cache_path() -> Path:
    return cache_file("audit.json")


def _read_cache(path: Path) -> dict[str, Any]:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (FileNotFoundError, OSError, UnicodeError, json.JSONDecodeError):
        return {}
    return data if isinstance(data, dict) else {}


def _write_cache(path: Path, data: dict[str, Any]) -> None:
    atomic_write(path, json.dumps(data, sort_keys=True, separators=(",", ":")) + "\n")


def _cache_fresh(fetched_at: Any, now: float) -> bool:
    if not isinstance(fetched_at, (int, float)) or isinstance(fetched_at, bool):
        return False
    if not math.isfinite(float(fetched_at)):
        return False
    return now - float(fetched_at) < CACHE_TTL_SECONDS


def _http_json(url: str, headers: dict[str, str] | None = None) -> Any | None:
    request = urllib.request.Request(
        url, headers={"User-Agent": "repomod", **(headers or {})}
    )
    try:
        with urllib.request.urlopen(request, timeout=_NETWORK_TIMEOUT) as response:
            return json.loads(response.read().decode("utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError):
        return None


def _non_empty_str(value: object) -> str | None:
    return value if isinstance(value, str) and value else None


def _registry_document(url: str, headers: dict[str, str] | None = None) -> dict[str, Any] | None:
    data = _http_json(url, headers)
    return data if isinstance(data, dict) else None


def _latest_pypi(name: str) -> str | None:
    encoded = urllib.parse.quote(name, safe="")
    document = _registry_document(f"https://pypi.org/pypi/{encoded}/json")
    if document is None:
        return None
    info = document.get("info")
    if not isinstance(info, dict):
        return None
    return _non_empty_str(info.get("version"))


def _latest_npm(name: str) -> str | None:
    encoded = urllib.parse.quote(name, safe="@")
    document = _registry_document(f"https://registry.npmjs.org/{encoded}/latest")
    if document is None:
        return None
    return _non_empty_str(document.get("version"))


def _latest_crates(name: str) -> str | None:
    encoded = urllib.parse.quote(name, safe="")
    document = _registry_document(
        f"https://crates.io/api/v1/crates/{encoded}",
        {"Accept": "application/json"},
    )
    if document is None:
        return None
    crate = document.get("crate")
    if not isinstance(crate, dict):
        return None
    for key in ("max_stable_version", "max_version"):
        version = _non_empty_str(crate.get(key))
        if version is not None:
            return version
    return None


def _escape_go_module(path: str) -> str:
    return "".join(
        f"!{character.lower()}" if character.isupper() else character for character in path
    )


def _latest_go(module: str) -> str | None:
    encoded = "/".join(
        urllib.parse.quote(part, safe="") for part in _escape_go_module(module).split("/")
    )
    document = _registry_document(f"https://proxy.golang.org/{encoded}/@latest")
    if document is None:
        return None
    return _non_empty_str(document.get("Version"))


_LATEST_FETCHERS: dict[str, Callable[[str], str | None]] = {
    "pypi": _latest_pypi,
    "npm": _latest_npm,
    "crates": _latest_crates,
    "go": _latest_go,
}

_OSV_ECOSYSTEMS = {
    "pypi": "PyPI",
    "npm": "npm",
    "crates": "crates.io",
    "go": "Go",
}


def _osv_query_batch(queries: list[dict[str, Any]]) -> list[list[dict[str, Any]]]:
    results: list[list[dict[str, Any]]] = []
    for offset in range(0, len(queries), _OSV_BATCH_SIZE):
        batch = queries[offset : offset + _OSV_BATCH_SIZE]
        expected = len(results) + len(batch)
        try:
            request = urllib.request.Request(
                _OSV_URL,
                data=json.dumps({"queries": batch}).encode("utf-8"),
                headers={"Content-Type": "application/json", "User-Agent": "repomod"},
            )
            with urllib.request.urlopen(request, timeout=_NETWORK_TIMEOUT) as response:
                payload = json.loads(response.read().decode("utf-8"))
        except (OSError, UnicodeError, json.JSONDecodeError):
            results.extend([[] for _ in batch])
            continue
        entries = payload.get("results") if isinstance(payload, dict) else None
        if not isinstance(entries, list):
            results.extend([[] for _ in batch])
            continue
        for entry in entries:
            vulns = entry.get("vulns") if isinstance(entry, dict) else None
            if isinstance(vulns, list):
                results.append([vuln for vuln in vulns if isinstance(vuln, dict)])
            else:
                results.append([])
        while len(results) < expected:
            results.append([])
    return results[: len(queries)]


@dataclass(slots=True)
class LockedDependency:
    ecosystem: str
    name: str
    locked: str
    direct: bool = True


@dataclass(slots=True)
class AuditOutcome:
    repository: str
    findings: list[Finding] = field(default_factory=list)
    fixed: list[str] = field(default_factory=list)
    fix_errors: list[str] = field(default_factory=list)


def _has_ecosystem(tags: set[str], root: Path, manifest: str, *names: str) -> bool:
    return bool(tags.intersection(names)) or (root / manifest).is_file()


def _collect_locked(
    root: Path, repository: Repository
) -> tuple[list[LockedDependency], list[Finding]]:
    tags = set(repository.tags)
    locked: list[LockedDependency] = []
    findings: list[Finding] = []
    if _has_ecosystem(tags, root, "pyproject.toml", "python"):
        specs, _document, error = _python_direct_specs(root)
        if error is not None:
            message = f"pyproject.toml is invalid TOML: {error}"
            findings.append(_finding(repository, "audit-lock", "pyproject.toml", message, operational=True))
        else:
            names = {name for spec in specs if (name := _pep508_name(spec)) is not None}
            versions, lock_error = _uv_locked_versions(root)
            if lock_error is not None:
                message = f"uv.lock is invalid TOML: {lock_error}"
                findings.append(_finding(repository, "audit-lock", "uv.lock", message, operational=True))
            locked.extend(
                LockedDependency("pypi", name, versions[name], True)
                for name in sorted(names)
                if name in versions
            )
    if _has_ecosystem(tags, root, "package.json", "typescript", "javascript"):
        seen_js: set[str] = set()
        for project in _js_project_roots(root):
            dependencies = _js_direct_dependencies(project)
            versions = _pnpm_locked_versions(project)
            for name in sorted(dependencies):
                if name.casefold() in seen_js:
                    continue
                seen_js.add(name.casefold())
                version = versions.get(name)
                if version is None:
                    spec = dependencies[name].strip().removeprefix("v")
                    if re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+[^ ]*", spec) is not None:
                        version = spec
                if version is not None:
                    locked.append(LockedDependency("npm", name, version, True))
    if _has_ecosystem(tags, root, "Cargo.toml", "rust"):
        direct, problems = _rust_direct_dependencies(root)
        for problem in problems:
            head, _, _tail = problem.partition(":")
            message = f"Cargo manifest is unreadable: {problem}"
            findings.append(_finding(repository, "audit-lock", head, message, operational=True))
        versions, lock_error = _cargo_locked_versions(root)
        if lock_error is not None:
            message = f"Cargo.lock is invalid TOML: {lock_error}"
            findings.append(_finding(repository, "audit-lock", "Cargo.lock", message, operational=True))
        seen: set[str] = set()
        for dependency in direct:
            if (
                dependency.has_path
                and not dependency.has_version
                and _is_self_dependency(root, dependency.manifest, dependency.path_value)
            ):
                continue
            key = _normalize_rust_name(dependency.name)
            if key in seen:
                continue
            seen.add(key)
            version = versions.get(key)
            if version is None:
                continue
            locked.append(LockedDependency("crates", dependency.name, version, True))
    if _has_ecosystem(tags, root, "go.mod", "go"):
        requirements, direct_map = _go_requirements(root)
        locked.extend(
            LockedDependency("go", name, requirements[name], direct_map.get(name, True))
            for name in sorted(requirements)
        )
    return locked, findings


def _collect_vuln_candidates(
    root: Path, repository: Repository, direct_locked: list[LockedDependency]
) -> list[LockedDependency]:
    candidates = list(direct_locked)
    seen = {(item.ecosystem, item.name.casefold(), item.locked) for item in candidates}
    tags = set(repository.tags)
    if _has_ecosystem(tags, root, "uv.lock", "python"):
        versions, _ = _uv_locked_versions(root)
        for name, version in versions.items():
            key = ("pypi", name.casefold(), version)
            if key not in seen:
                seen.add(key)
                candidates.append(LockedDependency("pypi", name, version, False))
    if _has_ecosystem(tags, root, "Cargo.lock", "rust"):
        versions, _ = _cargo_locked_versions(root)
        for name, version in versions.items():
            key = ("crates", name.casefold(), version)
            if key not in seen:
                seen.add(key)
                candidates.append(LockedDependency("crates", name, version, False))
    if _has_ecosystem(tags, root, "go.mod", "go"):
        requirements, _ = _go_requirements(root)
        for name, version in requirements.items():
            key = ("go", name.casefold(), version)
            if key not in seen:
                seen.add(key)
                candidates.append(LockedDependency("go", name, version, False))
    if _has_ecosystem(tags, root, "package.json", "typescript", "javascript"):
        for project in _js_project_roots(root):
            for name, version in _pnpm_all_versions(project).items():
                key = ("npm", name.casefold(), version)
                if key not in seen:
                    seen.add(key)
                    candidates.append(LockedDependency("npm", name, version, False))
    return candidates


def _source_kind(source: dict[Any, Any]) -> str:
    return "filesystem" if "path" in source else "git"


def _banned_rust_message(name: str, *, transitive: bool, version: str | None = None) -> str:
    reason = _banned_rust_reason(name)
    if reason == "http":
        detail = f"{name} {version}" if transitive and version is not None else name
        return (
            f"remove HTTP client {detail}; "
            "use h12tiny-*, laputa-h2-futures-lite, or laputa-hyper-futures-lite"
        )
    if transitive and version is not None:
        return f"remove bloated transitive {name} {version}"
    return f"remove bloated dependency {name}"


def _is_self_dependency(root: Path, manifest: str, path_value: str | None) -> bool:
    if path_value is None:
        return False
    root_manifest = root / "Cargo.toml"
    if not root_manifest.is_file() or root_manifest.is_symlink():
        return False
    document, _ = read_toml(root_manifest)
    if not isinstance(document, dict) or not isinstance(document.get("package"), dict):
        return False
    parent = (root / manifest).parent
    try:
        return Path(os.path.normpath(parent / path_value)) == Path(os.path.normpath(root))
    except (OSError, ValueError):
        return False


def _policy_findings(root: Path, repository: Repository) -> list[Finding]:
    findings: list[Finding] = []
    tags = set(repository.tags)
    if _has_ecosystem(tags, root, "pyproject.toml", "python"):
        lock = root / "uv.lock"
        if not lock.is_file() or lock.is_symlink():
            findings.append(_finding(repository, "audit-lock", "uv.lock", "uv.lock is required"))
        for candidate in _iter_python_files(root, "requirements.txt"):
            relative = safe_relative(root, candidate) or candidate.name
            findings.append(_finding(repository, "audit-banned", relative, "remove requirements.txt"))
        for candidate in sorted(root.glob("requirements*.txt")):
            is_file = candidate.is_file() and not candidate.is_symlink()
            is_safe = safe_relative(root, candidate) is not None
            if is_file and is_safe:
                relative = candidate.relative_to(root).as_posix()
                if all(finding.file != relative for finding in findings):
                    findings.append(_finding(repository, "audit-banned", relative, "remove requirements file"))
        specs, document, _ = _python_direct_specs(root)
        seen_specs: set[str] = set()
        for spec in specs:
            if not _pep508_is_filesystem(spec) or spec in seen_specs:
                continue
            seen_specs.add(spec)
            name = _pep508_name(spec) or spec.strip()
            mechanism = spec.partition("@")[2].strip().split(";")[0].strip().strip("\"'")
            message = (
                f"dependency '{name}' uses {mechanism}; depend on the registry version instead"
            )
            findings.append(_finding(repository, "audit-registry", "pyproject.toml", message))
        if isinstance(document, dict):
            tool = document.get("tool")
            sources: Any = None
            if isinstance(tool, dict):
                uv_section = tool.get("uv")
                if isinstance(uv_section, dict):
                    sources = uv_section.get("sources")
            if isinstance(sources, dict):
                for name, source in sorted(sources.items()):
                    if isinstance(source, dict) and ("path" in source or "git" in source):
                        kind = _source_kind(source)
                        target = source.get("path", source.get("git"))
                        detail = f' "{target}"' if isinstance(target, str) else ""
                        message = (
                            f"dependency '{name}' uses {kind} source{detail}; "
                            "depend on the registry version instead"
                        )
                        findings.append(_finding(repository, "audit-registry", "pyproject.toml", message))
    if _has_ecosystem(tags, root, "Cargo.toml", "rust"):
        direct, _ = _rust_direct_dependencies(root)
        for dependency in sorted(direct, key=lambda item: item.name.casefold()):
            if dependency.has_path and not dependency.has_version:
                if _is_self_dependency(root, dependency.manifest, dependency.path_value):
                    continue
                detail = f' "{dependency.path_value}"' if dependency.path_value is not None else ""
                message = (
                    f"dependency '{dependency.name}' uses path{detail}; "
                    "depend on the registry version instead"
                )
                findings.append(_finding(repository, "audit-registry", dependency.manifest, message))
            elif dependency.has_git:
                detail = f' "{dependency.git_value}"' if dependency.git_value is not None else ""
                message = (
                    f"dependency '{dependency.name}' uses git{detail}; "
                    "depend on the registry version instead"
                )
                findings.append(_finding(repository, "audit-registry", dependency.manifest, message))
        seen_banned: set[str] = set()
        for dependency in direct:
            if dependency.has_path and not dependency.has_version:
                continue
            normalized = _normalize_rust_name(dependency.name)
            if _is_banned_rust(dependency.name) and normalized not in seen_banned:
                seen_banned.add(normalized)
                message = _banned_rust_message(dependency.name, transitive=False)
                findings.append(_finding(repository, "audit-banned", dependency.manifest, message))
        versions, _ = _cargo_locked_versions(root)
        for normalized, version in sorted(versions.items()):
            if _is_banned_rust(normalized) and normalized not in seen_banned:
                seen_banned.add(normalized)
                message = _banned_rust_message(normalized, transitive=True, version=version)
                findings.append(_finding(repository, "audit-banned", "Cargo.lock", message))
    if _has_ecosystem(tags, root, "package.json", "typescript", "javascript"):
        for project in _js_project_roots(root):
            dependencies = _js_direct_dependencies(project)
            relative_manifest = (project / "package.json").relative_to(root).as_posix()
            for name in sorted(dependencies):
                if _js_spec_is_filesystem(dependencies[name]):
                    spec = dependencies[name].strip()
                    message = (
                        f"dependency '{name}' uses \"{spec}\"; depend on the registry version instead"
                    )
                    findings.append(_finding(repository, "audit-registry", relative_manifest, message))
            manifest, _ = _read_json(project / "package.json")
            manager = manifest.get("packageManager") if isinstance(manifest, dict) else None
            if isinstance(manager, str) and manager.split("@", 1)[0].strip().casefold() == "yarn":
                message = f"remove yarn packageManager {manager}; pnpm is the package manager"
                findings.append(_finding(repository, "audit-banned", relative_manifest, message))
            if not (project / "pnpm-lock.yaml").is_file():
                relative_lock = (project / "pnpm-lock.yaml").relative_to(root).as_posix()
                findings.append(_finding(repository, "audit-lock", relative_lock, "pnpm-lock.yaml is required"))
        yarn_seen: set[str] = set()
        for directory in [root, *_js_project_roots(root)]:
            for artifact in ("yarn.lock", ".yarnrc", ".yarnrc.yml", ".yarnrc.yaml"):
                candidate = directory / artifact
                if not candidate.is_file() or candidate.is_symlink():
                    continue
                relative = safe_relative(root, candidate)
                if relative is None or relative in yarn_seen:
                    continue
                yarn_seen.add(relative)
                message = f"remove {artifact}; pnpm is the only package manager"
                findings.append(_finding(repository, "audit-banned", relative, message))
            yarn_dir = directory / ".yarn"
            if yarn_dir.is_dir() and not yarn_dir.is_symlink():
                relative = safe_relative(root, yarn_dir)
                if relative is not None and relative not in yarn_seen:
                    yarn_seen.add(relative)
                    message = "remove .yarn/; pnpm is the only package manager"
                    findings.append(_finding(repository, "audit-banned", relative, message))
    if _has_ecosystem(tags, root, "go.mod", "go"):
        for line in _go_replace_filesystem(root):
            message = f"remove filesystem replace {line}"
            findings.append(_finding(repository, "audit-registry", "go.mod", message))
    return findings


def _resolve_latest(
    locked: list[LockedDependency],
    *,
    offline: bool,
    refresh: bool,
    cache: dict[str, Any],
    now: float,
    fetcher: Callable[[str, str], str | None] | None = None,
) -> dict[tuple[str, str], str | None]:
    latest_entries = cache.get("latest")
    if not isinstance(latest_entries, dict):
        latest_entries = {}
        cache["latest"] = latest_entries
    resolved: dict[tuple[str, str], str | None] = {}
    missing: list[LockedDependency] = []
    for item in locked:
        if fetcher is not None:
            resolved[(item.ecosystem, item.name)] = fetcher(item.ecosystem, item.name)
            continue
        key = f"{item.ecosystem}:{item.name.casefold()}:{item.locked}"
        entry = latest_entries.get(key)
        is_hit = (
            isinstance(entry, dict)
            and isinstance(entry.get("version"), str)
            and not refresh
            and _cache_fresh(entry.get("fetched_at"), now)
        )
        if is_hit:
            assert isinstance(entry, dict)
            version = entry.get("version")
            assert isinstance(version, str)
            resolved[(item.ecosystem, item.name)] = version
        elif offline:
            if isinstance(entry, dict) and isinstance(entry.get("version"), str):
                resolved[(item.ecosystem, item.name)] = entry.get("version")
            else:
                resolved[(item.ecosystem, item.name)] = None
        else:
            missing.append(item)
    if missing and fetcher is None and not offline:
        grouped: dict[str, list[LockedDependency]] = {}
        for item in missing:
            grouped.setdefault(item.ecosystem, []).append(item)
        workers = min(8, max(1, len(missing)))
        with ThreadPoolExecutor(max_workers=workers, thread_name_prefix="repomod-audit") as pool:
            futures = {}
            for ecosystem, items in grouped.items():
                impl = _LATEST_FETCHERS.get(ecosystem)
                if impl is None:
                    for item in items:
                        resolved[(item.ecosystem, item.name)] = None
                    continue
                for item in items:
                    futures[pool.submit(impl, item.name)] = item
            for future in as_completed(futures):
                item = futures[future]
                try:
                    version = future.result()
                except Exception:
                    version = None
                resolved[(item.ecosystem, item.name)] = version
                key = f"{item.ecosystem}:{item.name.casefold()}:{item.locked}"
                latest_entries[key] = {"version": version, "fetched_at": now}
    return resolved


def _resolve_vulns(
    candidates: list[LockedDependency],
    *,
    offline: bool,
    refresh: bool,
    cache: dict[str, Any],
    now: float,
    osv_fetcher: Callable[[list[dict[str, Any]]], list[list[dict[str, Any]]]] | None = None,
) -> dict[tuple[str, str, str], list[dict[str, Any]]]:
    osv_entries = cache.get("osv")
    if not isinstance(osv_entries, dict):
        osv_entries = {}
        cache["osv"] = osv_entries
    resolved: dict[tuple[str, str, str], list[dict[str, Any]]] = {}
    queries: list[dict[str, Any]] = []
    order: list[LockedDependency] = []
    for item in candidates:
        ecosystem = _OSV_ECOSYSTEMS.get(item.ecosystem)
        if ecosystem is None:
            continue
        key = f"{item.ecosystem}:{item.name.casefold()}:{item.locked}"
        entry = osv_entries.get(key)
        is_hit = (
            isinstance(entry, dict)
            and isinstance(entry.get("vulns"), list)
            and not refresh
            and _cache_fresh(entry.get("fetched_at"), now)
        )
        if is_hit:
            assert isinstance(entry, dict)
            raw = entry.get("vulns")
            assert isinstance(raw, list)
            resolved[(item.ecosystem, item.name, item.locked)] = [
                vuln for vuln in raw if isinstance(vuln, dict)
            ]
        elif offline:
            if isinstance(entry, dict) and isinstance(entry.get("vulns"), list):
                raw_offline = entry.get("vulns")
                assert isinstance(raw_offline, list)
                cached = [vuln for vuln in raw_offline if isinstance(vuln, dict)]
            else:
                cached = []
            resolved[(item.ecosystem, item.name, item.locked)] = cached
        else:
            queries.append(
                {"package": {"name": item.name, "ecosystem": ecosystem}, "version": item.locked}
            )
            order.append(item)
    if queries:
        batches = osv_fetcher(queries) if osv_fetcher is not None else _osv_query_batch(queries)
        for item, vulns in zip(order, batches, strict=True):
            resolved[(item.ecosystem, item.name, item.locked)] = vulns
            key = f"{item.ecosystem}:{item.name.casefold()}:{item.locked}"
            osv_entries[key] = {"vulns": vulns, "fetched_at": now}
    return resolved


def _audit_one(
    catalog: Catalog,
    repository: Repository,
    *,
    offline: bool,
    refresh: bool,
    cache: dict[str, Any],
    now: float,
    latest_fetcher: Callable[[str, str], str | None] | None = None,
    osv_fetcher: Callable[[list[dict[str, Any]]], list[list[dict[str, Any]]]] | None = None,
    upstream: UpstreamStatus | None = None,
) -> AuditOutcome:
    outcome = AuditOutcome(str(repository.id))
    try:
        root = repository_path(catalog, repository)
    except (OSError, ValueError) as error:
        outcome.findings.append(
            _finding(
                repository, "audit-lock", None, f"cannot resolve canonical checkout: {error}", operational=True
            )
        )
        return outcome
    if not root.is_dir():
        outcome.findings.append(
            _finding(repository, "audit-lock", None, "checkout is missing", operational=True)
        )
        return outcome
    ignored = repository.ignored_rules
    outcome.findings.extend(
        finding for finding in _policy_findings(root, repository) if finding.rule not in ignored
    )
    if (
        upstream is not None
        and upstream.parent is not None
        and "audit-upstream" not in ignored
    ):
        if upstream.error is not None:
            outcome.findings.append(
                _finding(
                    repository,
                    "audit-upstream",
                    None,
                    f"could not compare with {upstream.parent}: {upstream.error}",
                    operational=True,
                )
            )
        elif isinstance(upstream.behind_by, int) and upstream.behind_by > 0:
            branch = ""
            if upstream.parent_branch is not None and upstream.parent_branch != "":
                branch = f"@{upstream.parent_branch}"
            ahead = ""
            if isinstance(upstream.ahead_by, int) and upstream.ahead_by > 0:
                ahead = f", {upstream.ahead_by} ahead"
            outcome.findings.append(
                _finding(
                    repository,
                    "audit-upstream",
                    None,
                    f"fork is {upstream.behind_by} commits behind {upstream.parent}{branch}{ahead}",
                )
            )
    locked, lock_findings = _collect_locked(root, repository)
    outcome.findings.extend(finding for finding in lock_findings if finding.rule not in ignored)
    if locked:
        direct = [item for item in locked if item.direct]
        latest = _resolve_latest(
            direct, offline=offline, refresh=refresh, cache=cache, now=now, fetcher=latest_fetcher
        )
        for item in sorted(direct, key=lambda entry: (entry.ecosystem, entry.name.casefold())):
            newest = latest.get((item.ecosystem, item.name))
            if newest is None:
                continue
            if _is_outdated(item.locked, newest) and "audit-outdated" not in ignored:
                message = f"{item.ecosystem}:{item.name} {item.locked} is outdated; latest is {newest}"
                outcome.findings.append(_finding(repository, "audit-outdated", None, message))
        candidates = _collect_vuln_candidates(root, repository, locked)
        vulns = _resolve_vulns(
            candidates, offline=offline, refresh=refresh, cache=cache, now=now, osv_fetcher=osv_fetcher
        )
        for item in sorted(
            candidates, key=lambda entry: (entry.ecosystem, entry.name.casefold(), entry.locked)
        ):
            entries = vulns.get((item.ecosystem, item.name, item.locked), [])
            for vuln in entries:
                if "audit-vuln" in ignored:
                    continue
                vuln_id = vuln.get("id", "unknown")
                summary = vuln.get("summary", "")
                detail = f" {summary}" if isinstance(summary, str) and summary else ""
                message = f"{item.ecosystem}:{item.name} {item.locked} has {vuln_id}{detail}"
                outcome.findings.append(_finding(repository, "audit-vuln", None, message))
    outcome.findings.sort(key=lambda finding: (finding.rule, finding.file or "", finding.message))
    return outcome


def audit_repositories(
    catalog: Catalog,
    repositories: tuple[Repository, ...] | None = None,
    *,
    offline: bool = False,
    refresh: bool = False,
    cache_path: Path | None = None,
    now: float | None = None,
    jobs: int = 4,
    latest_fetcher: Callable[[str, str], str | None] | None = None,
    osv_fetcher: Callable[[list[dict[str, Any]]], list[list[dict[str, Any]]]] | None = None,
    upstream_statuses: dict[str, UpstreamStatus] | None = None,
) -> list[AuditOutcome]:
    current_time = time.time() if now is None else now
    path = cache_path or _audit_cache_path()
    cache = _read_cache(path)
    if not isinstance(cache.get("latest"), dict):
        cache["latest"] = {}
    if not isinstance(cache.get("osv"), dict):
        cache["osv"] = {}
    selected = list(repositories) if repositories is not None else list(catalog.repositories)
    if jobs < 1:
        raise ValueError("jobs must be at least one")
    workers = min(max(jobs, 1), 16, max(1, len(selected)))
    outcomes: list[AuditOutcome] = []
    with ThreadPoolExecutor(max_workers=workers, thread_name_prefix="repomod-audit") as pool:
        futures = {
            pool.submit(
                _audit_one,
                catalog,
                repository,
                offline=offline,
                refresh=refresh,
                cache=cache,
                now=current_time,
                latest_fetcher=latest_fetcher,
                osv_fetcher=osv_fetcher,
                upstream=upstream_statuses.get(str(repository.id)) if upstream_statuses is not None else None,
            ): repository
            for repository in selected
        }
        for future in as_completed(futures):
            repository = futures[future]
            try:
                outcomes.append(future.result())
            except Exception as error:
                outcomes.append(
                    AuditOutcome(
                        str(repository.id),
                        findings=[
                            _finding(
                                repository,
                                "audit-lock",
                                None,
                                f"audit could not complete: {error}",
                                operational=True,
                            )
                        ],
                    )
                )
    outcomes.sort(key=lambda item: item.repository.casefold())
    if latest_fetcher is None and osv_fetcher is None and not offline:
        with contextlib.suppress(OSError):
            _write_cache(path, cache)
    return outcomes


@dataclass(slots=True)
class UpgradeTask:
    directory: Path
    argv: list[str]
    tool: str


def _upgrade_tasks(root: Path, repository: Repository) -> list[UpgradeTask]:
    tags = set(repository.tags)
    tasks: list[UpgradeTask] = []
    if ("python" in tags or (root / "pyproject.toml").is_file()) and (
        root / "uv.lock"
    ).is_file():
        tasks.append(UpgradeTask(root, ["uv", "lock", "--upgrade"], "uv"))
    if _has_ecosystem(tags, root, "package.json", "typescript", "javascript"):
        tasks.extend(
            UpgradeTask(project, ["pnpm", "update", "--latest"], "pnpm")
            for project in _js_project_roots(root)
            if (project / "pnpm-lock.yaml").is_file()
        )
    if ("rust" in tags or (root / "Cargo.toml").is_file()) and (root / "Cargo.toml").is_file():
        tasks.append(UpgradeTask(root, ["cargo", "update"], "cargo"))
    if ("go" in tags or (root / "go.mod").is_file()) and (root / "go.mod").is_file():
        tasks.append(UpgradeTask(root, ["go", "get", "-u", "./..."], "go"))
        tasks.append(UpgradeTask(root, ["go", "mod", "tidy"], "go"))
    return tasks


def _run_task(task: UpgradeTask, cache_dir: Path | None = None) -> None:
    environment = os.environ.copy()
    if task.tool == "uv" and cache_dir is not None:
        environment["UV_CACHE_DIR"] = str(cache_dir)
    try:
        result = subprocess.run(
            task.argv,
            cwd=task.directory,
            env=environment,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=600,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise RuntimeError(f"{' '.join(task.argv)} failed: {error}") from error
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        suffix = detail or f"exited with status {result.returncode}"
        raise RuntimeError(f"{' '.join(task.argv)} failed: {suffix}")


def fix_repository(catalog: Catalog, repository: Repository) -> AuditOutcome:
    outcome = AuditOutcome(str(repository.id))
    try:
        root = repository_path(catalog, repository)
    except (OSError, ValueError) as error:
        outcome.fix_errors.append(str(error))
        return outcome
    try:
        current = status(root, repository.id)
    except (GitError, OSError) as error:
        outcome.fix_errors.append(str(error))
        return outcome
    if not current.present or not current.valid:
        outcome.fix_errors.append(current.error or "checkout is unavailable")
        return outcome
    if current.dirty:
        outcome.fix_errors.append("checkout is dirty")
        return outcome
    tasks = _upgrade_tasks(root, repository)
    if not tasks:
        outcome.fix_errors.append("nothing to upgrade")
        return outcome
    with tempfile.TemporaryDirectory(prefix="repomod-audit-") as temporary:
        cache_dir = Path(temporary)
        for task in tasks:
            if shutil.which(task.tool) is None:
                outcome.fix_errors.append(f"{task.tool} is required and was not found")
                continue
            try:
                _run_task(task, cache_dir)
            except RuntimeError as error:
                outcome.fix_errors.append(str(error))
            else:
                outcome.fixed.append(" ".join(task.argv))
    return outcome
