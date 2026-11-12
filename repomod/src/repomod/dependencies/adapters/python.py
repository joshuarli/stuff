from __future__ import annotations

import hashlib
import posixpath
import re
import tomllib
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from functools import cache
from pathlib import Path, PurePosixPath
from typing import Any, ClassVar, Literal, cast

from packaging.markers import InvalidMarker, Marker
from packaging.requirements import InvalidRequirement, Requirement
from packaging.utils import NormalizedName, canonicalize_name
from packaging.version import InvalidVersion, Version

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

_PUBLIC_PYPI = frozenset({"https://pypi.org/simple"})
_SHA = re.compile(r"[0-9a-f]{40}\Z")
_SIMPLE_KEY = re.compile(r"[A-Za-z0-9_-]+\Z")

type SourceShape = Literal["path", "git", "url", "registry"]


@dataclass(frozen=True, slots=True)
class PythonAdapter:
    name: ClassVar[str] = "python"
    index: str | None = None

    def matches(self, path: str) -> bool:
        name = path.rsplit("/", 1)[-1]
        return name in {"pyproject.toml", "uv.lock"} or (
            name.startswith("requirements") and name.endswith(".txt")
        )

    def extract(self, inputs: Sequence[InputContent]) -> AdapterResult:
        issues: list[Issue] = []
        pyprojects: dict[str, _RawManifest] = {}
        locks: dict[str, _Lock] = {}
        requirements: dict[str, tuple[_TextRequirement, ...]] = {}
        for content in inputs:
            name = content.path.rsplit("/", 1)[-1]
            if name.startswith("requirements") and name.endswith(".txt"):
                requirements[content.path] = _parse_requirements(
                    content.path, content.text, content.vendored, issues
                )
                continue
            try:
                document = tomllib.loads(content.text)
            except tomllib.TOMLDecodeError as error:
                issues.append(
                    Issue(CoverageKind.PARSE_ERROR, f"invalid TOML: {error}", path=content.path)
                )
                continue
            if name == "uv.lock":
                lock = _parse_lock(content.path, document, issues)
                if lock is not None:
                    locks[content.path] = lock
            else:
                pyprojects[content.path] = _raw_manifest(content.path, document, content.vendored)

        roots: dict[str, Mapping[str, Any]] = {}
        for raw in pyprojects.values():
            if _workspace_table(raw.document) is not None:
                roots[raw.directory] = raw.document
        manifests: dict[str, _Manifest] = {}
        for path, raw in pyprojects.items():
            manifest = _resolve_manifest(raw, roots, issues)
            if manifest is not None:
                manifests[path] = manifest
        local = {
            canonicalize_name(manifest.name): manifest
            for manifest in manifests.values()
            if manifest.name is not None
        }

        plans = _plans(manifests, locks, requirements, issues)
        drafts, package_keys = _instances(
            locks, plans, manifests, local, self._index(), issues
        )
        contexts = _contexts(drafts)
        instance_ids = {key: _instance_id(draft, contexts) for key, draft in drafts.items()}

        components: dict[str, Component] = {}
        instances: list[Instance] = []
        for key, draft in drafts.items():
            component = Component(draft.component_purl, draft.ecosystem, draft.name, draft.display)
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
            edges.extend(
                _direct_edges(plan, drafts, instance_ids, manifests, package_keys, issues)
            )
        edges.extend(_transitive_edges(locks, drafts, instance_ids, package_keys, issues))
        return AdapterResult(
            projects=tuple(plan.project for plan in plans),
            components=tuple(components.values()),
            instances=tuple(instances),
            edges=tuple(edges),
            issues=_deduplicate_issues(issues),
        )

    def _index(self) -> str | None:
        return self.index if self.index is not None else _default_index()


@dataclass(frozen=True, slots=True)
class _RawManifest:
    path: str
    directory: str
    document: Mapping[str, Any]
    vendored: bool


@dataclass(frozen=True, slots=True)
class _Manifest:
    path: str
    directory: str
    document: Mapping[str, Any]
    workspace: str | None
    name: str | None
    version: str | None
    dependencies: tuple[_Requirement, ...]
    sources: Mapping[str, _SourceSpec]
    vendored: bool


@dataclass(frozen=True, slots=True)
class _Requirement:
    requirement: Requirement
    scope: Scope
    condition: str | None
    optional: bool
    locator: str
    vendored: bool


@dataclass(frozen=True, slots=True)
class _TextRequirement:
    requirement: Requirement
    locator: str
    path: str
    vendored: bool


@dataclass(frozen=True, slots=True)
class _SourceSpec:
    shape: SourceShape
    value: str
    marker: str | None = None
    editable: bool = False
    rev: str | None = None
    base: str = "."


@dataclass(frozen=True, slots=True)
class _LockedRef:
    name: str
    version: str | None
    marker: str | None
    source: Mapping[str, Any] | None


@dataclass(frozen=True, slots=True)
class _LockedPackage:
    name: str
    version: str | None
    source: Mapping[str, Any] | None
    dependencies: tuple[_LockedRef, ...]
    optional: Mapping[str, tuple[_LockedRef, ...]]
    dev: Mapping[str, tuple[_LockedRef, ...]]
    markers: tuple[str, ...]
    path: str
    index: int


@dataclass(frozen=True, slots=True)
class _Lock:
    path: str
    packages: tuple[_LockedPackage, ...]


@dataclass(frozen=True, slots=True)
class _ResolvedRequirement:
    requirement: _Requirement
    package: _LockedPackage | None
    own: _LockedPackage | None
    ref: _LockedRef | None
    reference_index: int | None
    reference_group: tuple[str, str] | None
    resolution: Resolution


@dataclass(frozen=True, slots=True)
class _ProjectPlan:
    manifest: _Manifest | None
    project: Project
    lock: _Lock | None
    own: tuple[_LockedPackage, ...]
    declarations: tuple[_ResolvedRequirement, ...]
    text_requirements: tuple[_TextRequirement, ...]
    lock_path: str


@dataclass(frozen=True, slots=True)
class _Draft:
    name: str
    display: str
    version: str | None
    ecosystem: Ecosystem
    source_kind: SourceKind
    source_key: str
    directory: str | None = None
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


def _raw_manifest(path: str, document: dict[str, Any], vendored: bool) -> _RawManifest:
    return _RawManifest(path, posixpath.dirname(path) or ".", document, vendored)


def _table(value: object) -> dict[str, Any] | None:
    return cast(dict[str, Any], value) if isinstance(value, dict) else None


def _list(value: object) -> list[Any] | None:
    return cast(list[Any], value) if isinstance(value, list) else None


def _text(value: object) -> str | None:
    return value if isinstance(value, str) and value else None


def _workspace_table(document: Mapping[str, Any]) -> dict[str, Any] | None:
    tool = _table(document.get("tool"))
    uv = _table(tool.get("uv")) if tool is not None else None
    return _table(uv.get("workspace")) if uv is not None else None


def _resolve_manifest(
    raw: _RawManifest, roots: Mapping[str, Mapping[str, Any]], issues: list[Issue]
) -> _Manifest | None:
    project = _table(raw.document.get("project"))
    name = _text(project.get("name")) if project is not None else None
    if project is None or name is None:
        return None
    version = _text(project.get("version"))
    cover = _cover(raw.directory, roots)
    sources = _sources(raw.document, raw.directory, cover, roots)
    return _Manifest(
        path=raw.path,
        directory=raw.directory,
        document=raw.document,
        workspace=cover,
        name=name,
        version=version,
        dependencies=_requirements(raw, project, issues),
        sources=sources,
        vendored=raw.vendored,
    )


def _cover(directory: str, roots: Mapping[str, Mapping[str, Any]]) -> str | None:
    if directory in roots:
        return directory
    candidates = [root for root in roots if root == "." or directory.startswith(root + "/")]
    if not candidates:
        return None
    root = max(candidates, key=len)
    workspace = _workspace_table(roots[root])
    excluded = workspace.get("exclude") if workspace is not None else None
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


def _sources(
    document: Mapping[str, Any],
    directory: str,
    root: str | None,
    roots: Mapping[str, Mapping[str, Any]],
) -> Mapping[str, _SourceSpec]:
    sources: dict[str, _SourceSpec] = {}
    root_document = roots.get(root or "")
    if root_document is not None:
        for key, value in _source_table(root_document, root or "."):
            sources[canonicalize_name(key)] = value
    for key, value in _source_table(document, directory):
        sources[canonicalize_name(key)] = value
    return sources


def _source_table(
    document: Mapping[str, Any], base: str
) -> tuple[tuple[str, _SourceSpec], ...]:
    tool = _table(document.get("tool"))
    uv = _table(tool.get("uv")) if tool is not None else None
    table = _table(uv.get("sources")) if uv is not None else None
    if table is None:
        return ()
    entries: list[tuple[str, _SourceSpec]] = []
    for key, raw in table.items():
        spec = _source_spec(raw, base)
        if spec is not None:
            entries.append((key, spec))
    return tuple(entries)


def _source_spec(raw: object, base: str) -> _SourceSpec | None:
    if isinstance(raw, list):
        for item in raw:
            spec = _source_spec(item, base)
            if spec is not None:
                return spec
        return None
    table = _table(raw)
    if table is None:
        return None
    marker = _text(table.get("marker"))
    editable = table.get("editable") is True
    rev = _text(table.get("rev")) or _text(table.get("tag")) or _text(table.get("branch"))
    if table.get("workspace") is True:
        return _SourceSpec("path", "workspace", marker, editable, rev, base)
    path = _text(table.get("path"))
    if path is not None:
        return _SourceSpec("path", path, marker, editable, rev, base)
    git = _text(table.get("git"))
    if git is not None:
        return _SourceSpec("git", git, marker, editable, rev, base)
    url = _text(table.get("url"))
    if url is not None:
        return _SourceSpec("url", url, marker, editable, rev, base)
    return None


def _requirements(
    raw: _RawManifest, project: Mapping[str, Any], issues: list[Issue]
) -> tuple[_Requirement, ...]:
    requirements: list[_Requirement] = []
    requirements.extend(
        _parse_requirement_list(
            raw, project.get("dependencies"), "project.dependencies", Scope.RUNTIME, None, False, issues
        )
    )
    optional = _table(project.get("optional-dependencies"))
    if optional is not None:
        for group, values in optional.items():
            requirements.extend(
                _parse_requirement_list(
                    raw,
                    values,
                    f"project.optional-dependencies.{_key(group)}",
                    Scope.OPTIONAL,
                    f"extra == '{group}'",
                    True,
                    issues,
                )
            )
    groups = _table(raw.document.get("dependency-groups"))
    if groups is not None:
        expanded = _expand_groups(groups, issues, raw.path)
        for group in sorted(expanded):
            requirements.extend(
                _parse_requirement_list(
                    raw,
                    expanded[group],
                    f"dependency-groups.{_key(group)}",
                    Scope.DEV,
                    f"group == '{group}'",
                    False,
                    issues,
                )
            )
    return tuple(requirements)


def _expand_groups(
    groups: Mapping[str, Any], issues: list[Issue], path: str
) -> dict[str, list[str]]:
    expanded: dict[str, list[str]] = {}

    def resolve(name: str, seen: tuple[str, ...]) -> list[str]:
        if name in expanded:
            return expanded[name]
        if name in seen:
            issues.append(
                Issue(CoverageKind.PARSE_ERROR, f"dependency group {name} includes itself", path=path)
            )
            return []
        values = _list(groups.get(name))
        if values is None:
            issues.append(
                Issue(CoverageKind.DYNAMIC_VALUE, f"dependency group {name} is not declared", path=path)
            )
            return []
        items: list[str] = []
        for value in values:
            if isinstance(value, str):
                items.append(value)
                continue
            table = _table(value)
            included = _text(table.get("include-group")) if table is not None else None
            if included is None:
                issues.append(
                    Issue(
                        CoverageKind.DYNAMIC_VALUE,
                        f"dependency group {name} has an unsupported entry",
                        path=path,
                    )
                )
                continue
            items.extend(resolve(included, (*seen, name)))
        expanded[name] = items
        return items

    for name in groups:
        resolve(name, ())
    return expanded


def _parse_requirement_list(
    raw: _RawManifest,
    values: object,
    locator: str,
    scope: Scope,
    condition: str | None,
    optional: bool,
    issues: list[Issue],
) -> list[_Requirement]:
    items = _list(values)
    if items is None:
        return []
    requirements: list[_Requirement] = []
    for index, item in enumerate(items):
        if not isinstance(item, str):
            issues.append(
                Issue(
                    CoverageKind.PARSE_ERROR,
                    "requirement is not a string",
                    path=raw.path,
                    locator=f"{locator}[{index}]",
                )
            )
            continue
        try:
            requirement = Requirement(item)
        except InvalidRequirement as error:
            issues.append(
                Issue(
                    CoverageKind.PARSE_ERROR,
                    f"invalid requirement: {error}",
                    path=raw.path,
                    locator=f"{locator}[{index}]",
                )
            )
            continue
        requirements.append(
            _Requirement(
                requirement=requirement,
                scope=scope,
                condition=condition,
                optional=optional,
                locator=f"{locator}[{index}]",
                vendored=raw.vendored,
            )
        )
    return requirements


def _parse_requirements(
    path: str, text: str, vendored: bool, issues: list[Issue]
) -> tuple[_TextRequirement, ...]:
    requirements: list[_TextRequirement] = []
    for number, raw in enumerate(text.splitlines(), start=1):
        line = raw.split("#", 1)[0].strip()
        if not line or line.startswith("-"):
            continue
        try:
            requirement = Requirement(line)
        except InvalidRequirement as error:
            issues.append(
                Issue(
                    CoverageKind.PARSE_ERROR,
                    f"invalid requirement: {error}",
                    path=path,
                    locator=f"L{number}",
                )
            )
            continue
        requirements.append(_TextRequirement(requirement, f"L{number}", path, vendored))
    return tuple(requirements)


def _parse_lock(path: str, document: dict[str, Any], issues: list[Issue]) -> _Lock | None:
    packages = document.get("package")
    if not isinstance(packages, list):
        issues.append(Issue(CoverageKind.PARSE_ERROR, "uv.lock has no package list", path=path))
        return None
    parsed: list[_LockedPackage] = []
    for index, raw in enumerate(packages):
        table = _table(raw)
        if table is None:
            issues.append(Issue(CoverageKind.PARSE_ERROR, "lock package is not a table", path=path))
            continue
        name = _text(table.get("name"))
        version = _text(table.get("version"))
        source = _table(table.get("source"))
        if name is None or (version is None and not _local_source(source)):
            issues.append(Issue(CoverageKind.PARSE_ERROR, "lock package is malformed", path=path))
            continue
        dependencies = _lock_refs(table.get("dependencies"), path, issues)
        optional = _lock_ref_groups(table.get("optional-dependencies"), path, issues)
        dev = _lock_ref_groups(table.get("dev-dependencies"), path, issues)
        markers = tuple(
            item for item in _list(table.get("resolution-markers")) or [] if isinstance(item, str)
        )
        parsed.append(
            _LockedPackage(
                name=name,
                version=version,
                source=source,
                dependencies=dependencies,
                optional=optional,
                dev=dev,
                markers=markers,
                path=path,
                index=index,
            )
        )
    return _Lock(path, tuple(parsed))


def _local_source(source: Mapping[str, Any] | None) -> bool:
    if source is None:
        return False
    return any(_text(source.get(key)) is not None for key in ("editable", "directory", "virtual", "path"))


def _lock_refs(
    value: object, path: str, issues: list[Issue]
) -> tuple[_LockedRef, ...]:
    items = _list(value)
    if items is None:
        return ()
    refs: list[_LockedRef] = []
    for item in items:
        table = _table(item)
        name = _text(table.get("name")) if table is not None else None
        if table is None or name is None:
            issues.append(Issue(CoverageKind.PARSE_ERROR, "lock dependency is malformed", path=path))
            continue
        refs.append(
            _LockedRef(
                name=name,
                version=_text(table.get("version")),
                marker=_text(table.get("marker")),
                source=_table(table.get("source")),
            )
        )
    return tuple(refs)


def _lock_ref_groups(
    value: object, path: str, issues: list[Issue]
) -> Mapping[str, tuple[_LockedRef, ...]]:
    groups: dict[str, tuple[_LockedRef, ...]] = {}
    table = _table(value)
    if table is None:
        return groups
    for name, group in table.items():
        groups[name] = _lock_refs(group, path, issues)
    return groups


def _plans(
    manifests: Mapping[str, _Manifest],
    locks: Mapping[str, _Lock],
    requirements: Mapping[str, tuple[_TextRequirement, ...]],
    issues: list[Issue],
) -> list[_ProjectPlan]:
    plans: list[_ProjectPlan] = []
    by_directory: dict[str, int] = {}
    for manifest in sorted(manifests.values(), key=lambda item: item.path):
        plan = _project_plan(manifest, locks, issues)
        by_directory[manifest.directory] = len(plans)
        plans.append(plan)
    for path in sorted(requirements):
        directory = posixpath.dirname(path) or "."
        index = by_directory.get(directory)
        if index is not None and plans[index].manifest is not None:
            existing = plans[index]
            plans[index] = _ProjectPlan(
                existing.manifest,
                existing.project,
                existing.lock,
                existing.own,
                existing.declarations,
                (*existing.text_requirements, *requirements[path]),
                existing.lock_path,
            )
            continue
        lock_path = "uv.lock" if directory == "." else f"{directory}/uv.lock"
        if lock_path not in locks:
            issues.append(
                Issue(CoverageKind.MISSING_LOCK, f"no lockfile at {lock_path}", path=lock_path)
            )
        plan = _ProjectPlan(
            manifest=None,
            project=Project(f"{directory}:pypi", Ecosystem.PYPI, directory),
            lock=None,
            own=(),
            declarations=(),
            text_requirements=requirements[path],
            lock_path=lock_path,
        )
        by_directory[directory] = len(plans)
        plans.append(plan)
    plans.sort(key=lambda plan: plan.project.id)
    return plans


def _project_plan(manifest: _Manifest, locks: Mapping[str, _Lock], issues: list[Issue]) -> _ProjectPlan:
    project = Project(f"{manifest.directory}:pypi", Ecosystem.PYPI, manifest.directory)
    lock_path = _lock_path(manifest)
    lock = locks.get(lock_path)
    if lock is None and manifest.dependencies:
        issues.append(
            Issue(CoverageKind.MISSING_LOCK, f"no lockfile at {lock_path}", path=lock_path)
        )
    owns = _own_packages(lock, manifest) if lock is not None else ()
    if lock is not None and manifest.dependencies and not owns:
        issues.append(
            Issue(
                CoverageKind.LOCK_MISMATCH,
                f"{lock.path} has no package entry for {manifest.name}",
                path=manifest.path,
            )
        )
    declarations: list[_ResolvedRequirement] = []
    for requirement in manifest.dependencies:
        if lock is None:
            source = manifest.sources.get(
                canonicalize_name(requirement.requirement.name)
            )
            declarations.append(
                _ResolvedRequirement(
                    requirement,
                    None,
                    None,
                    None,
                    None,
                    None,
                    _unlocked_resolution(source),
                )
            )
            continue
        if not owns:
            declarations.append(
                _ResolvedRequirement(
                    requirement, None, None, None, None, None, Resolution.UNKNOWN
                )
            )
            continue
        matches: list[_ResolvedRequirement] = []
        for own in owns:
            matched = _match(lock, own, requirement, manifest.sources)
            if matched is None:
                continue
            package, ref, index, group = matched
            matches.append(
                _ResolvedRequirement(
                    requirement, package, own, ref, index, group, Resolution.RESOLVED
                )
            )
        if not matches:
            issues.append(
                Issue(
                    CoverageKind.LOCK_MISMATCH,
                    f"{requirement.requirement.name} is not resolved in {lock.path}",
                    path=manifest.path,
                    locator=requirement.locator,
                )
            )
            declarations.append(
                _ResolvedRequirement(
                    requirement, None, None, None, None, None, Resolution.UNKNOWN
                )
            )
            continue
        declarations.extend(matches)
    return _ProjectPlan(
        manifest=manifest,
        project=project,
        lock=lock,
        own=owns,
        declarations=tuple(declarations),
        text_requirements=(),
        lock_path=lock_path,
    )


def _lock_path(manifest: _Manifest) -> str:
    directory = manifest.workspace or manifest.directory
    return "uv.lock" if directory == "." else f"{directory}/uv.lock"


def _unlocked_resolution(source: _SourceSpec | None) -> Resolution:
    if source is not None:
        if source.shape == "path":
            return Resolution.RESOLVED
        if source.shape == "git" and _git_commit(source) is not None:
            return Resolution.RESOLVED
    return Resolution.DECLARED_ONLY


def _git_commit(source: _SourceSpec) -> str | None:
    _url, fragment = _git_parts(source.value)
    if fragment is not None:
        return fragment
    if source.rev is not None and _SHA.fullmatch(source.rev):
        return source.rev
    return None


def _own_packages(lock: _Lock, manifest: _Manifest) -> tuple[_LockedPackage, ...]:
    if manifest.name is None:
        return ()
    wanted = canonicalize_name(manifest.name)
    candidates = [
        package
        for package in lock.packages
        if canonicalize_name(package.name) == wanted and _local_directory(package, lock.path) is not None
    ]
    if manifest.version is not None:
        exact = [package for package in candidates if package.version == manifest.version]
        if exact:
            return tuple(exact)
        unversioned = [package for package in candidates if package.version is None]
        if unversioned:
            return tuple(unversioned)
    return tuple(sorted(candidates, key=lambda package: package.markers))


def _local_directory(package: _LockedPackage, lock_path: str) -> str | None:
    source = package.source
    if source is None:
        return None
    for key in ("editable", "directory", "virtual", "path"):
        value = _text(source.get(key))
        if value is not None:
            return _normalize_path(posixpath.dirname(lock_path), value)
    return None


def _match(
    lock: _Lock,
    own: _LockedPackage,
    requirement: _Requirement,
    sources: Mapping[str, _SourceSpec],
) -> tuple[_LockedPackage, _LockedRef | None, int | None, tuple[str, str] | None] | None:
    wanted = canonicalize_name(requirement.requirement.name)
    refs: list[tuple[_LockedRef, tuple[str, str] | None, tuple[_LockedRef, ...]]] = []
    if requirement.scope is Scope.OPTIONAL:
        group = _condition_group(requirement.condition, "extra")
        if group is not None and group in own.optional:
            refs.extend(
                (ref, ("optional-dependencies", group), own.optional[group])
                for ref in own.optional[group]
            )
    if requirement.scope is Scope.DEV:
        group = _condition_group(requirement.condition, "group")
        if group is not None and group in own.dev:
            refs.extend(
                (ref, ("dev-dependencies", group), own.dev[group]) for ref in own.dev[group]
            )
    refs.extend((ref, None, own.dependencies) for ref in own.dependencies)
    for ref, group, group_refs in refs:
        if canonicalize_name(ref.name) != wanted:
            continue
        package = _resolve_ref(lock, ref, own.markers)
        if package is not None:
            index = list(group_refs).index(ref)
            return package, ref, index, group
    candidates = [
        package for package in lock.packages if canonicalize_name(package.name) == wanted
    ]
    if not candidates:
        return None
    if own.markers:
        overlapping = [
            package for package in candidates if set(package.markers) & set(own.markers)
        ]
        if overlapping:
            candidates = overlapping
    source = sources.get(wanted)
    if source is not None:
        preferred = [
            package for package in candidates if _shape(package, lock.path) == source.shape
        ]
        if preferred:
            candidates = preferred
    candidates.sort(
        key=lambda package: (
            not _satisfies(package.version, requirement.requirement),
            _version_key(package.version),
        )
    )
    return candidates[0], None, None, None


def _condition_group(condition: str | None, prefix: str) -> str | None:
    if condition is None:
        return None
    if prefix == "extra":
        marker = "extra == '"
        if condition.startswith(marker) and condition.endswith("'"):
            return condition[len(marker) : -1]
        return None
    marker = "group == '"
    if condition.startswith(marker) and condition.endswith("'"):
        return condition[len(marker) : -1]
    return None


def _resolve_ref(
    lock: _Lock, ref: _LockedRef, markers: Sequence[str] = ()
) -> _LockedPackage | None:
    wanted = canonicalize_name(ref.name)
    candidates = [
        package for package in lock.packages if canonicalize_name(package.name) == wanted
    ]
    exact: list[_LockedPackage] = []
    for package in candidates:
        if ref.version is not None and package.version != ref.version:
            continue
        if ref.source is not None and not _source_matches(package.source, ref.source):
            continue
        exact.append(package)
    if markers and len(exact) > 1:
        overlapping = [package for package in exact if set(package.markers) & set(markers)]
        if overlapping:
            exact = overlapping
    return exact[0] if exact else None


def _source_matches(left: Mapping[str, Any] | None, right: Mapping[str, Any] | None) -> bool:
    if left is None or right is None:
        return left is right
    return dict(left) == dict(right)


def _satisfies(version: str | None, requirement: Requirement) -> bool:
    if version is None:
        return True
    try:
        return requirement.specifier.contains(version, prereleases=True)
    except (InvalidVersion, ValueError):
        return True


def _version_key(version: str | None) -> tuple[int, Version | str]:
    if version is None:
        return (2, "")
    try:
        return (0, Version(version))
    except InvalidVersion:
        return (1, version)


def _shape(package: _LockedPackage, lock_path: str) -> SourceShape:
    if _local_directory(package, lock_path) is not None:
        return "path"
    source = package.source or {}
    if _text(source.get("git")) is not None:
        return "git"
    if _text(source.get("url")) is not None:
        return "url"
    return "registry"


def _normalize_path(base: str, value: str) -> str:
    return posixpath.normpath(posixpath.join(base, value))


def _normalize(value: str) -> str:
    return canonicalize_name(value)


def _key(value: str) -> str:
    if _SIMPLE_KEY.fullmatch(value):
        return value
    if "'" not in value:
        return f"'{value}'"
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def _instances(
    locks: Mapping[str, _Lock],
    plans: Sequence[_ProjectPlan],
    manifests: Mapping[str, _Manifest],
    local: Mapping[NormalizedName, _Manifest],
    index: str | None,
    issues: list[Issue],
) -> tuple[dict[tuple[str, str], _Draft], dict[tuple[str, int], tuple[str, str]]]:
    variants = _variant_keys(locks, index)
    drafts: dict[tuple[str, str], _Draft] = {}
    package_keys: dict[tuple[str, int], tuple[str, str]] = {}
    for lock in locks.values():
        for package in lock.packages:
            draft = _lock_draft(package, lock, local, variants, index, issues)
            key = (draft.purl, draft.source_key)
            drafts.setdefault(key, draft)
            package_keys[(lock.path, package.index)] = key
    for plan in plans:
        for item in plan.declarations:
            if item.package is not None:
                continue
            draft = _requirement_draft(item.requirement, plan.manifest, manifests, index)
            drafts.setdefault((draft.purl, draft.source_key), draft)
        for text in plan.text_requirements:
            draft = _text_draft(text)
            drafts.setdefault((draft.purl, draft.source_key), draft)
    return drafts, package_keys


def _variant_keys(locks: Mapping[str, _Lock], index: str | None) -> dict[tuple[str, int], str]:
    groups: dict[tuple[NormalizedName, str | None, str], set[tuple[str, ...]]] = {}
    for lock in locks.values():
        for package in lock.packages:
            identity, _source = _package_identity(package, lock.path, index)
            key = (canonicalize_name(package.name), package.version, identity)
            if key not in groups:
                groups[key] = set()
            groups[key].add(package.markers)
    keys: dict[tuple[str, int], str] = {}
    for lock in locks.values():
        for package in lock.packages:
            identity, _source = _package_identity(package, lock.path, index)
            group = (canonicalize_name(package.name), package.version, identity)
            if len(groups[group]) > 1:
                digest = hashlib.sha256("\0".join(package.markers).encode()).hexdigest()[:16]
                keys[(lock.path, package.index)] = f"{identity}|{digest}"
            else:
                keys[(lock.path, package.index)] = identity
    return keys


def _package_identity(
    package: _LockedPackage, lock_path: str, index: str | None
) -> tuple[str, Mapping[str, Any] | None]:
    directory = _local_directory(package, lock_path)
    if directory is not None:
        return f"path:{directory}", {"path": directory}
    source = package.source or {}
    git = _text(source.get("git"))
    if git is not None:
        url, commit = _git_parts(git)
        return f"git:{url}:{commit or ''}", {"git": git}
    url = _text(source.get("url"))
    if url is not None:
        return f"url:{redact_url(url, drop_query_and_fragment=True)}", {"url": url}
    registry = _text(source.get("registry"))
    if registry is not None:
        if _public_registry(registry, index):
            return "pypi:public", {"registry": registry}
        return f"registry:{redact_url(registry, drop_query_and_fragment=True)}", {"registry": registry}
    return f"unknown:{package.source!r}", package.source


def _git_parts(value: str) -> tuple[str, str | None]:
    base, _, fragment = value.partition("#")
    url = redact_url(base.split("?", 1)[0], drop_query_and_fragment=True)
    commit = fragment if _SHA.fullmatch(fragment) else None
    return url, commit


def _lock_draft(
    package: _LockedPackage,
    lock: _Lock,
    local: Mapping[NormalizedName, _Manifest],
    variants: Mapping[tuple[str, int], str],
    index: str | None,
    issues: list[Issue],
) -> _Draft:
    source_key = variants[(lock.path, package.index)]
    directory = _local_directory(package, lock.path)
    if directory is not None:
        manifest = local.get(canonicalize_name(package.name))
        inside = _inside(directory)
        kind = (
            SourceKind.WORKSPACE
            if manifest is not None and manifest.workspace is not None
            else SourceKind.PATH
        )
        return _Draft(
            name=package.name,
            display=package.name,
            version=package.version,
            ecosystem=Ecosystem.GENERIC,
            source_kind=kind,
            source_key=source_key,
            directory=directory,
            first_party=inside,
        )
    source = package.source or {}
    git = _text(source.get("git"))
    if git is not None:
        url, commit = _git_parts(git)
        return _Draft(
            name=canonicalize_name(package.name),
            display=package.name,
            version=package.version,
            ecosystem=Ecosystem.PYPI,
            source_kind=SourceKind.GIT,
            source_key=source_key,
            vcs_url=url,
            commit=commit,
            qualifiers=(("vcs_url", url),),
        )
    download = _text(source.get("url"))
    if download is not None:
        url = redact_url(download, drop_query_and_fragment=True)
        return _Draft(
            name=canonicalize_name(package.name),
            display=package.name,
            version=package.version,
            ecosystem=Ecosystem.PYPI,
            source_kind=SourceKind.URL,
            source_key=source_key,
            qualifiers=(("download_url", url),),
        )
    registry = _text(source.get("registry"))
    if registry is not None:
        url = redact_url(registry, drop_query_and_fragment=True)
        public = _public_registry(url, index)
        return _Draft(
            name=canonicalize_name(package.name),
            display=package.name,
            version=package.version,
            ecosystem=Ecosystem.PYPI,
            source_kind=SourceKind.REGISTRY,
            source_key=source_key,
            registry_url=url,
            repository_url=url if not public else None,
            qualifiers=() if public else (("repository_url", url),),
        )
    issues.append(
        Issue(
            CoverageKind.DYNAMIC_VALUE,
            f"unsupported uv.lock source for {package.name}",
            path=lock.path,
        )
    )
    return _Draft(
        name=canonicalize_name(package.name),
        display=package.name,
        version=package.version,
        ecosystem=Ecosystem.PYPI,
        source_kind=SourceKind.REGISTRY,
        source_key=source_key,
    )


def _inside(directory: str) -> bool:
    return directory == "." or not directory.startswith(("../", "/"))


def _public_registry(url: str, index: str | None) -> bool:
    normalized = url.rstrip("/")
    if normalized in _PUBLIC_PYPI:
        return True
    return index is not None and normalized == index.rstrip("/")


def _requirement_draft(
    requirement: _Requirement,
    manifest: _Manifest | None,
    manifests: Mapping[str, _Manifest],
    index: str | None,
) -> _Draft:
    name = canonicalize_name(requirement.requirement.name)
    display = requirement.requirement.name
    source = manifest.sources.get(name) if manifest is not None else None
    directory = manifest.directory if manifest is not None else "."
    if source is not None and source.shape == "path":
        if source.value == "workspace":
            target = next(
                (item for item in manifests.values() if item.name is not None and canonicalize_name(item.name) == name),
                None,
            )
            target_path = target.directory if target is not None else directory
        else:
            target_path = _normalize_path(source.base, source.value)
            target = next(
                (item for item in manifests.values() if item.directory == target_path), None
            )
        inside = _inside(target_path)
        kind = (
            SourceKind.WORKSPACE
            if target is not None and target.workspace is not None
            else SourceKind.PATH
        )
        return _Draft(
            name=display,
            display=display,
            version=target.version if target is not None else None,
            ecosystem=Ecosystem.GENERIC,
            source_kind=SourceKind.VENDORED if requirement.vendored else kind,
            source_key=(
                f"vendored:{_normalize(display)}"
                if requirement.vendored
                else f"{kind.value}:{target_path}"
            ),
            directory=target_path if inside else None,
            first_party=inside,
        )
    if source is not None and source.shape == "git":
        url, fragment = _git_parts(source.value)
        commit = fragment or _git_commit(source)
        return _Draft(
            name=name,
            display=display,
            version=None,
            ecosystem=Ecosystem.PYPI,
            source_kind=SourceKind.VENDORED if requirement.vendored else SourceKind.GIT,
            source_key=(
                f"vendored:{_normalize(display)}"
                if requirement.vendored
                else f"git:{url}:{commit or ''}"
            ),
            vcs_url=url,
            commit=commit,
            qualifiers=(("vcs_url", url),),
        )
    if source is not None and source.shape == "url":
        url = redact_url(source.value, drop_query_and_fragment=True)
        return _Draft(
            name=name,
            display=display,
            version=None,
            ecosystem=Ecosystem.PYPI,
            source_kind=SourceKind.VENDORED if requirement.vendored else SourceKind.URL,
            source_key=(
                f"vendored:{_normalize(display)}" if requirement.vendored else f"url:{url}"
            ),
            qualifiers=(("download_url", url),),
        )
    direct = requirement.requirement.url
    if direct is not None:
        url = redact_url(direct, drop_query_and_fragment=True)
        return _Draft(
            name=name,
            display=display,
            version=None,
            ecosystem=Ecosystem.PYPI,
            source_kind=SourceKind.VENDORED if requirement.vendored else SourceKind.URL,
            source_key=(
                f"vendored:{_normalize(display)}" if requirement.vendored else f"url:{url}"
            ),
            qualifiers=(("download_url", url),),
        )
    return _Draft(
        name=name,
        display=display,
        version=None,
        ecosystem=Ecosystem.PYPI,
        source_kind=SourceKind.VENDORED if requirement.vendored else SourceKind.REGISTRY,
        source_key=(
            f"vendored:{_normalize(display)}"
            if requirement.vendored
            else "pypi:public"
        ),
    )


def _text_draft(text: _TextRequirement) -> _Draft:
    name = canonicalize_name(text.requirement.name)
    display = text.requirement.name
    return _Draft(
        name=name,
        display=display,
        version=None,
        ecosystem=Ecosystem.PYPI,
        source_kind=SourceKind.VENDORED if text.vendored else SourceKind.REGISTRY,
        source_key=f"vendored:{_normalize(display)}" if text.vendored else "pypi:public",
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
    package_keys: Mapping[tuple[str, int], tuple[str, str]],
    issues: list[Issue],
) -> list[Edge]:
    edges: list[Edge] = []
    for item in plan.declarations:
        requirement = item.requirement
        condition = _condition(requirement, item.ref, item.own)
        if item.package is not None and plan.lock is not None:
            key = package_keys[(plan.lock.path, item.package.index)]
            target = drafts[key]
            resolution = Resolution.CONDITIONAL if condition is not None else Resolution.RESOLVED
            evidence = [Evidence(plan.manifest.path, requirement.locator)] if plan.manifest else []
            own = item.own
            if item.reference_index is not None and own is not None:
                group = item.reference_group
                prefix = f"package[name={own.name},version={own.version}]"
                locator = (
                    f"{prefix}.dependencies[{item.reference_index}]"
                    if group is None
                    else f"{prefix}.{group[0]}.{group[1]}[{item.reference_index}]"
                )
                evidence.append(Evidence(plan.lock.path, locator))
        else:
            target = _requirement_draft(requirement, plan.manifest, manifests, None)
            resolution = item.resolution
            evidence = [Evidence(plan.manifest.path, requirement.locator)] if plan.manifest else []
        edges.append(
            Edge(
                from_id=plan.project.id,
                to_id=instance_ids[(target.purl, target.source_key)],
                relation=Relation.DIRECT,
                scope=requirement.scope,
                optional=requirement.optional,
                constraint=str(requirement.requirement.specifier) or None,
                resolved_version=target.version,
                condition=condition,
                source_kind=SourceKind.VENDORED if requirement.vendored else target.source_kind,
                resolution=resolution,
                evidence=tuple(evidence),
            )
        )
    for text in plan.text_requirements:
        draft = _text_draft(text)
        edges.append(
            Edge(
                from_id=plan.project.id,
                to_id=instance_ids[(draft.purl, draft.source_key)],
                relation=Relation.DIRECT,
                scope=Scope.RUNTIME,
                optional=False,
                constraint=str(text.requirement.specifier) or None,
                resolved_version=None,
                condition=None,
                source_kind=draft.source_kind,
                resolution=Resolution.DECLARED_ONLY,
                evidence=(Evidence(text.path, text.locator),),
            )
        )
    return edges


def _condition(
    requirement: _Requirement, ref: _LockedRef | None, own: _LockedPackage | None
) -> str | None:
    parts: list[str] = []
    marker = (
        str(requirement.requirement.marker)
        if requirement.requirement.marker is not None
        else None
    )
    if marker is not None:
        parts.append(marker)
    if requirement.condition is not None:
        parts.append(requirement.condition)
    if own is not None and own.markers:
        normalized = [_normalize_marker(item) or item for item in own.markers]
        parts.append(" or ".join(normalized))
    if ref is not None and ref.marker is not None:
        parts.append(_normalize_marker(ref.marker) or ref.marker)
    return "; ".join(dict.fromkeys(parts)) or None


def _normalize_marker(value: str) -> str | None:
    try:
        return str(Marker(value))
    except InvalidMarker:
        return None


def _transitive_edges(
    locks: Mapping[str, _Lock],
    drafts: Mapping[tuple[str, str], _Draft],
    instance_ids: Mapping[tuple[str, str], str],
    package_keys: Mapping[tuple[str, int], tuple[str, str]],
    issues: list[Issue],
) -> list[Edge]:
    edges: list[Edge] = []
    for lock in locks.values():
        for package in lock.packages:
            if _local_directory(package, lock.path) is not None:
                continue
            key = package_keys[(lock.path, package.index)]
            origin_id = instance_ids[key]
            groups: list[
                tuple[tuple[str, str] | None, str | None, Scope, bool, tuple[_LockedRef, ...]]
            ] = [(None, None, Scope.RUNTIME, False, package.dependencies)]
            for group, refs in package.optional.items():
                groups.append(
                    (
                        ("optional-dependencies", group),
                        f"extra == '{group}'",
                        Scope.OPTIONAL,
                        True,
                        refs,
                    )
                )
            for refs_group, extra_condition, scope, optional, refs in groups:
                for index, ref in enumerate(refs):
                    target = _resolve_ref(lock, ref)
                    if target is None:
                        issues.append(
                            Issue(
                                CoverageKind.PARSE_ERROR,
                                f"lock dependency {ref.name} does not resolve in {lock.path}",
                                path=lock.path,
                                locator=_ref_locator(package, refs_group, index),
                            )
                        )
                        continue
                    target_key = package_keys[(lock.path, target.index)]
                    draft = drafts[target_key]
                    parts = [item for item in (extra_condition, ref.marker) if item is not None]
                    condition = "; ".join(dict.fromkeys(parts)) or None
                    edges.append(
                        Edge(
                            from_id=origin_id,
                            to_id=instance_ids[target_key],
                            relation=Relation.TRANSITIVE,
                            scope=scope,
                            optional=optional,
                            constraint=None,
                            resolved_version=target.version,
                            condition=condition,
                            source_kind=draft.source_kind,
                            resolution=Resolution.CONDITIONAL
                            if condition is not None
                            else Resolution.RESOLVED,
                            evidence=(Evidence(lock.path, _ref_locator(package, refs_group, index)),),
                        )
                    )
    return edges


def _ref_locator(
    package: _LockedPackage, group: tuple[str, str] | None, index: int
) -> str:
    prefix = f"package[name={package.name},version={package.version}]"
    if group is None:
        return f"{prefix}.dependencies[{index}]"
    return f"{prefix}.{group[0]}.{group[1]}[{index}]"


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


@cache
def _default_index() -> str | None:
    path = Path(__file__).resolve().parents[4] / "registries.toml"
    try:
        document = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        raise ValueError(f"cannot read {path}: {error}") from error
    python = document.get("python")
    if not isinstance(python, dict):
        return None
    index = python.get("index")
    return index if isinstance(index, str) else None
