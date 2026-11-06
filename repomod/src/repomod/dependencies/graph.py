from __future__ import annotations

import json
import tomllib
from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from functools import cache
from pathlib import Path
from typing import Any, cast
from urllib.parse import urlsplit

from packaging.utils import canonicalize_name

from repomod.catalog import Catalog
from repomod.dependencies.model import (
    Component,
    CoverageKind,
    Ecosystem,
    Edge,
    Evidence,
    IndexEntry,
    InputFile,
    Instance,
    Issue,
    Project,
    Relation,
    RepositoryData,
    RepositoryState,
    Resolution,
    Scope,
    SourceKind,
)

_ECOSYSTEMS = {ecosystem.value: ecosystem for ecosystem in Ecosystem}


@dataclass(frozen=True, slots=True)
class Inventory:
    path: Path
    repositories: Mapping[str, RepositoryData]
    entries: Mapping[str, IndexEntry]


type NodeKey = tuple[str, str]


@dataclass(frozen=True, slots=True)
class RepositoryEdge:
    repository: str
    edge: Edge


@dataclass(frozen=True, slots=True)
class Graph:
    repositories: Mapping[str, RepositoryData]
    catalog: Catalog | None
    projects: Mapping[NodeKey, Project]
    instances: Mapping[str, Instance]
    instance_repositories: Mapping[str, frozenset[str]]
    components: Mapping[str, Component]
    edges: tuple[RepositoryEdge, ...]
    outgoing: Mapping[NodeKey, tuple[Edge, ...]]
    incoming: Mapping[NodeKey, tuple[Edge, ...]]
    tags: Mapping[str, tuple[str, ...]]
    first_party_names: Mapping[Ecosystem, frozenset[str]]
    native: frozenset[str]
    catalogued: frozenset[str]

    def label(self, node: str) -> str:
        instance = self.instances.get(node)
        if instance is None:
            return node
        component = self.components.get(instance.component)
        name = component.display_name if component is not None else instance.component
        return f"{name}@{instance.version}" if instance.version is not None else name


@dataclass(frozen=True, slots=True)
class PackageQuery:
    text: str
    ecosystem: Ecosystem | None
    name: str
    version: str | None


@dataclass(frozen=True, slots=True)
class WhyPath:
    project: str
    repository: str
    target: str
    nodes: tuple[str, ...]
    edges: tuple[Edge, ...]


@dataclass(frozen=True, slots=True)
class WhyResult:
    package: PackageQuery
    matched: tuple[str, ...]
    paths: tuple[WhyPath, ...]
    truncated: bool


@dataclass(frozen=True, slots=True)
class Consumer:
    repository: str
    source: str
    project: str | None
    edge: Edge

    @property
    def direct(self) -> bool:
        return self.project is not None


def load_inventory(inventory: Path) -> Inventory:
    index_path = inventory / "index.json"
    try:
        document = json.loads(index_path.read_text(encoding="utf-8"))
    except FileNotFoundError as error:
        raise ValueError(f"dependency inventory not found: {index_path}") from error
    entries = parse_index(document)
    repositories: dict[str, RepositoryData] = {}
    for name in entries:
        path = _repository_file(inventory, name)
        try:
            content = json.loads(path.read_text(encoding="utf-8"))
        except FileNotFoundError as error:
            raise ValueError(f"dependency inventory file is missing: {path}") from error
        repositories[name] = parse_repository(content)
    return Inventory(inventory, repositories, entries)


def parse_index(document: object) -> Mapping[str, IndexEntry]:
    if not isinstance(document, dict):
        raise ValueError("cannot read dependency inventory index")
    index = cast(Mapping[str, Any], document)
    if index.get("schema") != 1:
        raise ValueError("cannot read dependency inventory index")
    raw_repositories = index.get("repositories")
    if not isinstance(raw_repositories, dict):
        raise ValueError("dependency inventory index has no repositories")
    entries: dict[str, IndexEntry] = {}
    for name, raw in raw_repositories.items():
        if not isinstance(name, str) or not isinstance(raw, dict):
            raise ValueError("dependency inventory index is malformed")
        entries[name] = _entry(cast(Mapping[str, Any], raw))
    return entries


def parse_repository(document: object) -> RepositoryData:
    if not isinstance(document, dict):
        raise ValueError("dependency inventory file is malformed")
    return _repository_data(cast(Mapping[str, Any], document))


def _repository_file(inventory: Path, repository: str) -> Path:
    owner, _, name = repository.partition("/")
    return inventory / "repos" / owner / f"{name}.json"


def _entry(value: Mapping[str, Any]) -> IndexEntry:
    commit = value.get("commit")
    fingerprint = value.get("inputs_fingerprint")
    state = value.get("state")
    if (
        (commit is not None and not isinstance(commit, str))
        or not isinstance(fingerprint, str)
        or not isinstance(state, str)
    ):
        raise ValueError("dependency inventory index entry is malformed")
    return IndexEntry(commit, fingerprint, RepositoryState(state))


def _repository_data(value: Mapping[str, Any]) -> RepositoryData:
    tags = value.get("tags")
    inputs = value.get("inputs")
    fingerprint = value.get("inputs_fingerprint")
    if (
        not isinstance(tags, list)
        or not all(isinstance(item, str) for item in tags)
        or not isinstance(inputs, list)
        or not isinstance(fingerprint, str)
    ):
        raise ValueError("dependency inventory repository file is malformed")
    return RepositoryData(
        repository=_text(value, "repository"),
        tags=tuple(cast(list[str], tags)),
        inputs=tuple(_input(item) for item in inputs),
        inputs_fingerprint=fingerprint,
        projects=tuple(_project(item) for item in _list(value.get("projects"))),
        components=tuple(_component(item) for item in _list(value.get("components"))),
        instances=tuple(_instance(item) for item in _list(value.get("instances"))),
        edges=tuple(_edge(item) for item in _list(value.get("edges"))),
        issues=tuple(_issue(item) for item in _list(value.get("issues"))),
    )


def _table(value: object) -> Mapping[str, Any]:
    if not isinstance(value, dict):
        raise ValueError("dependency inventory record is not an object")
    return cast(Mapping[str, Any], value)


def _list(value: object) -> list[Any]:
    if value is None:
        return []
    if not isinstance(value, list):
        raise ValueError("dependency inventory record is not a list")
    return value


def _text(value: Mapping[str, Any], key: str) -> str:
    item = value.get(key)
    if not isinstance(item, str):
        raise ValueError(f"dependency inventory field {key} is malformed")
    return item


def _optional_text(value: Mapping[str, Any], key: str) -> str | None:
    item = value.get(key)
    if item is not None and not isinstance(item, str):
        raise ValueError(f"dependency inventory field {key} is malformed")
    return item


def _bool(value: Mapping[str, Any], key: str) -> bool:
    item = value.get(key)
    if not isinstance(item, bool):
        raise ValueError(f"dependency inventory field {key} is malformed")
    return item


def _input(value: object) -> InputFile:
    table = _table(value)
    return InputFile(_text(table, "path"), _text(table, "blob"))


def _project(value: object) -> Project:
    table = _table(value)
    return Project(_text(table, "id"), _ecosystem(_text(table, "ecosystem")), _text(table, "directory"))


def _component(value: object) -> Component:
    table = _table(value)
    return Component(
        _text(table, "purl"),
        _ecosystem(_text(table, "ecosystem")),
        _text(table, "name"),
        _text(table, "display_name"),
    )


def _instance(value: object) -> Instance:
    table = _table(value)
    return Instance(
        id=_text(table, "id"),
        purl=_text(table, "purl"),
        component=_text(table, "component"),
        version=_optional_text(table, "version"),
        source_kind=SourceKind(_text(table, "source_kind")),
        context=_text(table, "context"),
        registry_url=_optional_text(table, "registry_url"),
        repository_url=_optional_text(table, "repository_url"),
        vcs_url=_optional_text(table, "vcs_url"),
        commit=_optional_text(table, "commit"),
        first_party=_bool(table, "first_party"),
    )


def _edge(value: object) -> Edge:
    table = _table(value)
    return Edge(
        from_id=_text(table, "from"),
        to_id=_text(table, "to"),
        relation=Relation(_text(table, "relation")),
        scope=Scope(_text(table, "scope")),
        optional=_bool(table, "optional"),
        constraint=_optional_text(table, "constraint"),
        resolved_version=_optional_text(table, "resolved_version"),
        condition=_optional_text(table, "condition"),
        source_kind=SourceKind(_text(table, "source_kind")),
        resolution=Resolution(_text(table, "resolution")),
        evidence=tuple(_evidence(item) for item in _list(table.get("evidence"))),
    )


def _evidence(value: object) -> Evidence:
    table = _table(value)
    return Evidence(_text(table, "path"), _text(table, "locator"))


def _issue(value: object) -> Issue:
    table = _table(value)
    return Issue(
        CoverageKind(_text(table, "kind")),
        _text(table, "message"),
        path=_optional_text(table, "path"),
        locator=_optional_text(table, "locator"),
    )


def _ecosystem(value: str) -> Ecosystem:
    ecosystem = _ECOSYSTEMS.get(value)
    if ecosystem is None:
        raise ValueError(f"unknown ecosystem: {value}")
    return ecosystem


def build_graph(inventory: Inventory, catalog: Catalog | None = None) -> Graph:
    projects: dict[NodeKey, Project] = {}
    instances: dict[str, Instance] = {}
    instance_repositories: dict[str, set[str]] = {}
    components: dict[str, Component] = {}
    edges: list[RepositoryEdge] = []
    outgoing: dict[NodeKey, list[Edge]] = {}
    incoming: dict[NodeKey, list[Edge]] = {}
    tags: dict[str, tuple[str, ...]] = {}
    names: dict[Ecosystem, set[str]] = {ecosystem: set() for ecosystem in Ecosystem}
    for name, data in inventory.repositories.items():
        tags[name] = data.tags
        for project in data.projects:
            projects[(name, project.id)] = project
        for component in data.components:
            components[component.purl] = component
        for instance in data.instances:
            instances.setdefault(instance.id, instance)
            instance_repositories.setdefault(instance.id, set()).add(name)
        for edge in data.edges:
            edges.append(RepositoryEdge(name, edge))
            outgoing.setdefault((name, edge.from_id), []).append(edge)
            incoming.setdefault((name, edge.to_id), []).append(edge)
    catalog_by_id = (
        {str(repository.id).casefold(): repository for repository in catalog.repositories}
        if catalog is not None
        else {}
    )
    for name, data in inventory.repositories.items():
        effective = catalog_by_id.get(name.casefold())
        effective_tags = effective.tags if effective is not None else data.tags
        tags[name] = effective_tags
        if "crates-io" in effective_tags:
            for instance in data.instances:
                if instance.source_kind in {SourceKind.WORKSPACE, SourceKind.PATH}:
                    names[Ecosystem.CARGO].add(
                        _normalize_name(Ecosystem.CARGO, _component_name(components, instance))
                    )
        if "pypi" in effective_tags:
            for instance in data.instances:
                if instance.source_kind in {SourceKind.WORKSPACE, SourceKind.PATH}:
                    names[Ecosystem.PYPI].add(
                        _normalize_name(Ecosystem.PYPI, _component_name(components, instance))
                    )
    return Graph(
        repositories=inventory.repositories,
        catalog=catalog,
        projects=projects,
        instances=instances,
        instance_repositories={
            key: frozenset(values) for key, values in instance_repositories.items()
        },
        components=components,
        edges=tuple(edges),
        outgoing={key: tuple(values) for key, values in outgoing.items()},
        incoming={key: tuple(values) for key, values in incoming.items()},
        tags=tags,
        first_party_names={ecosystem: frozenset(values) for ecosystem, values in names.items()},
        native=_native_registry(),
        catalogued=frozenset(inventory.repositories) | frozenset(catalog_by_id),
    )


def _component_name(components: Mapping[str, Component], instance: Instance) -> str:
    component = components.get(instance.component)
    return component.name if component is not None else instance.component


def parse_query(value: str) -> PackageQuery:
    text = value.strip()
    if not text:
        raise ValueError("package is empty")
    ecosystem: Ecosystem | None = None
    if ":" in text:
        prefix, _, rest = text.partition(":")
        candidate = _ECOSYSTEMS.get(prefix)
        if candidate is not None and rest:
            ecosystem = candidate
            text = rest
    name, version = _split_version(text)
    if not name:
        raise ValueError(f"cannot parse package: {value!r}")
    return PackageQuery(value, ecosystem, name, version)


def _split_version(text: str) -> tuple[str, str | None]:
    if text.startswith("@"):
        index = text.find("@", 1)
        if index < 0:
            return text, None
        return text[:index], text[index + 1 :] or None
    if "@" in text:
        name, _, version = text.rpartition("@")
        return name, version or None
    return text, None


def matching_instances(graph: Graph, query: PackageQuery) -> tuple[str, ...]:
    matches: list[str] = []
    for instance_id, instance in graph.instances.items():
        component = graph.components.get(instance.component)
        if component is None:
            continue
        if query.ecosystem is not None and component.ecosystem is not query.ecosystem:
            continue
        if _normalize_name(component.ecosystem, component.name) != _normalize_name(
            component.ecosystem, query.name
        ) and _normalize_name(component.ecosystem, component.display_name) != _normalize_name(
            component.ecosystem, query.name
        ):
            continue
        if query.version is not None and instance.version != query.version:
            continue
        matches.append(instance_id)
    return tuple(sorted(set(matches)))


def _normalize_name(ecosystem: Ecosystem, name: str) -> str:
    if ecosystem is Ecosystem.CARGO:
        return name.replace("_", "-").casefold()
    if ecosystem is Ecosystem.PYPI:
        return canonicalize_name(name)
    if ecosystem in {Ecosystem.NPM, Ecosystem.GITHUB}:
        return name.casefold()
    return name


def why(
    graph: Graph,
    query: PackageQuery,
    *,
    repositories: Iterable[str] | None = None,
    max_paths: int = 20,
) -> WhyResult:
    if max_paths < 1:
        raise ValueError("max-paths must be at least one")
    selected = _selected(repositories)
    matched = tuple(
        instance_id
        for instance_id in matching_instances(graph, query)
        if selected is None
        or bool(
            {repository.casefold() for repository in _repositories_of(graph, instance_id)}
            & selected
        )
    )
    targets = set(matched)
    if not targets:
        return WhyResult(query, (), (), False)
    paths: list[WhyPath] = []
    truncated = False
    for repository, project_id in sorted(graph.projects):
        if selected is not None and repository.casefold() not in selected:
            continue
        found, capped = _paths_from(graph, project_id, repository, targets, max_paths)
        paths.extend(found)
        truncated = truncated or capped
    paths.sort(key=lambda item: (len(item.nodes), item.nodes))
    return WhyResult(query, matched, tuple(paths), truncated)


def _selected(repositories: Iterable[str] | None) -> set[str] | None:
    if repositories is None:
        return None
    return {repository.casefold() for repository in repositories}


def _repositories_of(graph: Graph, instance_id: str) -> frozenset[str]:
    return graph.instance_repositories.get(instance_id, frozenset[str]())


def _paths_from(
    graph: Graph, project: str, repository: str, targets: set[str], cap: int
) -> tuple[list[WhyPath], bool]:
    found: list[WhyPath] = []
    truncated = False
    frontier: list[tuple[str, tuple[str, ...], tuple[Edge, ...]]] = [(project, (project,), ())]
    explored = 0
    while frontier:
        next_frontier: list[tuple[str, tuple[str, ...], tuple[Edge, ...]]] = []
        for node, nodes, edges in sorted(frontier, key=lambda item: item[1]):
            for edge in graph.outgoing.get((repository, node), ()):
                if edge.to_id in nodes:
                    continue
                new_nodes = (*nodes, edge.to_id)
                new_edges = (*edges, edge)
                if edge.to_id in targets:
                    if len(found) < cap:
                        found.append(
                            WhyPath(project, repository, edge.to_id, new_nodes, new_edges)
                        )
                    else:
                        truncated = True
                else:
                    explored += 1
                    if explored > 200_000:
                        return found, True
                    next_frontier.append((edge.to_id, new_nodes, new_edges))
        frontier = next_frontier
    return found, truncated


def consumers(graph: Graph, query: PackageQuery) -> tuple[Consumer, ...]:
    consumers: list[Consumer] = []
    seen: set[tuple[str, str, str]] = set()
    for instance_id in matching_instances(graph, query):
        for repository in _repositories_of(graph, instance_id):
            for edge in graph.incoming.get((repository, instance_id), ()):
                key = (repository, edge.from_id, edge.to_id)
                if key in seen:
                    continue
                seen.add(key)
                project = edge.from_id if (repository, edge.from_id) in graph.projects else None
                consumers.append(Consumer(repository, edge.from_id, project, edge))
    return tuple(sorted(consumers, key=lambda item: (item.repository, item.source, item.edge.to_id)))


def is_first_party(graph: Graph, instance: Instance) -> bool:
    if instance.first_party:
        return True
    if instance.vcs_url is not None and _catalogued(graph, instance.vcs_url):
        return True
    component = graph.components.get(instance.component)
    if component is None or instance.source_kind is not SourceKind.REGISTRY:
        return False
    if (
        component.ecosystem is Ecosystem.CARGO
        and instance.repository_url is not None
        and _normalize_registry(instance.repository_url) in graph.native
    ):
        return True
    return _normalize_name(component.ecosystem, component.name) in graph.first_party_names.get(
        component.ecosystem, frozenset()
    )


def is_first_party_edge(graph: Graph, edge: Edge) -> bool:
    instance = graph.instances.get(edge.to_id)
    return instance is not None and is_first_party(graph, instance)


def _catalogued(graph: Graph, url: str) -> bool:
    repo = _github_repo(url)
    if repo is None:
        return False
    return repo.casefold() in graph.catalogued


def _github_repo(url: str) -> str | None:
    value = url.strip()
    try:
        if value.startswith("git@github.com:"):
            path = value.removeprefix("git@github.com:")
        elif value.startswith(("ssh://", "https://", "http://")):
            parsed = urlsplit(value)
            if parsed.hostname is None or parsed.hostname.casefold() != "github.com":
                return None
            path = parsed.path.lstrip("/")
        else:
            return None
    except ValueError:
        return None
    path = path.removesuffix(".git").removesuffix("/")
    parts = path.split("/")
    if len(parts) != 2 or not all(parts):
        return None
    return f"{parts[0]}/{parts[1]}"


def _normalize_registry(value: str) -> str:
    return value.removeprefix("sparse+").removeprefix("registry+").rstrip("/")


@cache
def _native_registry() -> frozenset[str]:
    path = Path(__file__).resolve().parents[3] / "registries.toml"
    try:
        document = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError):
        return frozenset[str]()
    cargo = document.get("cargo")
    if not isinstance(cargo, dict):
        return frozenset[str]()
    native = cargo.get("native")
    if not isinstance(native, str):
        return frozenset[str]()
    return frozenset({_normalize_registry(native)})


def label(graph: Graph, node: str) -> str:
    instance = graph.instances.get(node)
    if instance is None:
        return node
    component = graph.components.get(instance.component)
    name = component.display_name if component is not None else instance.component
    return f"{name}@{instance.version}" if instance.version is not None else name
