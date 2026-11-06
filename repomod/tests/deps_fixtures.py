from __future__ import annotations

from collections.abc import Sequence
from pathlib import Path

from repomod.dependencies import graph, snapshot
from repomod.dependencies.model import (
    Component,
    CoverageKind,
    Ecosystem,
    IndexEntry,
    Instance,
    Issue,
    Project,
    RepositoryData,
    SourceKind,
    purl,
)


def instance(
    ecosystem: Ecosystem,
    name: str,
    version: str | None,
    *,
    context: str = "",
    source_kind: SourceKind = SourceKind.REGISTRY,
    first_party: bool = False,
    vcs_url: str | None = None,
    repository_url: str | None = None,
) -> Instance:
    component = purl(ecosystem, name)
    value = purl(ecosystem, name, version)
    return Instance(
        f"{value}#{context}",
        value,
        component,
        version,
        source_kind,
        context=context,
        vcs_url=vcs_url,
        repository_url=repository_url,
        first_party=first_party,
    )


def components(instances: Sequence[Instance]) -> tuple[Component, ...]:
    found: dict[str, Component] = {}
    for item in instances:
        purl_type = item.purl.removeprefix("pkg:").split("/", 1)[0]
        encoded = item.component.split("/", 1)[1]
        found.setdefault(
            item.component,
            Component(item.component, Ecosystem(purl_type), encoded, encoded),
        )
    return tuple(found.values())


def project(directory: str, ecosystem: Ecosystem) -> Project:
    return Project(f"{directory}:{ecosystem.value}", ecosystem, directory)


def issue(
    kind: CoverageKind, path: str | None = None, locator: str | None = None, message: str = ""
) -> Issue:
    return Issue(kind, message or kind.value, path=path, locator=locator)


def repository(
    name: str,
    instances: Sequence[Instance],
    *,
    tags: tuple[str, ...] = ("tool",),
    projects: Sequence[Project] = (),
    issues: Sequence[Issue] = (),
) -> RepositoryData:
    return RepositoryData(
        repository=name,
        tags=tags,
        inputs=(),
        inputs_fingerprint=snapshot.fingerprint(()),
        projects=tuple(projects),
        components=components(instances),
        instances=tuple(instances),
        issues=tuple(issues),
    )


def write_inventory(root: Path, repositories: Sequence[RepositoryData]) -> Path:
    inventory = root / "data" / "dependencies" / "v1"
    inventory.mkdir(parents=True, exist_ok=True)
    entries = {}
    for data in repositories:
        path = snapshot.repository_file(inventory, data.repository)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(snapshot.serialize(snapshot.repository_dict(data)), encoding="utf-8")
        entries[data.repository] = IndexEntry("a" * 40, data.inputs_fingerprint, data.state)
    (inventory / "index.json").write_text(
        snapshot.serialize(snapshot.index_dict(entries, snapshot.PARSER_VERSION)),
        encoding="utf-8",
    )
    return inventory


def build_graph(root: Path, repositories: Sequence[RepositoryData]) -> graph.Graph:
    inventory = write_inventory(root, repositories)
    return graph.build_graph(graph.load_inventory(inventory))
