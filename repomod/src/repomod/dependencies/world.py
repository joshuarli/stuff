from __future__ import annotations

import re
from collections import defaultdict
from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from enum import Enum
from urllib.parse import parse_qs, urlsplit

from repomod.dependencies import graph as graph_module
from repomod.dependencies.graph import Graph
from repomod.dependencies.model import Component, Ecosystem, Instance, Relation, SourceKind

_FORGES = frozenset({"github.com", "gitlab.com", "codeberg.org", "bitbucket.org", "git.sr.ht", "docker.io"})
_HOST_ALIASES = {"ghcr.io": "github.com", "index.docker.io": "docker.io", "hub.docker.com": "docker.io"}
_SHORTHANDS = {"github": "github.com", "gitlab": "gitlab.com", "bitbucket": "bitbucket.org"}
_BARE_REPOSITORY = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
_LOCAL = frozenset({SourceKind.WORKSPACE, SourceKind.PATH})
_DISTRIBUTIONS = {Ecosystem.APK: "alpinelinux.org"}
_TOOLCHAINS = {
    "go": "github.com/golang",
    "node": "github.com/nodejs",
    "nodejs": "github.com/nodejs",
    "pnpm": "github.com/pnpm",
    "python": "github.com/python",
    "rust": "github.com/rust-lang",
    "zig": "github.com/ziglang",
}


class Origin(Enum):
    YOURS = "yours"
    THIRD_PARTY = "third-party"
    UNKNOWN = "owner unknown"


@dataclass(frozen=True, slots=True)
class Package:
    component: Component
    owner: str | None
    origin: Origin
    in_repo: bool
    direct: bool
    repositories: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class World:
    packages: tuple[Package, ...]
    repositories: tuple[str, ...]
    owners: frozenset[str]


def source_owner(url: str) -> str | None:
    """Return `host/owner` for forge URLs, the host for other URLs, or None."""
    text = url.strip().removeprefix("git+")
    head, separator, rest = text.partition(":")
    if separator and head in _SHORTHANDS and not rest.startswith("//"):
        return _owner(_SHORTHANDS[head], rest)
    if text.startswith("git@"):
        host, _, path = text.removeprefix("git@").partition(":")
        return _owner(host, path)
    if "://" not in text:
        return _owner("github.com", text) if _BARE_REPOSITORY.match(text) else None
    try:
        parts = urlsplit(text)
        host = parts.hostname or ""
    except ValueError:
        return None
    return _owner(host, parts.path)


def display_owner(owner: str | None) -> str:
    return owner.removeprefix("github.com/") if owner is not None else "unknown"


def build(
    graph: Graph,
    repositories: Iterable[str],
    sources: Mapping[str, tuple[str, ...]],
) -> World:
    selected = tuple(sorted(set(repositories), key=str.casefold))
    yours = frozenset(f"github.com/{name.split('/', 1)[0].casefold()}" for name in graph.catalogued)
    reach: dict[str, set[str]] = defaultdict(set)
    instances: dict[str, list[Instance]] = defaultdict(list)
    local: dict[str, set[str]] = defaultdict(set)
    published: set[str] = set()
    direct: set[str] = set()
    for repository in selected:
        data = graph.repositories[repository]
        projects = {project.id for project in data.projects}
        by_id = {instance.id: instance for instance in data.instances}
        for instance in data.instances:
            reach[instance.component].add(repository)
            instances[instance.component].append(instance)
            if instance.first_party or instance.source_kind in _LOCAL:
                local[instance.component].add(repository)
            elif graph_module.is_first_party(graph, instance):
                published.add(instance.component)
        for edge in data.edges:
            if edge.relation is Relation.DIRECT and edge.from_id in projects and edge.to_id in by_id:
                direct.add(by_id[edge.to_id].component)
    packages = []
    for purl, found in reach.items():
        component = graph.components.get(purl)
        if component is None:
            continue
        if purl in local:
            owner: str | None = f"github.com/{min(local[purl], key=str.casefold).split('/', 1)[0].casefold()}"
        else:
            owner = _component_owner(component, instances[purl], sources.get(purl, ()), yours)
        if purl in local or purl in published or owner in yours:
            origin = Origin.YOURS
        elif owner is not None:
            origin = Origin.THIRD_PARTY
        else:
            origin = Origin.UNKNOWN
        packages.append(
            Package(
                component,
                owner,
                origin,
                purl in local,
                purl in direct,
                tuple(sorted(found, key=str.casefold)),
            )
        )
    packages.sort(key=lambda package: (-len(package.repositories), package.component.purl.casefold()))
    return World(tuple(packages), selected, yours)


def _owner(host: str, path: str) -> str | None:
    host = host.casefold().removeprefix("www.")
    host = _HOST_ALIASES.get(host, host)
    if not host:
        return None
    if host not in _FORGES:
        return host
    segment = path.strip("/").split("/", 1)[0].casefold()
    return f"{host}/{segment}" if segment else None


def _component_owner(
    component: Component,
    instances: Iterable[Instance],
    urls: Iterable[str],
    yours: frozenset[str],
) -> str | None:
    for instance in instances:
        query = parse_qs(instance.purl.partition("?")[2])
        for url in (instance.vcs_url, *query.get("download_url", ())):
            if url is not None and (owner := source_owner(url)) is not None:
                return owner
    name = component.name.split("?", 1)[0]
    match component.ecosystem:
        case Ecosystem.GITHUB:
            return _owner("github.com", name)
        case Ecosystem.GOLANG:
            host, _, path = name.partition("/")
            return _owner(host, path) if path else None
        case Ecosystem.DOCKER:
            return _image_owner(name)
        case Ecosystem.APK:
            return _DISTRIBUTIONS[Ecosystem.APK]
        case Ecosystem.GENERIC if name in _TOOLCHAINS:
            return _TOOLCHAINS[name]
        case _:
            pass
    owners = [owner for url in urls if (owner := source_owner(url)) is not None]
    if owners:
        return next((owner for owner in owners if "/" in owner), owners[0])
    if component.ecosystem is Ecosystem.NPM and name.startswith("@") and "/" in name:
        scope = name[1:].split("/", 1)[0].casefold()
        return f"github.com/{scope}" if f"github.com/{scope}" in yours else f"npmjs.com/@{scope}"
    return None


def _image_owner(name: str) -> str | None:
    parts = name.split("/")
    if len(parts) == 1:
        return "docker.io/library"
    if "." in parts[0] or ":" in parts[0]:
        return _owner(parts[0].split(":", 1)[0], "/".join(parts[1:]))
    return _owner("docker.io", parts[0])
