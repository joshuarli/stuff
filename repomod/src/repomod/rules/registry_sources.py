from __future__ import annotations

import json
import os
import re
import tomllib
from dataclasses import dataclass
from functools import cache
from pathlib import Path, PurePosixPath
from typing import Any

from repomod.catalog import GIT_SOURCE_IGNORE_PREFIX
from repomod.io import read_toml
from repomod.rules.workspace import is_workspace_excluded, workspace_excluded
from repomod.types import Finding, Repository, Rule, TagPredicate, violation

RULE_ID = "registry-sources"
_CRATES_IO_SOURCE_IDS = frozenset(
    {
        "registry+https://github.com/rust-lang/crates.io-index",
        "sparse+https://index.crates.io/",
    }
)
_PRUNED_DIRECTORIES = frozenset({".git", ".venv", ".work", "node_modules", "target"})
_LISTED_NAMES = 5


@dataclass(frozen=True, slots=True)
class RegistryPolicy:
    enforce: bool
    cargo_source: str
    npm_registry: str
    python_index: str


def registry_policy_file() -> Path:
    return Path(__file__).resolve().parents[3] / "registries.toml"


def _table(document: dict[str, Any], name: str, key: str, scheme: str) -> str:
    table = document.get(name)
    if not isinstance(table, dict) or set(table) != {key}:
        raise ValueError(f"registries.toml [{name}] must contain only {key}")
    value = table[key]
    if not isinstance(value, str) or not value.startswith(scheme) or not value.endswith("/"):
        raise ValueError(f"registries.toml {name}.{key} must be a {scheme} URL ending in /")
    return value


def parse_registry_policy(source: str) -> RegistryPolicy:
    try:
        document = tomllib.loads(source)
    except tomllib.TOMLDecodeError as error:
        raise ValueError(f"registries.toml is invalid TOML: {error}") from error
    if set(document) != {"enforce", "cargo", "npm", "python"}:
        raise ValueError("registries.toml must contain only enforce, [cargo], [npm], and [python]")
    enforce = document["enforce"]
    if not isinstance(enforce, bool):
        raise ValueError("registries.toml enforce must be a boolean")
    return RegistryPolicy(
        enforce,
        _table(document, "cargo", "source", "sparse+https://"),
        _table(document, "npm", "registry", "https://"),
        _table(document, "python", "index", "https://"),
    )


def load_registry_policy(path: Path | None = None) -> RegistryPolicy:
    target = registry_policy_file() if path is None else path
    if target.is_symlink():
        raise ValueError(f"registry policy is a symlink: {target}")
    try:
        source = target.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        raise ValueError(f"cannot read registry policy at {target}: {error}") from error
    return parse_registry_policy(source)


@cache
def _default_policy() -> RegistryPolicy:
    return load_registry_policy()


def _relative(root: Path, path: Path) -> str:
    return path.relative_to(root).as_posix()


def _walk(root: Path) -> dict[str, list[Path]]:
    excluded = workspace_excluded(root)
    found: dict[str, list[Path]] = {}
    for directory, names, files in os.walk(root):
        names[:] = sorted(name for name in names if name not in _PRUNED_DIRECTORIES)
        base = Path(directory)
        relative = PurePosixPath(base.relative_to(root).as_posix())
        if relative.parts and is_workspace_excluded(excluded, relative):
            names[:] = []
            continue
        for name in files:
            found.setdefault(name, []).append(base / name)
    return found


def _chain(root: Path, directory: Path) -> list[Path]:
    chain: list[Path] = [directory]
    while chain[-1] != root:
        chain.append(chain[-1].parent)
    return chain


def _outside_repository(root: Path, base: Path, target: str) -> bool:
    if target.startswith("/") or re.match(r"[A-Za-z]:", target):
        return True
    resolved = os.path.normpath(base / target)
    return not (resolved == str(root) or resolved.startswith(str(root) + os.sep))


class _Report:
    def __init__(self, repository: Repository, root: Path) -> None:
        self.repository = repository
        self.root = root
        self.findings: dict[Finding, None] = {}

    def add(self, path: Path, message: str) -> None:
        finding = violation(self.repository, RULE_ID, _relative(self.root, path), message)
        self.findings[finding] = None

    def git_source(self, path: Path, ecosystem: str, name: str, source: str) -> None:
        if f"{GIT_SOURCE_IGNORE_PREFIX}{name}" in self.repository.ignored_rules:
            return
        self.add(
            path,
            f"{ecosystem} package {name} resolves from git source {source}; publish it to stuff "
            f"or add {GIT_SOURCE_IGNORE_PREFIX}{name} to the catalog ignore with an ignore_reason",
        )

    def unapproved(
        self, path: Path, ecosystem: str, source: str, names: list[str], approved: str, hint: str
    ) -> None:
        listed = ", ".join(sorted(names)[:_LISTED_NAMES])
        more = "" if len(names) <= _LISTED_NAMES else f" and {len(names) - _LISTED_NAMES} more"
        self.add(
            path,
            f"{len(names)} {ecosystem} package(s) resolve from {source} ({listed}{more}) instead "
            f"of {approved}; {hint}",
        )


def _cargo_config(report: _Report, policy: RegistryPolicy, directory: Path) -> None:
    reasons: list[str] = []
    for candidate in _chain(report.root, directory):
        path = candidate / ".cargo" / "config.toml"
        if not path.is_file():
            continue
        document, error = read_toml(path)
        if document is None:
            reasons.append(f"{_relative(report.root, path)} is unreadable: {error}")
            continue
        sources = document.get("source")
        sources = sources if isinstance(sources, dict) else {}
        crates_io = sources.get("crates-io")
        replacement = crates_io.get("replace-with") if isinstance(crates_io, dict) else None
        target = sources.get(replacement) if isinstance(replacement, str) else None
        if isinstance(target, dict) and target.get("registry") == policy.cargo_source:
            return
        reasons.append(
            f"{_relative(report.root, path)} does not replace crates-io with {policy.cargo_source}"
        )
    fix = (
        f'[source.crates-io] replace-with = "stuff" and [source.stuff] registry = '
        f'"{policy.cargo_source}"'
    )
    detail = "; ".join(reasons) if reasons else "no .cargo/config.toml found"
    report.add(directory / ".cargo" / "config.toml", f"{detail}; set {fix}")


def _cargo_lock(report: _Report, policy: RegistryPolicy, lock: Path) -> None:
    document, error = read_toml(lock)
    if document is None:
        raise ValueError(f"cannot read {_relative(report.root, lock)}: {error}")
    other: dict[str, list[str]] = {}
    for package in document.get("package", []):
        source = package.get("source")
        name = str(package.get("name"))
        if source is None or source in _CRATES_IO_SOURCE_IDS or source == policy.cargo_source:
            continue
        if str(source).startswith("git+"):
            report.git_source(lock, "cargo", name, str(source).split("#")[0])
        else:
            other.setdefault(str(source), []).append(name)
    for source, names in other.items():
        report.unapproved(
            lock,
            "cargo",
            source,
            names,
            "crates.io (replaced by stuff)",
            "remove the alternate registry or move the crates to stuff",
        )


def _npmrc_entries(path: Path) -> list[tuple[str, str]]:
    entries = []
    for line in path.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if not stripped or stripped[0] in "#;" or "=" not in stripped:
            continue
        key, _, value = stripped.partition("=")
        entries.append((key.strip(), value.strip()))
    return entries


def _npm_config(report: _Report, policy: RegistryPolicy, directory: Path) -> None:
    satisfied = False
    for candidate in _chain(report.root, directory):
        npmrc = candidate / ".npmrc"
        if npmrc.is_file():
            for key, value in _npmrc_entries(npmrc):
                if key == "registry" and value == policy.npm_registry:
                    satisfied = True
                elif key == "registry" or key.endswith(":registry"):
                    report.add(
                        npmrc,
                        f"{key}={value} is not the stuff registry; keep only "
                        f"registry={policy.npm_registry}",
                    )
        workspace = candidate / "pnpm-workspace.yaml"
        if workspace.is_file():
            for line in workspace.read_text(encoding="utf-8").splitlines():
                if re.match(r"(registry|registries)\s*:", line):
                    report.add(
                        workspace,
                        "pnpm-workspace.yaml overrides the registry; remove it and set "
                        f"registry={policy.npm_registry} in .npmrc",
                    )
    if not satisfied:
        report.add(
            directory / ".npmrc",
            f"no .npmrc sets the stuff registry; add registry={policy.npm_registry}",
        )


def _is_git_url(url: str) -> bool:
    return url.startswith(("git+", "git:", "github:")) or re.match(
        r"https://(codeload\.)?github\.com/", url
    ) is not None


def _package_name(key: str) -> str:
    name = key.strip().strip("'\"").lstrip("/").split("(")[0]
    at = name.rfind("@")
    return name[:at] if at > 0 else name


def _pnpm_lock(report: _Report, policy: RegistryPolicy, lock: Path) -> None:
    other: dict[str, list[str]] = {}
    section = ""
    importer = ""
    package = ""
    for line in lock.read_text(encoding="utf-8").splitlines():
        if line and not line.startswith(" "):
            section = line.rstrip(":")
            continue
        key = re.match(r"  (\S.*):\s*$", line)
        if key and section == "importers":
            importer = key.group(1).strip("'\"")
        elif key and section in {"packages", "snapshots"}:
            package = _package_name(key.group(1))
        for match in re.finditer(r"\b(?:version|specifier): (?:link|file):(\S+)", line):
            base = lock.parent / importer
            target = match.group(1).strip("'\"")
            if _outside_repository(report.root, base, target):
                report.add(
                    lock,
                    f"importer {importer or '.'} links {target} outside the repository; "
                    "depend on a published stuff package instead",
                )
        tarball = re.search(r"\btarball: ([^,}\s]+)", line)
        if tarball is None:
            if re.search(r"\btype: git\b|\brepo: ", line) and section == "packages":
                report.git_source(lock, "npm", package, "a git repository")
            continue
        url = tarball.group(1).strip("'\"")
        if url.startswith((policy.npm_registry, "file:")):
            continue
        if _is_git_url(url):
            report.git_source(lock, "npm", package, url)
        else:
            other.setdefault(url.split("/")[2] if "://" in url else url, []).append(package)
    for host, names in other.items():
        report.unapproved(
            lock,
            "npm",
            host,
            sorted(set(names)),
            policy.npm_registry,
            f"set registry={policy.npm_registry} in .npmrc and reinstall",
        )


def _package_lock(report: _Report, policy: RegistryPolicy, lock: Path) -> None:
    try:
        document = json.loads(lock.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise ValueError(f"cannot read {_relative(report.root, lock)}: {error}") from error
    packages = document.get("packages")
    if not isinstance(packages, dict):
        raise ValueError(f"{_relative(report.root, lock)} predates lockfileVersion 2")
    other: dict[str, list[str]] = {}
    for key, entry in packages.items():
        if not key or not isinstance(entry, dict):
            continue
        name = entry.get("name") or key.rsplit("node_modules/", 1)[-1]
        resolved = entry.get("resolved")
        if entry.get("link") is True:
            if isinstance(resolved, str) and _outside_repository(report.root, lock.parent, resolved):
                report.add(
                    lock,
                    f"{name} links {resolved} outside the repository; depend on a published stuff "
                    "package instead",
                )
            continue
        if not isinstance(resolved, str) or resolved.startswith((policy.npm_registry, "file:")):
            continue
        if _is_git_url(resolved):
            report.git_source(lock, "npm", name, resolved)
        else:
            host = resolved.split("/")[2] if "://" in resolved else resolved
            other.setdefault(host, []).append(name)
    for host, names in other.items():
        report.unapproved(
            lock,
            "npm",
            host,
            sorted(set(names)),
            policy.npm_registry,
            f"set registry={policy.npm_registry} in .npmrc and reinstall",
        )


def _uv_index_satisfied(indexes: object, policy: RegistryPolicy) -> bool:
    return isinstance(indexes, list) and any(
        isinstance(index, dict) and index.get("default") is True and index.get("url") == policy.python_index
        for index in indexes
    )


def _python_config(report: _Report, policy: RegistryPolicy, directory: Path) -> None:
    for candidate in _chain(report.root, directory):
        uv_toml = candidate / "uv.toml"
        pyproject = candidate / "pyproject.toml"
        if uv_toml.is_file():
            document, _ = read_toml(uv_toml)
            if document is not None and _uv_index_satisfied(document.get("index"), policy):
                return
        if pyproject.is_file():
            document, _ = read_toml(pyproject)
            tool = document.get("tool") if document is not None else None
            uv = tool.get("uv") if isinstance(tool, dict) else None
            if isinstance(uv, dict) and _uv_index_satisfied(uv.get("index"), policy):
                return
    target = "uv.toml" if (directory / "uv.toml").is_file() else "pyproject.toml"
    report.add(
        directory / target,
        f'no default index points at stuff; add [[tool.uv.index]] url = "{policy.python_index}" '
        "default = true (or [[index]] in uv.toml)",
    )


def _uv_lock(report: _Report, policy: RegistryPolicy, lock: Path) -> None:
    document, error = read_toml(lock)
    if document is None:
        raise ValueError(f"cannot read {_relative(report.root, lock)}: {error}")
    other: dict[str, list[str]] = {}
    for package in document.get("package", []):
        source = package.get("source")
        name = str(package.get("name"))
        if not isinstance(source, dict):
            continue
        if "registry" in source:
            if source["registry"] != policy.python_index:
                other.setdefault(str(source["registry"]), []).append(name)
        elif "git" in source:
            report.git_source(lock, "python", name, str(source["git"]).split("#")[0])
        elif any(kind in source for kind in ("editable", "virtual", "directory", "path")):
            kind = next(kind for kind in ("editable", "virtual", "directory", "path") if kind in source)
            target = str(source[kind])
            if _outside_repository(report.root, lock.parent, target):
                report.add(
                    lock,
                    f"python package {name} resolves from {target} outside the repository; "
                    "depend on a published stuff package instead",
                )
        else:
            other.setdefault(", ".join(sorted(map(str, source))), []).append(name)
    for source, names in other.items():
        report.unapproved(
            lock,
            "python",
            source,
            names,
            policy.python_index,
            f"set the default index to {policy.python_index} and run uv lock",
        )


def registry_source_findings(
    root: Path, repository: Repository, policy: RegistryPolicy
) -> list[Finding]:
    if "fork" in repository.tags:
        return []
    report = _Report(repository, root)
    files = _walk(root)
    for lock in files.get("Cargo.lock", []):
        _cargo_config(report, policy, lock.parent)
        _cargo_lock(report, policy, lock)
    npm_directories = {path.parent for name in ("pnpm-lock.yaml", "package-lock.json") for path in files.get(name, [])}
    if (root / "package.json").is_file():
        npm_directories.add(root)
    for directory in sorted(npm_directories):
        _npm_config(report, policy, directory)
    for lock in files.get("pnpm-lock.yaml", []):
        _pnpm_lock(report, policy, lock)
    for lock in files.get("package-lock.json", []):
        _package_lock(report, policy, lock)
    python_directories = {path.parent for path in files.get("uv.lock", [])}
    if (root / "pyproject.toml").is_file():
        python_directories.add(root)
    for directory in sorted(python_directories):
        _python_config(report, policy, directory)
    for lock in files.get("uv.lock", []):
        _uv_lock(report, policy, lock)
    return sorted(report.findings, key=lambda item: (item.file or "", item.message))


def _check(root: Path, repository: Repository) -> list[Finding]:
    policy = _default_policy()
    if not policy.enforce:
        return []
    return registry_source_findings(root, repository, policy)


def registry_sources_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            RULE_ID,
            "Require dependencies to resolve only from the private stuff registry",
            TagPredicate(),
            _check,
            None,
            None,
            (),
        ),
    )
