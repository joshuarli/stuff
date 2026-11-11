from __future__ import annotations

import hashlib
import posixpath
import re
import tomllib
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from functools import cache
from pathlib import Path, PurePosixPath
from typing import Any, ClassVar, cast

from repomod.dependencies.model import (
    AdapterResult,
    Component,
    CoverageKind,
    Ecosystem,
    Edge,
    Evidence,
    InputContent,
    Instance,
    Issue,
    Project,
    Relation,
    Resolution,
    Scope,
    SourceKind,
    purl,
    redact_url,
)

_CRATES_IO = frozenset({"https://github.com/rust-lang/crates.io-index", "https://index.crates.io"})
_REFERENCE = re.compile(r"(?P<name>\S+)(?: (?P<version>\S+)(?: \((?P<source>.+)\))?)?\Z")
_SHA = re.compile(r"[0-9a-f]{40}\Z")
_SIMPLE_KEY = re.compile(r"[A-Za-z0-9_-]+\Z")
_SECTIONS: tuple[tuple[str, Scope], ...] = (
    ("dependencies", Scope.RUNTIME),
    ("dev-dependencies", Scope.DEV),
    ("build-dependencies", Scope.BUILD),
)


@dataclass(frozen=True, slots=True)
class CargoAdapter:
    name: ClassVar[str] = "cargo"
    source: str | None = None
    native: str | None = None

    def matches(self, path: str) -> bool:
        return path.rsplit("/", 1)[-1] in {"Cargo.toml", "Cargo.lock"}

    def extract(self, inputs: Sequence[InputContent]) -> AdapterResult:
        issues: list[Issue] = []
        documents: dict[str, _Manifest] = {}
        locks: dict[str, _Lock] = {}
        for content in inputs:
            name = content.path.rsplit("/", 1)[-1]
            try:
                document = tomllib.loads(content.text)
            except tomllib.TOMLDecodeError as error:
                issues.append(
                    Issue(CoverageKind.PARSE_ERROR, f"invalid TOML: {error}", path=content.path)
                )
                continue
            if name == "Cargo.lock":
                lock = _parse_lock(content.path, document, issues)
                if lock is not None:
                    locks[content.path] = lock
            else:
                documents[content.path] = _raw_manifest(content.path, document, content.vendored)

        roots: dict[str, dict[str, Any]] = {}
        for candidate in documents.values():
            table = _workspace_table(candidate.document)
            if table is not None:
                roots[candidate.directory] = table
        manifests: dict[str, _Manifest] = {}
        for path, raw in documents.items():
            manifest = _resolve_manifest(raw, roots, issues)
            if manifest is not None:
                manifests[path] = manifest
        local = {
            (_normalize(manifest.package), manifest.version): manifest
            for manifest in manifests.values()
            if manifest.package is not None
        }

        plans = [
            plan
            for manifest in sorted(manifests.values(), key=lambda item: item.path)
            if (plan := _project_plan(manifest, locks, issues)) is not None
        ]
        drafts = _instances(locks, plans, manifests, local, self._mirrors(), issues)
        contexts = _contexts(drafts)
        instance_ids = {key: _instance_id(draft, contexts) for key, draft in drafts.items()}

        components: dict[str, Component] = {}
        instances: list[Instance] = []
        for key, draft in drafts.items():
            component = Component(draft.component_purl, draft.ecosystem, draft.name, draft.name)
            components.setdefault(component.purl, component)
            instances.append(
                Instance(
                    id=instance_ids[key],
                    purl=draft.purl,
                    component=draft.component_purl,
                    version=draft.version,
                    source_kind=draft.source_kind,
                    context=contexts[key],
                    registry_url=draft.registry_url,
                    repository_url=draft.repository_url,
                    vcs_url=draft.vcs_url,
                    commit=draft.commit,
                    first_party=draft.first_party,
                )
            )

        edges: list[Edge] = []
        for plan in plans:
            edges.extend(_direct_edges(plan, drafts, instance_ids, manifests, issues))
        edges.extend(_transitive_edges(locks, drafts, instance_ids, issues))
        return AdapterResult(
            projects=tuple(plan.project for plan in plans),
            components=tuple(components.values()),
            instances=tuple(instances),
            edges=tuple(edges),
            issues=_deduplicate_issues(issues),
        )

    def _mirrors(self) -> tuple[str | None, str | None]:
        if self.source is not None or self.native is not None:
            return self.source, self.native
        return _default_mirrors()


@dataclass(frozen=True, slots=True)
class _Manifest:
    path: str
    directory: str
    document: Mapping[str, Any]
    workspace: str | None
    package: str | None
    version: str | None
    dependencies: tuple[_Declaration, ...]
    features: Mapping[str, tuple[str, ...]]
    vendored: bool


@dataclass(frozen=True, slots=True)
class _Declaration:
    key: str
    package: str
    scope: Scope
    target: str | None
    constraint: str | None
    path: str | None
    git: str | None
    rev: str | None
    tag: str | None
    branch: str | None
    optional: bool
    locator: str
    vendored: bool
    unresolved: bool


@dataclass(frozen=True, slots=True)
class _LockedPackage:
    name: str
    version: str
    source: str | None
    dependencies: tuple[str, ...]
    path: str


@dataclass(frozen=True, slots=True)
class _Lock:
    path: str
    packages: tuple[_LockedPackage, ...]


@dataclass(frozen=True, slots=True)
class _Reference:
    name: str
    version: str | None
    source: str | None


@dataclass(frozen=True, slots=True)
class _ResolvedDeclaration:
    declaration: _Declaration
    package: _LockedPackage | None
    resolution: Resolution
    reference: str | None


@dataclass(frozen=True, slots=True)
class _ProjectPlan:
    manifest: _Manifest
    project: Project
    lock: _Lock | None
    own: _LockedPackage | None
    declarations: tuple[_ResolvedDeclaration, ...]


@dataclass(frozen=True, slots=True)
class _Draft:
    name: str
    version: str | None
    ecosystem: Ecosystem
    source_kind: SourceKind
    source_key: str
    registry_url: str | None = None
    repository_url: str | None = None
    vcs_url: str | None = None
    commit: str | None = None
    first_party: bool = False
    qualifiers: tuple[tuple[str, str], ...] = ()

    @property
    def purl(self) -> str:
        return purl(self.ecosystem, self.name, self.version, qualifiers=dict(self.qualifiers))

    @property
    def component_purl(self) -> str:
        return purl(self.ecosystem, self.name, qualifiers=dict(self.qualifiers))


def _raw_manifest(path: str, document: dict[str, Any], vendored: bool) -> _Manifest:
    return _Manifest(
        path,
        posixpath.dirname(path) or ".",
        document,
        None,
        None,
        None,
        (),
        {},
        vendored,
    )


def _workspace_table(document: Mapping[str, Any]) -> dict[str, Any] | None:
    workspace = document.get("workspace")
    return cast(dict[str, Any], workspace) if isinstance(workspace, dict) else None


def _resolve_manifest(
    manifest: _Manifest, roots: Mapping[str, dict[str, Any]], issues: list[Issue]
) -> _Manifest | None:
    package = manifest.document.get("package")
    if not isinstance(package, dict):
        return None
    name = package.get("name")
    if not isinstance(name, str) or not name:
        issues.append(
            Issue(CoverageKind.PARSE_ERROR, "package name is missing or not a string", path=manifest.path)
        )
        return None
    cover = _cover(manifest.directory, roots)
    version = _package_version(manifest, package, cover, roots, issues)
    return _Manifest(
        path=manifest.path,
        directory=manifest.directory,
        document=manifest.document,
        workspace=cover,
        package=name,
        version=version,
        dependencies=_declarations(manifest, cover, roots, issues),
        features=_features(manifest.document, manifest.path, issues),
        vendored=manifest.vendored,
    )


def _cover(directory: str, roots: Mapping[str, dict[str, Any]]) -> str | None:
    if directory in roots:
        return directory
    candidates = [root for root in roots if root == "." or directory.startswith(root + "/")]
    if not candidates:
        return None
    root = max(candidates, key=len)
    excluded = roots[root].get("exclude")
    if isinstance(excluded, list) and _excluded(directory, root, excluded):
        return None
    return root


def _excluded(directory: str, root: str, patterns: Sequence[object]) -> bool:
    relative = directory if root == "." else directory[len(root) + 1 :]
    parts = relative.split("/")
    candidates = ["/".join(parts[: index + 1]) for index in range(len(parts))]
    for pattern in patterns:
        if not isinstance(pattern, str):
            continue
        normalized = pattern.rstrip("/")
        if any(PurePosixPath(candidate).full_match(normalized) for candidate in candidates):
            return True
    return False


def _package_version(
    manifest: _Manifest,
    package: Mapping[str, Any],
    root: str | None,
    roots: Mapping[str, dict[str, Any]],
    issues: list[Issue],
) -> str | None:
    version = package.get("version")
    if isinstance(version, dict) and version.get("workspace") is True:
        table = roots.get(root or "")
        workspace_package = table.get("package") if table is not None else None
        version = workspace_package.get("version") if isinstance(workspace_package, dict) else None
    if version is None:
        return None
    if not isinstance(version, str) or not version:
        issues.append(
            Issue(CoverageKind.PARSE_ERROR, "package version is not a string", path=manifest.path)
        )
        return None
    return version


def _features(
    document: Mapping[str, Any], path: str, issues: list[Issue]
) -> Mapping[str, tuple[str, ...]]:
    features: dict[str, tuple[str, ...]] = {}
    table = document.get("features")
    if not isinstance(table, dict):
        return features
    for name, entries in table.items():
        if not isinstance(name, str) or not isinstance(entries, list):
            issues.append(
                Issue(CoverageKind.DYNAMIC_VALUE, "feature is not a list of strings", path=path)
            )
            continue
        features[name] = tuple(entry for entry in entries if isinstance(entry, str))
    return features


def _declarations(
    manifest: _Manifest,
    root: str | None,
    roots: Mapping[str, dict[str, Any]],
    issues: list[Issue],
) -> tuple[_Declaration, ...]:
    sections: list[tuple[Mapping[str, Any], Scope, str | None, str]] = []
    for section, scope in _SECTIONS:
        table = manifest.document.get(section)
        if isinstance(table, dict):
            sections.append((cast(Mapping[str, Any], table), scope, None, section))
    targets = manifest.document.get("target")
    if isinstance(targets, dict):
        for target, table in targets.items():
            if not isinstance(target, str) or not isinstance(table, dict):
                continue
            for section, scope in _SECTIONS:
                nested = table.get(section)
                if isinstance(nested, dict):
                    sections.append((cast(Mapping[str, Any], nested), scope, target, section))
    declarations: list[_Declaration] = []
    for table, scope, target, section in sections:
        for key, spec in table.items():
            prefix = f"target.{_key(target)}." if target is not None else ""
            locator = f"{prefix}{section}.{_key(key)}"
            declaration = _declaration(
                manifest, key, spec, scope, target, locator, root, roots, issues
            )
            if declaration is not None:
                declarations.append(declaration)
    return tuple(declarations)


def _declaration(
    manifest: _Manifest,
    key: str,
    spec: object,
    scope: Scope,
    target: str | None,
    locator: str,
    root: str | None,
    roots: Mapping[str, dict[str, Any]],
    issues: list[Issue],
) -> _Declaration | None:
    if isinstance(spec, str):
        table: dict[str, Any] = {"version": spec}
        inherit = False
    elif isinstance(spec, dict):
        table = cast(dict[str, Any], spec)
        inherit = table.get("workspace") is True
    else:
        issues.append(
            Issue(
                CoverageKind.PARSE_ERROR,
                "dependency declaration is not a string or table",
                path=manifest.path,
                locator=locator,
            )
        )
        return None
    merged = dict(table)
    unresolved = False
    if inherit:
        workspace_table = roots.get(root or "")
        dependencies = workspace_table.get("dependencies") if workspace_table is not None else None
        inherited = (
            dependencies.get(key) if isinstance(dependencies, dict) else None
        )
        if isinstance(inherited, str):
            merged = {"version": inherited}
        elif isinstance(inherited, dict):
            merged = dict(cast(dict[str, Any], inherited))
        else:
            issues.append(
                Issue(
                    CoverageKind.DYNAMIC_VALUE,
                    f"workspace dependency {key} is not declared",
                    path=manifest.path,
                    locator=locator,
                )
            )
            merged = {}
            unresolved = True
        for field in ("features", "optional", "default-features"):
            if field in table:
                merged[field] = table[field]
    package = merged.get("package")
    package_name = package if isinstance(package, str) and package else key
    version = merged.get("version")
    if version is not None and not isinstance(version, str):
        issues.append(
            Issue(
                CoverageKind.PARSE_ERROR,
                "dependency version is not a string",
                path=manifest.path,
                locator=locator,
            )
        )
        version = None
    path = merged.get("path")
    git = merged.get("git")
    registry = merged.get("registry")
    if registry is not None:
        issues.append(
            Issue(
                CoverageKind.DYNAMIC_VALUE,
                f"named registry {registry!r} cannot be resolved statically",
                path=manifest.path,
                locator=locator,
            )
        )
    if path is None and git is None and version is None and not unresolved:
        issues.append(
            Issue(
                CoverageKind.DYNAMIC_VALUE,
                "dependency has no version, path, or Git source",
                path=manifest.path,
                locator=locator,
            )
        )
    rev = _string_or_none(merged.get("rev"))
    tag = _string_or_none(merged.get("tag"))
    branch = _string_or_none(merged.get("branch"))
    constraint = version if isinstance(version, str) else (rev or tag or branch)
    return _Declaration(
        key=key,
        package=package_name,
        scope=scope,
        target=target,
        constraint=constraint,
        path=_relative_path(manifest.directory, path) if isinstance(path, str) else None,
        git=git if isinstance(git, str) and git else None,
        rev=rev,
        tag=tag,
        branch=branch,
        optional=merged.get("optional") is True,
        locator=locator,
        vendored=manifest.vendored,
        unresolved=unresolved,
    )


def _relative_path(directory: str, path: str) -> str:
    return posixpath.normpath(posixpath.join(directory, path))


def _string_or_none(value: object) -> str | None:
    return value if isinstance(value, str) and value else None


def _lock_path(manifest: _Manifest) -> str:
    directory = manifest.workspace or manifest.directory
    return "Cargo.lock" if directory == "." else f"{directory}/Cargo.lock"


def _parse_lock(path: str, document: dict[str, Any], issues: list[Issue]) -> _Lock | None:
    packages = document.get("package")
    if not isinstance(packages, list):
        issues.append(Issue(CoverageKind.PARSE_ERROR, "Cargo.lock has no package list", path=path))
        return None
    parsed: list[_LockedPackage] = []
    for raw in packages:
        if not isinstance(raw, dict):
            issues.append(Issue(CoverageKind.PARSE_ERROR, "lock package is not a table", path=path))
            continue
        name = raw.get("name")
        version = raw.get("version")
        source = raw.get("source")
        dependencies = raw.get("dependencies", [])
        if (
            not isinstance(name, str)
            or not name
            or not isinstance(version, str)
            or not version
            or (source is not None and not isinstance(source, str))
            or not isinstance(dependencies, list)
        ):
            issues.append(Issue(CoverageKind.PARSE_ERROR, "lock package is malformed", path=path))
            continue
        references = tuple(item for item in dependencies if isinstance(item, str))
        if len(references) != len(dependencies):
            issues.append(
                Issue(
                    CoverageKind.PARSE_ERROR,
                    f"lock package {name} has non-string dependencies",
                    path=path,
                )
            )
        parsed.append(_LockedPackage(name, version, source, references, path))
    return _Lock(path, tuple(parsed))


def _reference(value: str) -> _Reference | None:
    match = _REFERENCE.fullmatch(value)
    if match is None:
        return None
    return _Reference(match.group("name"), match.group("version"), match.group("source"))


def _project_plan(
    manifest: _Manifest, locks: Mapping[str, _Lock], issues: list[Issue]
) -> _ProjectPlan | None:
    if manifest.package is None:
        return None
    lock = locks.get(_lock_path(manifest))
    consuming = False
    if lock is None:
        lock = _consuming_lock(manifest, locks)
        consuming = lock is not None
    own = _own_package(lock, manifest) if lock is not None else None
    if lock is not None and manifest.dependencies and own is None:
        issues.append(
            Issue(
                CoverageKind.LOCK_MISMATCH,
                f"{lock.path} has no package entry for {manifest.package} {manifest.version}",
                path=manifest.path,
            )
        )
    if lock is None and manifest.dependencies and not _cargo_fuzz(manifest):
        issues.append(
            Issue(
                CoverageKind.MISSING_LOCK,
                f"no lockfile at {_lock_path(manifest)}",
                path=_lock_path(manifest),
            )
        )
    resolved: list[_ResolvedDeclaration] = []
    for declaration in manifest.dependencies:
        if lock is None:
            if (
                declaration.path is None
                and declaration.git is None
                and declaration.constraint is None
                and not declaration.unresolved
            ):
                issues.append(
                    Issue(
                        CoverageKind.DYNAMIC_VALUE,
                        f"cannot establish a version for {declaration.package}",
                        path=manifest.path,
                        locator=declaration.locator,
                    )
                )
            resolved.append(
                _ResolvedDeclaration(declaration, None, _unlocked_resolution(declaration), None)
            )
            continue
        if own is None:
            resolved.append(_ResolvedDeclaration(declaration, None, Resolution.UNKNOWN, None))
            continue
        package = _match_lock(lock, declaration, own)
        if package is None:
            if consuming:
                resolved.append(
                    _ResolvedDeclaration(
                        declaration, None, _unlocked_resolution(declaration), None
                    )
                )
                continue
            issues.append(
                Issue(
                    CoverageKind.LOCK_MISMATCH,
                    f"{declaration.package} is not resolved in {lock.path}",
                    path=manifest.path,
                    locator=declaration.locator,
                )
            )
            resolved.append(_ResolvedDeclaration(declaration, None, Resolution.UNKNOWN, None))
            continue
        resolved.append(
            _ResolvedDeclaration(
                declaration, package, Resolution.RESOLVED, _reference_text(package, own)
            )
        )
    return _ProjectPlan(
        manifest=manifest,
        project=Project(f"{manifest.directory}:cargo", Ecosystem.CARGO, manifest.directory),
        lock=lock,
        own=own,
        declarations=tuple(resolved),
    )


def _consuming_lock(manifest: _Manifest, locks: Mapping[str, _Lock]) -> _Lock | None:
    if manifest.package is None or manifest.version is None:
        return None
    wanted = _normalize(manifest.package)
    own_path = _lock_path(manifest)
    for path in sorted(locks):
        if path == own_path:
            continue
        lock = locks[path]
        for package in lock.packages:
            if (
                package.source is None
                and _normalize(package.name) == wanted
                and package.version == manifest.version
            ):
                return lock
    return None


def _cargo_fuzz(manifest: _Manifest) -> bool:
    package = manifest.document.get("package")
    if not isinstance(package, Mapping):
        return False
    metadata = package.get("metadata")
    if not isinstance(metadata, Mapping):
        return False
    return metadata.get("cargo-fuzz") is True


def _unlocked_resolution(declaration: _Declaration) -> Resolution:
    if declaration.path is not None:
        return Resolution.RESOLVED
    if (
        declaration.git is not None
        and isinstance(declaration.rev, str)
        and _SHA.fullmatch(declaration.rev)
    ):
        return Resolution.RESOLVED
    if declaration.constraint is not None:
        return Resolution.DECLARED_ONLY
    return Resolution.UNKNOWN


def _own_package(lock: _Lock, manifest: _Manifest) -> _LockedPackage | None:
    if manifest.package is None:
        return None
    for package in lock.packages:
        if (
            package.source is None
            and _normalize(package.name) == _normalize(manifest.package)
            and package.version == manifest.version
        ):
            return package
    return None


def _match_lock(
    lock: _Lock, declaration: _Declaration, own: _LockedPackage
) -> _LockedPackage | None:
    wanted = _normalize(declaration.package)
    candidates = [package for package in lock.packages if _normalize(package.name) == wanted]
    if not candidates:
        return None
    references = [
        reference
        for item in own.dependencies
        if (reference := _reference(item)) is not None and _normalize(reference.name) == wanted
    ]
    for reference in references:
        exact = _resolve_reference(lock, reference)
        if exact is not None:
            return exact
    return min(
        candidates,
        key=lambda package: (_score(package, declaration), package.source or "", package.version),
    )


def _resolve_reference(lock: _Lock, reference: _Reference) -> _LockedPackage | None:
    wanted = _normalize(reference.name)
    for package in lock.packages:
        if _normalize(package.name) != wanted:
            continue
        if reference.version is not None and package.version != reference.version:
            continue
        if reference.source is not None and package.source != reference.source:
            continue
        return package
    return None


def _score(package: _LockedPackage, declaration: _Declaration) -> int:
    if declaration.path is not None:
        return 0 if package.source is None else 1
    if declaration.git is not None:
        return 0 if package.source is not None and package.source.startswith("git+") else 1
    return 0 if package.source is not None and not package.source.startswith("git+") else 1


def _reference_text(package: _LockedPackage, own: _LockedPackage) -> str:
    for item in own.dependencies:
        reference = _reference(item)
        if reference is None or _normalize(reference.name) != _normalize(package.name):
            continue
        if reference.version is not None and reference.version != package.version:
            continue
        if reference.source is not None and reference.source != package.source:
            continue
        return item
    return package.name


def _instances(
    locks: Mapping[str, _Lock],
    plans: Sequence[_ProjectPlan],
    manifests: Mapping[str, _Manifest],
    local: Mapping[tuple[str, str | None], _Manifest],
    mirrors: tuple[str | None, str | None],
    issues: list[Issue],
) -> dict[tuple[str, str], _Draft]:
    drafts: dict[tuple[str, str], _Draft] = {}
    for lock in locks.values():
        for package in lock.packages:
            draft = _lock_draft(package, local, mirrors, issues)
            drafts.setdefault((draft.purl, draft.source_key), draft)
    for plan in plans:
        for item in plan.declarations:
            if item.package is not None:
                continue
            draft = _declaration_draft(item.declaration, manifests)
            drafts.setdefault((draft.purl, draft.source_key), draft)
    return drafts


def _lock_draft(
    package: _LockedPackage,
    local: Mapping[tuple[str, str | None], _Manifest],
    mirrors: tuple[str | None, str | None],
    issues: list[Issue],
) -> _Draft:
    if package.source is None:
        manifest = local.get((_normalize(package.name), package.version))
        directory = manifest.directory if manifest is not None else f"lock:{_normalize(package.name)}"
        kind = SourceKind.WORKSPACE if manifest is not None and manifest.workspace is not None else SourceKind.PATH
        return _Draft(
            name=package.name,
            version=package.version,
            ecosystem=Ecosystem.GENERIC,
            source_kind=kind,
            source_key=f"{kind.value}:{directory}",
            first_party=True,
        )
    kind, url, commit, qualifiers = _source(package.source, mirrors, package.path, issues)
    return _Draft(
        name=package.name,
        version=package.version,
        ecosystem=Ecosystem.CARGO,
        source_kind=kind,
        source_key=package.source,
        registry_url=url if kind is SourceKind.REGISTRY else None,
        repository_url=url if kind is SourceKind.REGISTRY and qualifiers else None,
        vcs_url=url if kind is SourceKind.GIT else None,
        commit=commit,
        qualifiers=qualifiers,
    )


def _source(
    source: str,
    mirrors: tuple[str | None, str | None],
    path: str,
    issues: list[Issue],
) -> tuple[SourceKind, str | None, str | None, tuple[tuple[str, str], ...]]:
    if source.startswith(("registry+", "sparse+")):
        url = redact_url(source.split("+", 1)[1], drop_query_and_fragment=True)
        qualifiers = () if _public_registry(url, mirrors) else (("repository_url", url),)
        return SourceKind.REGISTRY, url, None, qualifiers
    if source.startswith("git+"):
        value = source.removeprefix("git+")
        base, _, fragment = value.partition("#")
        commit = fragment if _SHA.fullmatch(fragment) else None
        url = redact_url(base.split("?", 1)[0], drop_query_and_fragment=True)
        return SourceKind.GIT, url, commit, (("vcs_url", url),)
    issues.append(
        Issue(CoverageKind.DYNAMIC_VALUE, f"unsupported lock source: {source}", path=path)
    )
    url = redact_url(source, drop_query_and_fragment=True)
    return SourceKind.REGISTRY, url, None, (("repository_url", url),)


def _public_registry(url: str, mirrors: tuple[str | None, str | None]) -> bool:
    normalized = url.rstrip("/")
    if normalized in _CRATES_IO:
        return True
    source = mirrors[0]
    return (
        source is not None
        and normalized == source.removeprefix("sparse+").removeprefix("registry+").rstrip("/")
    )


def _declaration_draft(
    declaration: _Declaration, manifests: Mapping[str, _Manifest]
) -> _Draft:
    if declaration.path is not None:
        target = next(
            (item for item in manifests.values() if item.directory == declaration.path),
            None,
        )
        version = target.version if target is not None else None
        if declaration.vendored:
            return _Draft(
                name=declaration.package,
                version=version,
                ecosystem=Ecosystem.GENERIC,
                source_kind=SourceKind.VENDORED,
                source_key=f"vendored:{_normalize(declaration.package)}",
                first_party=True,
            )
        kind = (
            SourceKind.WORKSPACE
            if target is not None and target.workspace is not None
            else SourceKind.PATH
        )
        directory = target.directory if target is not None else declaration.path
        return _Draft(
            name=declaration.package,
            version=version,
            ecosystem=Ecosystem.GENERIC,
            source_kind=kind,
            source_key=f"{kind.value}:{directory}",
            first_party=True,
        )
    if declaration.git is not None:
        commit = (
            declaration.rev
            if isinstance(declaration.rev, str) and _SHA.fullmatch(declaration.rev)
            else None
        )
        url = redact_url(declaration.git, drop_query_and_fragment=True)
        return _Draft(
            name=declaration.package,
            version=None,
            ecosystem=Ecosystem.CARGO,
            source_kind=SourceKind.VENDORED if declaration.vendored else SourceKind.GIT,
            source_key=(
                f"vendored:{_normalize(declaration.package)}"
                if declaration.vendored
                else f"git:{url}:{commit or ''}"
            ),
            vcs_url=url,
            commit=commit,
            qualifiers=(("vcs_url", url),),
        )
    return _Draft(
        name=declaration.package,
        version=None,
        ecosystem=Ecosystem.CARGO,
        source_kind=SourceKind.VENDORED if declaration.vendored else SourceKind.REGISTRY,
        source_key=(
            f"vendored:{_normalize(declaration.package)}"
            if declaration.vendored
            else "registry:public"
        ),
    )


def _contexts(drafts: Mapping[tuple[str, str], _Draft]) -> dict[tuple[str, str], str]:
    groups: dict[str, set[str]] = {}
    for draft in drafts.values():
        groups.setdefault(draft.purl, set()).add(draft.source_key)
    contexts: dict[tuple[str, str], str] = {}
    for key, draft in drafts.items():
        if len(groups[draft.purl]) > 1:
            digest = hashlib.sha256(f"{draft.purl}\0{draft.source_key}".encode()).hexdigest()
            contexts[key] = digest[:16]
        else:
            contexts[key] = ""
    return contexts


def _instance_id(draft: _Draft, contexts: Mapping[tuple[str, str], str]) -> str:
    return f"{draft.purl}#{contexts[(draft.purl, draft.source_key)]}"


def _direct_edges(
    plan: _ProjectPlan,
    drafts: Mapping[tuple[str, str], _Draft],
    instance_ids: Mapping[tuple[str, str], str],
    manifests: Mapping[str, _Manifest],
    issues: list[Issue],
) -> list[Edge]:
    edges: list[Edge] = []
    for item in plan.declarations:
        declaration = item.declaration
        condition = _condition(declaration, plan.manifest)
        if item.package is None:
            target = _declaration_draft(declaration, manifests)
            resolution = item.resolution
            evidence = [Evidence(plan.manifest.path, declaration.locator)]
        else:
            target = _draft_for_package(item.package, drafts)
            resolution = Resolution.CONDITIONAL if condition is not None else Resolution.RESOLVED
            evidence = [Evidence(plan.manifest.path, declaration.locator)]
            if (
                plan.own is not None
                and plan.lock is not None
                and item.reference in plan.own.dependencies
            ):
                index = plan.own.dependencies.index(item.reference or "")
                evidence.append(
                    Evidence(
                        plan.lock.path,
                        f"package[name={plan.own.name},version={plan.own.version}].dependencies[{index}]",
                    )
                )
        edges.append(
            Edge(
                from_id=plan.project.id,
                to_id=instance_ids[(target.purl, target.source_key)],
                relation=Relation.DIRECT,
                scope=declaration.scope,
                optional=declaration.optional,
                constraint=declaration.constraint,
                resolved_version=target.version,
                condition=condition,
                source_kind=SourceKind.VENDORED if declaration.vendored else target.source_kind,
                resolution=resolution,
                evidence=tuple(evidence),
            )
        )
    return edges


def _deduplicate_issues(issues: Sequence[Issue]) -> tuple[Issue, ...]:
    seen: set[tuple[CoverageKind, str | None]] = set()
    unique: list[Issue] = []
    for issue in issues:
        if issue.kind is CoverageKind.MISSING_LOCK:
            key = (issue.kind, issue.path)
            if key in seen:
                continue
            seen.add(key)
        unique.append(issue)
    return tuple(unique)


def _draft_for_package(
    package: _LockedPackage, drafts: Mapping[tuple[str, str], _Draft]
) -> _Draft:
    if package.source is not None:
        for draft in drafts.values():
            if (
                draft.source_key == package.source
                and _normalize(draft.name) == _normalize(package.name)
                and draft.version == package.version
            ):
                return draft
        raise ValueError(f"lock package is missing from the instance set: {package.name}")
    candidates = [
        draft
        for draft in drafts.values()
        if draft.ecosystem is Ecosystem.GENERIC
        and _normalize(draft.name) == _normalize(package.name)
        and draft.version == package.version
    ]
    if not candidates:
        raise ValueError(f"local lock package is missing from the instance set: {package.name}")
    candidates.sort(key=lambda draft: (draft.source_kind is not SourceKind.WORKSPACE, draft.source_key))
    return candidates[0]


def _transitive_edges(
    locks: Mapping[str, _Lock],
    drafts: Mapping[tuple[str, str], _Draft],
    instance_ids: Mapping[tuple[str, str], str],
    issues: list[Issue],
) -> list[Edge]:
    edges: list[Edge] = []
    for lock in locks.values():
        for package in lock.packages:
            if package.source is None:
                continue
            origin = _draft_for_package(package, drafts)
            origin_id = instance_ids[(origin.purl, origin.source_key)]
            for index, item in enumerate(package.dependencies):
                reference = _reference(item)
                target = _resolve_reference(lock, reference) if reference is not None else None
                if target is None:
                    issues.append(
                        Issue(
                            CoverageKind.PARSE_ERROR,
                            f"lock dependency {item!r} does not resolve in {lock.path}",
                            path=lock.path,
                            locator=(
                                f"package[name={package.name},version={package.version}]"
                                f".dependencies[{index}]"
                            ),
                        )
                    )
                    continue
                draft = _draft_for_package(target, drafts)
                edges.append(
                    Edge(
                        from_id=origin_id,
                        to_id=instance_ids[(draft.purl, draft.source_key)],
                        relation=Relation.TRANSITIVE,
                        scope=Scope.RUNTIME,
                        optional=False,
                        constraint=None,
                        resolved_version=target.version,
                        condition=None,
                        source_kind=draft.source_kind,
                        resolution=Resolution.RESOLVED,
                        evidence=(
                            Evidence(
                                lock.path,
                                f"package[name={package.name},version={package.version}]"
                                f".dependencies[{index}]",
                            ),
                        ),
                    )
                )
    return edges


def _condition(declaration: _Declaration, manifest: _Manifest) -> str | None:
    parts: list[str] = []
    if declaration.target is not None:
        parts.append(declaration.target)
    if declaration.optional:
        parts.append(f"feature:{_feature_for(declaration.key, manifest.features)}")
    return "; ".join(parts) or None


def _feature_for(key: str, features: Mapping[str, tuple[str, ...]]) -> str:
    enabling = sorted(name for name, entries in features.items() if f"dep:{key}" in entries)
    return enabling[0] if enabling else key


def _normalize(value: str) -> str:
    return value.replace("_", "-").casefold()


def _key(value: str) -> str:
    if _SIMPLE_KEY.fullmatch(value):
        return value
    if "'" not in value:
        return f"'{value}'"
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


@cache
def _default_mirrors() -> tuple[str | None, str | None]:
    path = Path(__file__).resolve().parents[4] / "registries.toml"
    try:
        document = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        raise ValueError(f"cannot read {path}: {error}") from error
    cargo = document.get("cargo")
    if not isinstance(cargo, dict):
        return None, None
    source = cargo.get("source")
    native = cargo.get("native")
    return (
        source if isinstance(source, str) else None,
        native if isinstance(native, str) else None,
    )
