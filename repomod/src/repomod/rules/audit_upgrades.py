from __future__ import annotations

import contextlib
import os
import re
import shutil
import subprocess
import tempfile
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

from repomod.audit import (
    RegistrySources,
    UpgradeTask,
    _audit_cache_path,
    _cargo_manifests,
    _cargo_req_satisfies,
    _collect_locked,
    _crate_release,
    _is_outdated,
    _js_direct_dependencies,
    _js_project_roots,
    _js_spec_is_filesystem,
    _local_rust_package_names,
    _normalize_python_name,
    _normalize_rust_name,
    _parse_cargo_version,
    _pep508_is_filesystem,
    _pep508_name,
    _python_direct_specs,
    _read_cache,
    _resolve_latest,
    _rust_direct_dependencies,
    _upgrade_tasks,
    _version_key,
    _write_cache,
)
from repomod.catalog import parse_dependency_ignore
from repomod.io import read_toml
from repomod.rules.python_standard import load_approved_python_tool_versions
from repomod.types import Finding, Repository, Rule, TagPredicate, operational, violation

_RULE_ID = "audit-upgrades"

_COPY_IGNORED = frozenset(
    {
        ".git",
        ".work",
        ".venv",
        "node_modules",
        "target",
        "dist",
        "build",
        "__pycache__",
        ".pytest_cache",
        ".ruff_cache",
    }
)

_ALLOWED_PATHS = (
    "uv.lock",
    "pnpm-lock.yaml",
    "*/pnpm-lock.yaml",
    "package.json",
    "*/package.json",
    "Cargo.lock",
    "go.mod",
    "go.sum",
)


def _non_registry_names(root: Path) -> set[tuple[str, str]]:
    skipped: set[tuple[str, str]] = set()
    specs, document, _ = _python_direct_specs(root)
    for spec in specs:
        if _pep508_is_filesystem(spec):
            name = _pep508_name(spec)
            if name is not None:
                skipped.add(("pypi", name))
    if isinstance(document, dict):
        tool = document.get("tool")
        sources: Any = None
        if isinstance(tool, dict):
            uv_section = tool.get("uv")
            if isinstance(uv_section, dict):
                sources = uv_section.get("sources")
        if isinstance(sources, dict):
            for name, source in sources.items():
                if isinstance(name, str) and isinstance(source, dict) and ("path" in source or "git" in source):
                    skipped.add(("pypi", _normalize_python_name(name)))
    if specs:
        skipped.update(("pypi", name) for name in load_approved_python_tool_versions())
    direct, _ = _rust_direct_dependencies(root)
    for dependency in direct:
        if dependency.has_git or (dependency.has_path and not dependency.has_version):
            skipped.add(("crates", _normalize_rust_name(dependency.name)))
    skipped.update(("crates", name) for name in _local_rust_package_names(root))
    for project in _js_project_roots(root):
        for name, spec in _js_direct_dependencies(project).items():
            if _js_spec_is_filesystem(spec):
                skipped.add(("npm", name.casefold()))
    return skipped


def _configured_cargo_source(root: Path) -> str | None:
    path = root / ".cargo" / "config.toml"
    if not path.is_file():
        return None
    document, _ = read_toml(path)
    if not isinstance(document, dict):
        return None
    sources = document.get("source")
    if not isinstance(sources, dict):
        return None
    crates_io = sources.get("crates-io")
    replacement = crates_io.get("replace-with") if isinstance(crates_io, dict) else None
    target = sources.get(replacement) if isinstance(replacement, str) else None
    registry = target.get("registry") if isinstance(target, dict) else None
    return registry if isinstance(registry, str) else None


def _configured_npm_registry(root: Path) -> str | None:
    path = root / ".npmrc"
    if not path.is_file():
        return None
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError):
        return None
    for line in lines:
        stripped = line.strip()
        if not stripped or stripped[0] in "#;" or "=" not in stripped:
            continue
        key, _, value = stripped.partition("=")
        if key.strip() == "registry" and value.strip():
            return value.strip()
    return None


def _configured_python_index(root: Path) -> str | None:
    for path, nested in ((root / "uv.toml", False), (root / "pyproject.toml", True)):
        if not path.is_file():
            continue
        document, _ = read_toml(path)
        if not isinstance(document, dict):
            continue
        section: Any = document
        if nested:
            tool = document.get("tool")
            section = tool.get("uv") if isinstance(tool, dict) else None
        indexes = section.get("index") if isinstance(section, dict) else None
        if not isinstance(indexes, list):
            continue
        for index in indexes:
            if isinstance(index, dict) and index.get("default") is True:
                url = index.get("url")
                if isinstance(url, str):
                    return url
    return None


def _configured_sources(root: Path) -> RegistrySources | None:
    cargo = _configured_cargo_source(root)
    npm = _configured_npm_registry(root)
    python = _configured_python_index(root)
    if cargo is None and npm is None and python is None:
        return None
    return RegistrySources(cargo, npm, python)


def _rust_version_reqs(root: Path) -> dict[str, str | None]:
    reqs: dict[str, str | None] = {}
    direct, _ = _rust_direct_dependencies(root)
    for dependency in direct:
        key = _normalize_rust_name(dependency.name)
        if key not in reqs or (reqs[key] is None and dependency.version_req is not None):
            reqs[key] = dependency.version_req
    return reqs


_SPECIFIER_CLAUSE = re.compile(r"\s*(===|==|!=|~=|<=|>=|<|>)\s*([0-9A-Za-z.*+!_-]+)\s*")
_REQUIREMENT_SPECIFIERS = re.compile(
    r"^\s*[A-Za-z0-9_.-]+\s*(?:\[[^\]]*\])?\s*\(?\s*([^;()]*?)\s*\)?\s*(?:;.*)?$"
)


def _python_specifiers(root: Path) -> dict[str, list[str]]:
    specifiers: dict[str, list[str]] = {}
    specs, _, _ = _python_direct_specs(root)
    for spec in specs:
        name = _pep508_name(spec)
        match = _REQUIREMENT_SPECIFIERS.match(spec)
        if name is not None and match is not None and match.group(1):
            specifiers.setdefault(name, []).append(match.group(1))
    return specifiers


def _padded(left: tuple[int, ...], right: tuple[int, ...]) -> tuple[tuple[int, ...], tuple[int, ...]]:
    width = max(len(left), len(right))
    return left + (0,) * (width - len(left)), right + (0,) * (width - len(right))


def _specifier_admits(specifier: str, version: str) -> bool:
    candidate = _version_key(version)
    for clause in specifier.split(","):
        match = _SPECIFIER_CLAUSE.fullmatch(clause)
        if match is None:
            raise ValueError(f"unsupported Python version specifier {clause.strip()!r}")
        operator, target = match.groups()
        if target.endswith(".*"):
            prefix = _version_key(target.removesuffix(".*"))
            matches_prefix = candidate[: len(prefix)] == prefix
            if matches_prefix != (operator == "=="):
                return False
            continue
        left, right = _padded(candidate, _version_key(target))
        if operator in {"==", "==="}:
            admitted = left == right
        elif operator == "!=":
            admitted = left != right
        elif operator == "<":
            admitted = left < right
        elif operator == "<=":
            admitted = left <= right
        elif operator == ">":
            admitted = left > right
        elif operator == ">=":
            admitted = left >= right
        else:
            floor = _version_key(target)
            ceiling = floor[:-1] if len(floor) > 1 else floor
            admitted = left >= right and candidate[: len(ceiling)] == ceiling
        if not admitted:
            return False
    return True


def _lowest_rust_version(root: Path) -> str | None:
    declared: list[str] = []
    workspace_version: str | None = None
    members: list[dict[str, Any]] = []
    for manifest in _cargo_manifests(root):
        document, _ = read_toml(manifest)
        if not isinstance(document, dict):
            continue
        workspace = document.get("workspace")
        shared = workspace.get("package") if isinstance(workspace, dict) else None
        shared_version = shared.get("rust-version") if isinstance(shared, dict) else None
        if isinstance(shared_version, str):
            workspace_version = shared_version
        package = document.get("package")
        if isinstance(package, dict):
            members.append(package)
    for package in members:
        version = package.get("rust-version")
        if isinstance(version, str):
            declared.append(version)
        elif (
            isinstance(version, dict)
            and version.get("workspace") is True
            and workspace_version is not None
        ):
            declared.append(workspace_version)
    return min(declared, key=_parse_cargo_version, default=None)


def _exact_version(requirement: str) -> str | None:
    text = requirement.strip()
    if text.startswith("=") and not text.startswith("=="):
        return text[1:].strip()
    return None


def _msrv_conflict(release: dict[str, Any], lowest: str | None) -> str | None:
    required = release["rust_version"]
    if lowest is None or required is None:
        return None
    if _parse_cargo_version(required) > _parse_cargo_version(lowest):
        return f"it needs Rust {required} but the lowest workspace rust-version is {lowest}"
    return None


def _pin_conflict(
    release: dict[str, Any], rust_reqs: dict[str, str | None]
) -> str | None:
    for crate, requirement in release["requirements"]:
        direct = rust_reqs.get(_normalize_rust_name(crate))
        if direct is None:
            continue
        required_exact = _exact_version(requirement)
        pinned = _exact_version(direct)
        conflicts = (
            required_exact is not None and not _cargo_req_satisfies(direct, required_exact)
        ) or (pinned is not None and not _cargo_req_satisfies(requirement, pinned))
        if conflicts:
            return f"it requires {crate} {requirement} but the repository pins {crate} {direct}"
    return None


def _outdated_findings(
    root: Path,
    repository: Repository,
    cache: dict[str, Any],
    now: float,
    fetcher: Callable[[str, str], str | None] | None,
    sources: RegistrySources | None = None,
) -> list[Finding]:
    locked, _ = _collect_locked(root, repository)
    skipped = _non_registry_names(root)

    def key_for(ecosystem: str, name: str) -> tuple[str, str]:
        if ecosystem == "crates":
            return (ecosystem, _normalize_rust_name(name))
        if ecosystem == "pypi":
            return (ecosystem, _normalize_python_name(name))
        return (ecosystem, name.casefold())

    for ignored in repository.ignored_rules:
        parsed = parse_dependency_ignore(ignored)
        if parsed is not None:
            skipped.add(key_for(*parsed))

    direct = [item for item in locked if item.direct and key_for(item.ecosystem, item.name) not in skipped]
    if not direct:
        return []
    latest = _resolve_latest(
        direct,
        offline=False,
        refresh=False,
        cache=cache,
        now=now,
        fetcher=fetcher,
        sources=sources,
    )
    rust_reqs = _rust_version_reqs(root)
    python_specifiers = _python_specifiers(root)
    lowest_rust_version = _lowest_rust_version(root)
    findings: list[Finding] = []
    unresolved: set[str] = set()
    for item in sorted(direct, key=lambda entry: (entry.ecosystem, entry.name.casefold())):
        newest = latest.get((item.ecosystem, item.name))
        if newest is None:
            unresolved.add(f"{item.ecosystem}:{item.name}")
        elif _is_outdated(item.locked, newest):
            fixable = True
            blocker = ""
            if item.ecosystem == "crates":
                req = rust_reqs.get(_normalize_rust_name(item.name))
                if req is not None and not _cargo_req_satisfies(req, newest):
                    fixable = False
                else:
                    release = _crate_release(
                        item.name,
                        newest,
                        cache=cache,
                        now=now,
                        cargo_source=sources.cargo if sources is not None else None,
                    )
                    reason = None
                    if release is not None:
                        reason = _msrv_conflict(release, lowest_rust_version) or _pin_conflict(
                            release, rust_reqs
                        )
                    if reason is not None:
                        fixable = False
                        blocker = f"; manual because {reason}"
            if item.ecosystem == "pypi":
                fixable = all(
                    _specifier_admits(specifier, newest)
                    for specifier in python_specifiers.get(item.name, [])
                )
            findings.append(
                violation(
                    repository,
                    _RULE_ID,
                    None,
                    f"{item.ecosystem}:{item.name} {item.locked} is outdated; latest is {newest}{blocker}",
                    fixable=fixable,
                )
            )
    findings.extend(
        operational(repository, _RULE_ID, None, f"could not resolve latest version for {name}")
        for name in sorted(unresolved)
    )
    return findings


def _check(root: Path, repository: Repository) -> list[Finding]:
    cache = _read_cache(_audit_cache_path())
    if not isinstance(cache.get("latest"), dict):
        cache["latest"] = {}
    if not isinstance(cache.get("osv"), dict):
        cache["osv"] = {}
    sources = _configured_sources(root)
    findings = _outdated_findings(root, repository, cache, time.time(), None, sources)
    with contextlib.suppress(OSError):
        _write_cache(_audit_cache_path(), cache)
    return findings


def _copy_tree(root: Path, destination: Path) -> None:
    shutil.copytree(
        root,
        destination,
        symlinks=True,
        ignore=lambda directory, names: {name for name in names if name in _COPY_IGNORED},
    )


def _sibling_sources(root: Path) -> set[Path]:
    sources: set[Path] = set()
    direct, _ = _rust_direct_dependencies(root)
    for dependency in direct:
        if dependency.has_path and dependency.path_value is not None:
            parent = (root / dependency.manifest).parent
            sources.add(Path(os.path.normpath(parent / dependency.path_value)))
    _, document, _ = _python_direct_specs(root)
    if isinstance(document, dict):
        tool = document.get("tool")
        uv_section = tool.get("uv") if isinstance(tool, dict) else None
        uv_sources = uv_section.get("sources") if isinstance(uv_section, dict) else None
        if isinstance(uv_sources, dict):
            for source in uv_sources.values():
                if isinstance(source, dict) and isinstance(source.get("path"), str):
                    sources.add(Path(os.path.normpath(root / source["path"])))
    for project in _js_project_roots(root):
        for spec in _js_direct_dependencies(project).values():
            target: str | None = None
            if spec.startswith(("link:", "file:")):
                target = spec.split(":", 1)[1]
            elif spec.startswith("."):
                target = spec
            if target is not None:
                sources.add(Path(os.path.normpath(project / target)))
    return sources


def _copy_siblings(root: Path, work: Path) -> None:
    sandbox = work.parent
    temporary_root = sandbox.parent
    for source in sorted(_sibling_sources(root)):
        try:
            if not source.exists() or source == root or source.is_relative_to(root):
                continue
            destination = sandbox / os.path.relpath(source, root.parent)
            if not destination.resolve(strict=False).is_relative_to(
                temporary_root.resolve(strict=False)
            ):
                continue
            if destination.exists():
                continue
            destination.parent.mkdir(parents=True, exist_ok=True)
            if source.is_dir():
                _copy_tree(source.resolve(), destination)
            else:
                shutil.copy2(source.resolve(), destination)
        except OSError:
            continue


def _run_upgrade_task(task: UpgradeTask, cache_dir: Path) -> None:
    environment = os.environ.copy()
    if task.tool == "uv":
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


def _candidate_relatives(root: Path) -> list[str]:
    relatives = ["uv.lock", "Cargo.lock", "go.mod", "go.sum"]
    for project in _js_project_roots(root):
        try:
            relative = project.relative_to(root).as_posix()
        except ValueError:
            continue
        prefix = "" if relative == "." else f"{relative}/"
        relatives.extend((f"{prefix}pnpm-lock.yaml", f"{prefix}package.json"))
    return relatives


def _changed_plan(root: Path, refreshed: Path) -> dict[str, str]:
    plan: dict[str, str] = {}
    for relative in _candidate_relatives(root):
        candidate = refreshed / relative
        if candidate.is_symlink() or not candidate.is_file():
            continue
        original = root / relative
        if original.is_symlink():
            continue
        try:
            updated = candidate.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as error:
            raise RuntimeError(f"cannot read refreshed {relative}: {error}") from error
        try:
            previous = original.read_text(encoding="utf-8") if original.is_file() else None
        except (OSError, UnicodeError) as error:
            raise RuntimeError(f"cannot read {relative}: {error}") from error
        if updated != previous:
            plan[relative] = updated
    return plan


def _fix(root: Path, repository: Repository) -> dict[str, str]:
    tasks = _upgrade_tasks(root, repository)
    if not tasks:
        raise RuntimeError("nothing to upgrade")
    missing = sorted({task.tool for task in tasks if shutil.which(task.tool) is None})
    if missing:
        raise RuntimeError(
            f"{' and '.join(missing)} {'is' if len(missing) == 1 else 'are'} "
            "required to upgrade dependencies and was not found"
        )
    with tempfile.TemporaryDirectory(prefix="repomod-audit-upgrades-") as temporary:
        work = Path(temporary) / "sandbox" / root.name
        work.parent.mkdir(parents=True)
        _copy_tree(root, work)
        _copy_siblings(root, work)
        cache_dir = Path(temporary) / "cache"
        cache_dir.mkdir()
        for task in tasks:
            relative = task.directory.relative_to(root)
            rebased = UpgradeTask(work / relative.as_posix(), task.argv, task.tool)
            _run_upgrade_task(rebased, cache_dir)
        return _changed_plan(root, work)


def audit_upgrade_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            _RULE_ID,
            "Upgrade locked registry dependencies to their latest stable releases",
            TagPredicate(any_of=frozenset({"python", "typescript", "javascript", "rust", "go"})),
            _check,
            _fix,
            "chore(repomod): upgrade dependencies",
            _ALLOWED_PATHS,
        ),
    )
