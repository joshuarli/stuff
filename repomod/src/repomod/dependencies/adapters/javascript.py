from __future__ import annotations

import hashlib
import json
import posixpath
import tomllib
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from functools import cache
from pathlib import Path
from typing import Any, ClassVar, cast

from ruamel.yaml import YAML
from ruamel.yaml.error import YAMLError

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

_UNSUPPORTED = frozenset({"yarn.lock", "bun.lock", "bun.lockb"})
_IMPORTER_SECTIONS: tuple[tuple[str, Scope, bool], ...] = (
    ("dependencies", Scope.RUNTIME, False),
    ("devDependencies", Scope.DEV, False),
    ("optionalDependencies", Scope.OPTIONAL, True),
    ("peerDependencies", Scope.PEER, False),
    ("packageManagerDependencies", Scope.TOOLCHAIN, False),
    ("configDependencies", Scope.TOOLCHAIN, False),
)

type DraftKey = tuple[str, str, str]


@dataclass(frozen=True, slots=True)
class JavaScriptAdapter:
    name: ClassVar[str] = "javascript"
    registry: str | None = None

    def matches(self, path: str) -> bool:
        name = path.rsplit("/", 1)[-1]
        return name in {
            "pnpm-lock.yaml",
            "package-lock.json",
            "yarn.lock",
            "bun.lock",
            "bun.lockb",
        }

    def extract(self, inputs: Sequence[InputContent]) -> AdapterResult:
        issues: list[Issue] = []
        projects: list[Project] = []
        refs: list[_Ref] = []
        edges: list[_EdgeSpec] = []
        registry = self.registry if self.registry is not None else _default_registry()
        for content in inputs:
            name = content.path.rsplit("/", 1)[-1]
            if name in _UNSUPPORTED:
                issues.append(
                    Issue(CoverageKind.UNSUPPORTED_FORMAT, f"no parser for {name}", path=content.path)
                )
                continue
            if name == "package-lock.json":
                lock = _parse_npm(content.path, content.text, content.vendored, registry, issues)
            else:
                lock = _parse_pnpm(content.path, content.text, content.vendored, registry, issues)
            if lock is None:
                continue
            projects.extend(lock.projects)
            refs.extend(lock.refs)
            edges.extend(lock.edges)
        return _assemble(projects, refs, edges, issues)


@dataclass(frozen=True, slots=True)
class _Ref:
    name: str
    version: str | None
    context: str
    source_kind: SourceKind
    source_key: str
    ecosystem: Ecosystem = Ecosystem.NPM
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
    def component(self) -> str:
        return purl(self.ecosystem, self.name, qualifiers=dict(self.qualifiers))

    @property
    def key(self) -> DraftKey:
        return (self.purl, self.source_key, self.context)


@dataclass(frozen=True, slots=True)
class _EdgeSpec:
    source: str
    ref: _Ref
    relation: Relation
    scope: Scope
    optional: bool
    constraint: str | None
    condition: str | None
    resolution: Resolution
    evidence: tuple[Evidence, ...]
    source_ref: _Ref | None = None


@dataclass(frozen=True, slots=True)
class _Lock:
    projects: tuple[Project, ...]
    refs: tuple[_Ref, ...]
    edges: tuple[_EdgeSpec, ...]


@dataclass(frozen=True, slots=True)
class _Dependency:
    name: str
    specifier: str | None
    value: str | None


def _assemble(
    projects: Sequence[Project],
    refs: Sequence[_Ref],
    edges: Sequence[_EdgeSpec],
    issues: Sequence[Issue],
) -> AdapterResult:
    drafts: dict[DraftKey, _Ref] = {}
    for ref in refs:
        drafts.setdefault(ref.key, ref)
    contexts = _final_contexts(drafts)
    ids = {key: f"{ref.purl}#{contexts[key]}" for key, ref in drafts.items()}
    components: dict[str, Component] = {}
    instances: list[Instance] = []
    for key, ref in drafts.items():
        component = Component(ref.component, ref.ecosystem, ref.name, ref.name)
        components.setdefault(component.purl, component)
        instances.append(
            Instance(
                id=ids[key],
                purl=ref.purl,
                component=ref.component,
                version=ref.version,
                source_kind=ref.source_kind,
                context=contexts[key],
                registry_url=ref.registry_url,
                repository_url=ref.repository_url,
                vcs_url=ref.vcs_url,
                commit=ref.commit,
                first_party=ref.first_party,
            )
        )
    final_edges: list[Edge] = []
    for spec in edges:
        key = spec.ref.key
        source = ids[spec.source_ref.key] if spec.source_ref is not None else spec.source
        final_edges.append(
            Edge(
                from_id=source,
                to_id=ids[key],
                relation=spec.relation,
                scope=spec.scope,
                optional=spec.optional,
                constraint=spec.constraint,
                resolved_version=spec.ref.version,
                condition=spec.condition,
                source_kind=spec.ref.source_kind,
                resolution=spec.resolution,
                evidence=spec.evidence,
            )
        )
    unique_projects: dict[str, Project] = {}
    for project in projects:
        unique_projects.setdefault(project.id, project)
    return AdapterResult(
        projects=tuple(unique_projects.values()),
        components=tuple(components.values()),
        instances=tuple(instances),
        edges=tuple(final_edges),
        issues=tuple(issues),
    )


def _final_contexts(drafts: Mapping[DraftKey, _Ref]) -> dict[DraftKey, str]:
    groups: dict[str, list[DraftKey]] = {}
    for key, ref in drafts.items():
        groups.setdefault(ref.purl, []).append(key)
    contexts: dict[DraftKey, str] = {}
    for purl_value, keys in groups.items():
        ordered = sorted(keys, key=lambda item: (item[1], item[2]))
        seen: dict[str, str] = {}
        for key in ordered:
            ref = drafts[key]
            context = ref.context
            if context in seen and seen[context] != ref.source_key:
                digest = hashlib.sha256(
                    f"{purl_value}\0{ref.source_key}\0{ref.context}".encode()
                ).hexdigest()
                context = digest[:16]
            seen[context] = ref.source_key
            contexts[key] = context
    return contexts


def _split_key(value: str) -> tuple[str, str | None, str]:
    base, separator, rest = value.partition("(")
    context = separator + rest if separator else ""
    index = base.rfind("@")
    if index <= 0:
        return base, None, context
    return base[:index], base[index + 1 :], context


def _inside(directory: str) -> bool:
    return directory == "." or not directory.startswith(("../", "/"))


def _normalize(base: str, value: str) -> str:
    return posixpath.normpath(posixpath.join(base, value))


def _scope(value: object) -> Mapping[str, Any]:
    return cast(Mapping[str, Any], value) if isinstance(value, dict) else {}


def _string(value: object) -> str | None:
    return value if isinstance(value, str) and value else None


def _key(value: str) -> str:
    if value and all(character.isalnum() or character in "._-@" for character in value):
        return value
    escaped = value.replace("\\", "\\\\").replace("'", "\\'")
    return f"'{escaped}'"


def _registry_source(
    name: str, version: str | None, resolved: str | None, registry: str | None, vendored: bool
) -> _Ref:
    if resolved is None:
        return _ref(
            name,
            version,
            SourceKind.REGISTRY,
            "npm:public",
            registry_url=registry,
            vendored=vendored,
        )
    if resolved.startswith(("git+", "git://", "github:")):
        url, commit = _git_parts(resolved)
        return _ref(
            name,
            version,
            SourceKind.GIT,
            f"npm:git:{url}:{commit or ''}",
            vcs_url=url,
            commit=commit,
            qualifiers=(("vcs_url", url),),
            vendored=vendored,
        )
    if resolved.startswith(("file:", "./", "../")):
        return _ref(
            name,
            version,
            SourceKind.PATH,
            f"npm:path:{resolved}",
            first_party=False,
            vendored=vendored,
        )
    if not resolved.startswith(("http://", "https://")):
        return _ref(name, version, SourceKind.REGISTRY, "npm:public", registry_url=registry, vendored=vendored)
    mirror = registry.rstrip("/") if registry is not None else None
    normalized = resolved
    if mirror is not None and normalized.startswith(mirror):
        return _ref(
            name,
            version,
            SourceKind.REGISTRY,
            "npm:public",
            registry_url=registry,
            vendored=vendored,
        )
    if normalized.startswith("https://registry.npmjs.org/"):
        return _ref(
            name,
            version,
            SourceKind.REGISTRY,
            "npm:public",
            registry_url="https://registry.npmjs.org/",
            vendored=vendored,
        )
    origin = _origin(resolved)
    return _ref(
        name,
        version,
        SourceKind.REGISTRY,
        f"npm:registry:{origin}",
        registry_url=origin,
        repository_url=origin,
        qualifiers=(("repository_url", origin),),
        vendored=vendored,
    )


def _git_parts(value: str) -> tuple[str, str | None]:
    raw = value.removeprefix("git+")
    base, _, fragment = raw.partition("#")
    commit = fragment if _is_sha(fragment) else None
    url = redact_url(base.split("?", 1)[0], drop_query_and_fragment=True)
    return url, commit


def _is_sha(value: str) -> bool:
    return len(value) == 40 and all(character in "0123456789abcdef" for character in value)


def _origin(value: str) -> str:
    scheme, _, rest = value.partition("://")
    host = rest.split("/", 1)[0]
    return f"{scheme}://{host}/"


def _ref(
    name: str,
    version: str | None,
    source_kind: SourceKind,
    source_key: str,
    *,
    ecosystem: Ecosystem = Ecosystem.NPM,
    context: str = "",
    registry_url: str | None = None,
    repository_url: str | None = None,
    vcs_url: str | None = None,
    commit: str | None = None,
    first_party: bool = False,
    qualifiers: tuple[tuple[str, str], ...] = (),
    vendored: bool = False,
) -> _Ref:
    if vendored:
        return _Ref(
            name=name,
            version=version,
            context=context,
            source_kind=SourceKind.VENDORED,
            source_key=f"npm:vendored:{name}",
            ecosystem=ecosystem,
            first_party=False,
            qualifiers=qualifiers,
        )
    return _Ref(
        name=name,
        version=version,
        context=context,
        source_kind=source_kind,
        source_key=source_key,
        ecosystem=ecosystem,
        registry_url=registry_url,
        repository_url=repository_url,
        vcs_url=vcs_url,
        commit=commit,
        first_party=first_party,
        qualifiers=qualifiers,
    )


def _parse_pnpm(
    path: str,
    text: str,
    vendored: bool,
    registry: str | None,
    issues: list[Issue],
) -> _Lock | None:
    try:
        documents = tuple(
            document
            for document in YAML(typ="base").load_all(text)
            if isinstance(document, dict)
        )
    except YAMLError as error:
        issues.append(Issue(CoverageKind.PARSE_ERROR, f"invalid YAML: {error}", path=path))
        return None
    if not documents:
        issues.append(Issue(CoverageKind.PARSE_ERROR, "pnpm lock is empty", path=path))
        return None
    version = documents[-1].get("lockfileVersion")
    if version is None:
        issues.append(
            Issue(CoverageKind.PARSE_ERROR, "pnpm lock has no lockfileVersion", path=path)
        )
        return None
    if not str(version).startswith("9"):
        issues.append(
            Issue(
                CoverageKind.UNSUPPORTED_FORMAT,
                f"unsupported pnpm lockfileVersion {version}",
                path=path,
            )
        )
        return None
    packages: dict[str, Mapping[str, Any]] = {}
    snapshots: dict[str, Mapping[str, Any]] = {}
    importers: dict[str, dict[str, list[_Dependency]]] = {}
    for document in documents:
        for key, entry in _scope(document.get("packages")).items():
            if isinstance(entry, dict):
                packages[key] = cast(Mapping[str, Any], entry)
        for key, entry in _scope(document.get("snapshots")).items():
            if isinstance(entry, dict):
                snapshots[key] = cast(Mapping[str, Any], entry)
        for key, entry in _scope(document.get("importers")).items():
            if not isinstance(entry, dict):
                continue
            sections = importers.setdefault(key, {})
            for section, _section, _optional in _IMPORTER_SECTIONS:
                table = _scope(entry.get(section))
                if not table:
                    continue
                target = sections.setdefault(section, [])
                for name, raw in table.items():
                    specifier, value = _pnpm_spec(raw)
                    target.append(_Dependency(name, specifier, value))

    directory_base = posixpath.dirname(path) or "."
    projects: list[Project] = []
    refs: list[_Ref] = []
    edges: list[_EdgeSpec] = []
    node_keys = set(snapshots)
    for key in packages:
        node_keys.add(key)
    node_refs: dict[str, _Ref] = {}
    for key in sorted(node_keys):
        name, version, context = _split_key(key)
        if version is None:
            issues.append(
                Issue(
                    CoverageKind.PARSE_ERROR,
                    f"pnpm package key is malformed: {key}",
                    path=path,
                )
            )
            continue
        base = f"{name}@{version}"
        resolution = _scope(packages.get(base, {}).get("resolution"))
        tarball = _string(resolution.get("tarball"))
        if tarball is not None:
            url = redact_url(tarball, drop_query_and_fragment=True)
            ref = _ref(
                name,
                version,
                SourceKind.URL,
                f"npm:url:{url}",
                context=context,
                qualifiers=(("download_url", url),),
                vendored=vendored,
            )
        else:
            ref = _ref(
                name,
                version,
                SourceKind.REGISTRY,
                "npm:public",
                context=context,
                registry_url=registry,
                vendored=vendored,
            )
        node_refs[key] = ref
        refs.append(ref)

    for directory in sorted(importers):
        project_dir = _normalize(directory_base, directory)
        projects.append(Project(f"{project_dir}:npm", Ecosystem.NPM, project_dir))
        project_id = f"{project_dir}:npm"
        for section in importers[directory]:
            for dependency in importers[directory][section]:
                scope, optional = _section_scope(section)
                ref = _resolve_pnpm(
                    dependency,
                    node_refs,
                    project_dir,
                    registry,
                    vendored,
                    issues,
                    path,
                )
                if ref is None:
                    continue
                refs.append(ref)
                edges.append(
                    _EdgeSpec(
                        source=project_id,
                        ref=ref,
                        relation=Relation.DIRECT,
                        scope=scope,
                        optional=optional,
                        constraint=dependency.specifier or dependency.value,
                        condition=None,
                        resolution=Resolution.RESOLVED,
                        evidence=(
                            Evidence(
                                path,
                                f"importers[{_key(directory)}].{section}.{_key(dependency.name)}",
                            ),
                        ),
                    )
                )

    for key in sorted(snapshots):
        ref = node_refs.get(key)
        if ref is None:
            continue
        entry = snapshots[key]
        for section, scope, is_optional in (
            ("dependencies", Scope.RUNTIME, False),
            ("optionalDependencies", Scope.OPTIONAL, True),
        ):
            for name, value in _scope(entry.get(section)).items():
                if not isinstance(value, str):
                    continue
                dependency = _Dependency(name, None, value)
                target = _resolve_pnpm(
                    dependency,
                    node_refs,
                    directory_base,
                    registry,
                    vendored,
                    issues,
                    path,
                )
                if target is None:
                    continue
                refs.append(target)
                edges.append(
                    _EdgeSpec(
                        source="",
                        source_ref=ref,
                        ref=target,
                        relation=Relation.TRANSITIVE,
                        scope=scope,
                        optional=is_optional,
                        constraint=None,
                        condition=None,
                        resolution=Resolution.RESOLVED,
                        evidence=(Evidence(path, f"snapshots[{_key(key)}].{section}.{_key(name)}"),),
                    )
                )
    refs.extend(edge.ref for edge in edges)
    return _Lock(tuple(projects), tuple(refs), tuple(edges))


def _pnpm_spec(raw: object) -> tuple[str | None, str | None]:
    if isinstance(raw, str):
        return raw, raw
    table = _scope(raw)
    return _string(table.get("specifier")), _string(table.get("version"))


def _section_scope(section: str) -> tuple[Scope, bool]:
    for name, scope, optional in _IMPORTER_SECTIONS:
        if name == section:
            return scope, optional
    return Scope.RUNTIME, False


def _resolve_pnpm(
    dependency: _Dependency,
    node_refs: Mapping[str, _Ref],
    base: str,
    registry: str | None,
    vendored: bool,
    issues: list[Issue],
    path: str,
) -> _Ref | None:
    specifier = dependency.specifier or ""
    value = dependency.value or ""
    local = _local_specifier(specifier) or _local_specifier(value)
    if local is not None:
        target = _normalize(base, local)
        return _ref(
            dependency.name,
            None,
            SourceKind.WORKSPACE if _inside(target) else SourceKind.PATH,
            f"npm:path:{target}",
            ecosystem=Ecosystem.GENERIC,
            first_party=_inside(target),
            vendored=vendored,
        )
    if specifier.startswith("workspace:"):
        raw = value.removeprefix("link:")
        target = _normalize(base, raw) if raw and raw != value else _normalize(base, "")
        return _ref(
            dependency.name,
            None,
            SourceKind.WORKSPACE,
            f"npm:path:{target}",
            ecosystem=Ecosystem.GENERIC,
            first_party=_inside(target),
            vendored=vendored,
        )
    if value.startswith(("http://", "https://")):
        url = redact_url(value, drop_query_and_fragment=True)
        return _ref(
            dependency.name,
            None,
            SourceKind.URL,
            f"npm:url:{url}",
            qualifiers=(("download_url", url),),
            vendored=vendored,
        )
    if value.startswith(("git+", "git://")) or specifier.startswith(("git+", "git://", "github:")):
        url, commit = _git_parts(value or specifier)
        return _ref(
            dependency.name,
            None,
            SourceKind.GIT,
            f"npm:git:{url}:{commit or ''}",
            vcs_url=url,
            commit=commit,
            qualifiers=(("vcs_url", url),),
            vendored=vendored,
        )
    key = value if _split_key(value)[1] is not None else f"{dependency.name}@{value}"
    if key in node_refs:
        return node_refs[key]
    name, version, _context = _split_key(key)
    if version is not None:
        base_key = f"{name}@{version}"
        if base_key in node_refs:
            return node_refs[base_key]
    if not value:
        return None
    issues.append(
        Issue(
            CoverageKind.PARSE_ERROR,
            f"pnpm dependency {dependency.name} does not resolve: {value!r}",
            path=path,
        )
    )
    return None


def _local_specifier(value: str) -> str | None:
    for prefix in ("link:", "file:"):
        if value.startswith(prefix):
            return value.removeprefix(prefix)
    return None


def _parse_npm(
    path: str,
    text: str,
    vendored: bool,
    registry: str | None,
    issues: list[Issue],
) -> _Lock | None:
    try:
        document = json.loads(text)
    except json.JSONDecodeError as error:
        issues.append(Issue(CoverageKind.PARSE_ERROR, f"invalid JSON: {error}", path=path))
        return None
    if not isinstance(document, dict):
        issues.append(Issue(CoverageKind.PARSE_ERROR, "package-lock is not an object", path=path))
        return None
    version = document.get("lockfileVersion")
    if version != 3:
        issues.append(
            Issue(
                CoverageKind.UNSUPPORTED_FORMAT,
                f"unsupported package-lock version {version!r}",
                path=path,
            )
        )
        return None
    packages = document.get("packages")
    if not isinstance(packages, dict):
        issues.append(Issue(CoverageKind.PARSE_ERROR, "package-lock has no packages", path=path))
        return None
    entries: dict[str, Mapping[str, Any]] = {
        key: cast(Mapping[str, Any], value)
        for key, value in packages.items()
        if isinstance(key, str) and isinstance(value, dict)
    }
    directory_base = posixpath.dirname(path) or "."
    refs: list[_Ref] = []
    refs_by_path: dict[str, _Ref] = {}
    for name, entry in sorted(entries.items()):
        if name == "":
            continue
        package_name = _npm_name(name, entry)
        if package_name is None:
            continue
        version_value = _string(entry.get("version"))
        linked = entry.get("link") is True
        resolved = _string(entry.get("resolved"))
        if linked:
            target = _normalize(directory_base, resolved) if resolved is not None else _normalize(directory_base, name)
            if version_value is None and resolved is not None:
                linked_entry = entries.get(resolved)
                version_value = _string(linked_entry.get("version")) if linked_entry is not None else None
            inside = _inside(target)
            ref = _ref(
                package_name,
                version_value,
                SourceKind.WORKSPACE if inside else SourceKind.PATH,
                f"npm:path:{target}",
                ecosystem=Ecosystem.GENERIC,
                first_party=inside,
                vendored=vendored,
            )
        else:
            ref = _registry_source(package_name, version_value, resolved, registry, vendored)
        refs.append(ref)
        refs_by_path[name] = ref

    projects: list[Project] = []
    edges: list[_EdgeSpec] = []
    root = entries.get("", {})
    root_dir = directory_base
    projects.append(Project(f"{root_dir}:npm", Ecosystem.NPM, root_dir))
    edges.extend(
        _npm_edges(
            entries,
            refs_by_path,
            root_dir,
            f"{root_dir}:npm",
            root,
            "packages['']",
            path,
            registry,
            vendored,
            issues,
            relation=Relation.DIRECT,
        )
    )
    for name, entry in sorted(entries.items()):
        if name == "" or "node_modules" in name.split("/"):
            continue
        if _npm_name(name, entry) is None:
            continue
        project_dir = _normalize(directory_base, name)
        project_id = f"{project_dir}:npm"
        projects.append(Project(project_id, Ecosystem.NPM, project_dir))
        edges.extend(
            _npm_edges(
                entries,
                refs_by_path,
                name,
                project_id,
                entry,
                f"packages[{_key(name)}]",
                path,
                registry,
                vendored,
                issues,
                relation=Relation.DIRECT,
            )
        )
    for name, entry in sorted(entries.items()):
        if "node_modules" not in name.split("/"):
            continue
        ref = refs_by_path.get(name)
        if ref is None:
            continue
        edges.extend(
            _npm_edges(
                entries,
                refs_by_path,
                name,
                "",
                entry,
                f"packages[{_key(name)}]",
                path,
                registry,
                vendored,
                issues,
                relation=Relation.TRANSITIVE,
                source_ref=ref,
            )
        )
    refs.extend(edge.ref for edge in edges)
    return _Lock(tuple(projects), tuple(refs), tuple(edges))


def _npm_name(path: str, entry: Mapping[str, Any]) -> str | None:
    name = _string(entry.get("name"))
    if name is not None:
        return name
    if path == "":
        return None
    parts = path.split("/")
    if "node_modules" in parts:
        index = len(parts) - 1 - parts[::-1].index("node_modules")
        if index + 1 >= len(parts):
            return None
        if parts[index + 1].startswith("@") and index + 2 < len(parts):
            return f"{parts[index + 1]}/{parts[index + 2]}"
        return parts[index + 1]
    return parts[-1] if parts else None


def _npm_edges(
    entries: Mapping[str, Mapping[str, Any]],
    refs_by_path: Mapping[str, _Ref],
    base: str,
    source: str,
    entry: Mapping[str, Any],
    locator_prefix: str,
    path: str,
    registry: str | None,
    vendored: bool,
    issues: list[Issue],
    *,
    relation: Relation,
    source_ref: _Ref | None = None,
) -> list[_EdgeSpec]:
    edges: list[_EdgeSpec] = []
    sections: tuple[tuple[str, Scope, bool], ...] = (
        ("dependencies", Scope.RUNTIME, False),
        ("devDependencies", Scope.DEV, False),
        ("optionalDependencies", Scope.OPTIONAL, True),
        ("peerDependencies", Scope.PEER, False),
    )
    for section, scope, optional in sections:
        table = _scope(entry.get(section))
        for name, constraint in table.items():
            if not isinstance(constraint, str):
                continue
            target = _resolve_npm(entries, refs_by_path, base, name)
            if target is None:
                if scope is Scope.PEER:
                    target = _ref(
                        name,
                        None,
                        SourceKind.REGISTRY,
                        "npm:public",
                        registry_url=registry,
                        vendored=vendored,
                    )
                    resolution = Resolution.DECLARED_ONLY
                else:
                    issues.append(
                        Issue(
                            CoverageKind.PARSE_ERROR,
                            f"package {name} does not resolve from {base or '.'}",
                            path=path,
                            locator=f"{locator_prefix}.{section}.{_key(name)}",
                        )
                    )
                    target = _ref(
                        name,
                        None,
                        SourceKind.REGISTRY,
                        "npm:public",
                        registry_url=registry,
                        vendored=vendored,
                    )
                    resolution = Resolution.DECLARED_ONLY
            else:
                resolution = Resolution.RESOLVED
            edges.append(
                _EdgeSpec(
                    source=source,
                    source_ref=source_ref,
                    ref=target,
                    relation=relation,
                    scope=scope,
                    optional=optional,
                    constraint=constraint,
                    condition=None,
                    resolution=resolution,
                    evidence=(Evidence(path, f"{locator_prefix}.{section}.{_key(name)}"),),
                )
            )
    return edges


def _resolve_npm(
    entries: Mapping[str, Mapping[str, Any]], refs_by_path: Mapping[str, _Ref], base: str, name: str
) -> _Ref | None:
    current = base
    while True:
        candidate = (
            f"node_modules/{name}"
            if current in {"", "."}
            else f"{current}/node_modules/{name}"
        )
        if candidate in refs_by_path:
            return refs_by_path[candidate]
        if current in {"", "."}:
            return None
        parent = posixpath.dirname(current)
        current = parent or "."


@cache
def _default_registry() -> str | None:
    path = Path(__file__).resolve().parents[4] / "registries.toml"
    try:
        document = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        raise ValueError(f"cannot read {path}: {error}") from error
    npm = document.get("npm")
    if not isinstance(npm, dict):
        return None
    registry = npm.get("registry")
    return registry if isinstance(registry, str) else None
