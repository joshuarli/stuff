from __future__ import annotations

import tomllib
from collections import defaultdict
from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
from typing import Any, cast

from packaging.version import InvalidVersion, Version

from repomod.dependencies.graph import Graph
from repomod.dependencies.model import (
    Component,
    Ecosystem,
    Instance,
    Relation,
    Scope,
    SourceKind,
)

ROLES_PATH = Path(__file__).resolve().parents[3] / "dependency-roles.toml"
_LOCAL_SOURCES = frozenset({SourceKind.PATH, SourceKind.WORKSPACE})
_UNALIGNED_ECOSYSTEMS = frozenset({Ecosystem.APK})


class Gap(Enum):
    BREAKING = "breaking"
    MINOR = "minor"
    PATCH = "patch"

    @property
    def rank(self) -> int:
        return {Gap.BREAKING: 0, Gap.MINOR: 1, Gap.PATCH: 2}[self]


@dataclass(frozen=True, slots=True)
class Declaration:
    repository: str
    instance: Instance
    component: Component
    version: str
    scopes: frozenset[Scope]


@dataclass(frozen=True, slots=True)
class VersionUse:
    version: str
    repositories: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class Outdated:
    component: Component
    latest: str
    gap: Gap
    uses: tuple[VersionUse, ...]

    @property
    def repositories(self) -> int:
        return sum(len(use.repositories) for use in self.uses)


@dataclass(frozen=True, slots=True)
class Vulnerable:
    component: Component
    version: str
    advisories: tuple[str, ...]
    latest: str | None
    direct_in: tuple[str, ...]
    transitive_in: tuple[str, ...]


@dataclass(frozen=True, slots=True)
class Weight:
    repository: str
    component: Component
    version: str
    scopes: frozenset[Scope]
    pulls: int
    sole: int


@dataclass(frozen=True, slots=True)
class Split:
    component: Component
    target: str
    target_is_latest: bool
    uses: tuple[VersionUse, ...]

    @property
    def off_target(self) -> tuple[str, ...]:
        return tuple(
            repository
            for use in self.uses
            if use.version != self.target
            for repository in use.repositories
        )


@dataclass(frozen=True, slots=True)
class Role:
    name: str
    ecosystem: Ecosystem
    preferred: str | None
    options: Mapping[str, frozenset[str]]


@dataclass(frozen=True, slots=True)
class RoleUse:
    role: Role
    options: Mapping[str, tuple[str, ...]]
    packages: Mapping[tuple[str, str], tuple[str, ...]]

    @property
    def in_use(self) -> tuple[str, ...]:
        return tuple(option for option, repositories in self.options.items() if repositories)

    @property
    def overlapping(self) -> tuple[str, ...]:
        counts: dict[str, int] = defaultdict(int)
        for repositories in self.options.values():
            for repository in repositories:
                counts[repository] += 1
        return tuple(sorted((name for name, count in counts.items() if count > 1), key=str.casefold))

    @property
    def off_preferred(self) -> tuple[str, ...]:
        if self.role.preferred is None:
            return ()
        return tuple(
            sorted(
                {
                    repository
                    for option, repositories in self.options.items()
                    if option != self.role.preferred
                    for repository in repositories
                },
                key=str.casefold,
            )
        )


def declarations(graph: Graph) -> tuple[Declaration, ...]:
    found: dict[tuple[str, str], tuple[Instance, Component, set[Scope]]] = {}
    for repository, data in graph.repositories.items():
        projects = {project.id for project in data.projects}
        for edge in data.edges:
            if edge.relation is not Relation.DIRECT or edge.from_id not in projects:
                continue
            instance = graph.instances.get(edge.to_id)
            if (
                instance is None
                or instance.version is None
                or instance.source_kind in _LOCAL_SOURCES
            ):
                continue
            component = graph.components.get(instance.component)
            if component is None:
                continue
            entry = found.setdefault((repository, instance.id), (instance, component, set()))
            entry[2].add(edge.scope)
    return tuple(
        Declaration(repository, instance, component, cast(str, instance.version), frozenset(scopes))
        for (repository, _), (instance, component, scopes) in sorted(
            found.items(), key=lambda item: (item[0][0].casefold(), item[0][1])
        )
    )


def gap(ecosystem: Ecosystem, current: str, latest: str) -> Gap | None:
    have = release(ecosystem, current)
    want = release(ecosystem, latest)
    if have is None or want is None or have >= want:
        return None
    width = max(len(have), len(want), 3)
    have = have + (0,) * (width - len(have))
    want = want + (0,) * (width - len(want))
    significant = next((index for index, part in enumerate(have) if part), len(have) - 1)
    if have[: significant + 1] != want[: significant + 1]:
        return Gap.BREAKING
    if have[: significant + 2] != want[: significant + 2]:
        return Gap.MINOR
    return Gap.PATCH


def release(ecosystem: Ecosystem, version: str) -> tuple[int, ...] | None:
    if ecosystem is Ecosystem.PYPI:
        try:
            return Version(version).release
        except InvalidVersion:
            return None
    parts: list[int] = []
    for piece in version.removeprefix("v").split("."):
        digits = ""
        for character in piece:
            if not character.isdigit():
                break
            digits += character
        if not digits:
            break
        parts.append(int(digits))
        if len(digits) != len(piece):
            break
    return tuple(parts) if parts else None


def outdated(
    found: Iterable[Declaration], enrichment: Mapping[str, Mapping[str, object]]
) -> tuple[Outdated, ...]:
    behind: dict[str, dict[str, set[str]]] = defaultdict(lambda: defaultdict(set))
    latest_of: dict[str, str] = {}
    components: dict[str, Component] = {}
    for declaration in found:
        latest = _latest(enrichment, declaration.instance)
        if latest is None:
            continue
        if gap(declaration.component.ecosystem, declaration.version, latest) is None:
            continue
        purl = declaration.component.purl
        components[purl] = declaration.component
        latest_of[purl] = latest
        behind[purl][declaration.version].add(declaration.repository)
    rows = []
    for purl, versions in behind.items():
        component = components[purl]
        worst = min(
            (cast(Gap, gap(component.ecosystem, version, latest_of[purl])) for version in versions),
            key=lambda item: item.rank,
        )
        rows.append(Outdated(component, latest_of[purl], worst, _uses(component, versions)))
    return tuple(
        sorted(rows, key=lambda row: (row.gap.rank, -row.repositories, row.component.name.casefold()))
    )


def vulnerable(
    graph: Graph,
    found: Iterable[Declaration],
    enrichment: Mapping[str, Mapping[str, object]],
) -> tuple[Vulnerable, ...]:
    direct: dict[str, set[str]] = defaultdict(set)
    for declaration in found:
        direct[declaration.instance.purl].add(declaration.repository)
    everywhere: dict[str, set[str]] = defaultdict(set)
    for instance_id, instance in graph.instances.items():
        everywhere[instance.purl].update(graph.instance_repositories.get(instance_id, ()))
    rows: dict[str, Vulnerable] = {}
    for instance in graph.instances.values():
        if instance.version is None or instance.purl in rows:
            continue
        advisories = _advisories(enrichment.get(instance.purl))
        component = graph.components.get(instance.component)
        if not advisories or component is None:
            continue
        rows[instance.purl] = Vulnerable(
            component,
            instance.version,
            advisories,
            _latest(enrichment, instance),
            _sorted(direct[instance.purl]),
            _sorted(everywhere[instance.purl] - direct[instance.purl]),
        )
    return tuple(
        sorted(
            rows.values(),
            key=lambda row: (
                -len(row.direct_in),
                -len(row.direct_in) - len(row.transitive_in),
                row.component.name.casefold(),
            ),
        )
    )


def weights(graph: Graph, found: Iterable[Declaration]) -> tuple[Weight, ...]:
    roots: dict[str, list[Declaration]] = defaultdict(list)
    for declaration in found:
        roots[declaration.repository].append(declaration)
    rows: list[Weight] = []
    for repository, declared in roots.items():
        children: dict[str, set[str]] = defaultdict(set)
        for edge in graph.repositories[repository].edges:
            if edge.relation is Relation.TRANSITIVE:
                children[edge.from_id].add(edge.to_id)
        reach = {
            declaration.instance.id: _closure(declaration.instance.id, children)
            for declaration in declared
        }
        holders: dict[str, int] = defaultdict(int)
        for closure in reach.values():
            for node in closure:
                holders[node] += 1
        root_ids = set(reach)
        for declaration in declared:
            closure = reach[declaration.instance.id] - root_ids
            if not closure:
                continue
            rows.append(
                Weight(
                    repository,
                    declaration.component,
                    declaration.version,
                    declaration.scopes,
                    len(closure),
                    sum(1 for node in closure if holders[node] == 1),
                )
            )
    return tuple(
        sorted(
            rows,
            key=lambda row: (-row.sole, -row.pulls, row.repository.casefold(), row.component.name),
        )
    )


def splits(
    found: Iterable[Declaration], enrichment: Mapping[str, Mapping[str, object]]
) -> tuple[Split, ...]:
    versions: dict[str, dict[str, set[str]]] = defaultdict(lambda: defaultdict(set))
    components: dict[str, Component] = {}
    latest_of: dict[str, str] = {}
    for declaration in found:
        if declaration.component.ecosystem in _UNALIGNED_ECOSYSTEMS:
            continue
        purl = declaration.component.purl
        components[purl] = declaration.component
        versions[purl][declaration.version].add(declaration.repository)
        latest = _latest(enrichment, declaration.instance)
        if latest is not None:
            latest_of[purl] = latest
    rows = []
    for purl, by_version in versions.items():
        repositories = {repository for found in by_version.values() for repository in found}
        if len(by_version) < 2 or len(repositories) < 2:
            continue
        component = components[purl]
        uses = _uses(component, by_version)
        latest = latest_of.get(purl)
        rows.append(
            Split(
                component,
                latest if latest is not None else uses[0].version,
                latest is not None,
                uses,
            )
        )
    return tuple(
        sorted(
            rows,
            key=lambda row: (
                -len(set(row.off_target)),
                -len(row.uses),
                row.component.name.casefold(),
            ),
        )
    )


def role_uses(roles: Iterable[Role], found: Iterable[Declaration]) -> tuple[RoleUse, ...]:
    used: dict[tuple[Ecosystem, str], set[str]] = defaultdict(set)
    for declaration in found:
        used[(declaration.component.ecosystem, declaration.component.name)].add(
            declaration.repository
        )
    rows = []
    for role in roles:
        options: dict[str, tuple[str, ...]] = {}
        packages: dict[tuple[str, str], tuple[str, ...]] = {}
        for option, names in role.options.items():
            repositories: set[str] = set()
            for name in names:
                for repository in used.get((role.ecosystem, name), ()):
                    repositories.add(repository)
                    packages[(repository, option)] = tuple(
                        sorted({*packages.get((repository, option), ()), name})
                    )
            options[option] = _sorted(repositories)
        rows.append(RoleUse(role, options, packages))
    return tuple(
        sorted(
            rows,
            key=lambda row: (
                -len(row.overlapping),
                -len(row.off_preferred),
                -len(row.in_use),
                row.role.name,
            ),
        )
    )


def load_roles(path: Path = ROLES_PATH) -> tuple[Role, ...]:
    try:
        document = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        raise ValueError(f"cannot read {path.name}: {error}") from error
    return parse_roles(document, path.name)


def parse_roles(document: Mapping[str, object], source: str) -> tuple[Role, ...]:
    roles = []
    for name, raw in document.items():
        if not isinstance(raw, dict):
            raise ValueError(f"{source} [{name}] must be a table")
        table = cast(Mapping[str, Any], raw)
        unknown = set(table) - {"ecosystem", "preferred", "options"}
        if unknown:
            raise ValueError(f"{source} [{name}] has unknown keys: {', '.join(sorted(unknown))}")
        try:
            ecosystem = Ecosystem(table.get("ecosystem"))
        except ValueError as error:
            raise ValueError(f"{source} [{name}] has an unknown ecosystem") from error
        raw_options = table.get("options")
        if not isinstance(raw_options, dict) or len(raw_options) < 2:
            raise ValueError(f"{source} [{name}.options] must name at least two options")
        options: dict[str, frozenset[str]] = {}
        seen: dict[str, str] = {}
        for option, packages in cast(Mapping[str, object], raw_options).items():
            if (
                not isinstance(packages, list)
                or not packages
                or not all(isinstance(item, str) and item for item in packages)
            ):
                raise ValueError(f"{source} [{name}.options] {option} must list package names")
            for package in cast(list[str], packages):
                if package in seen:
                    raise ValueError(
                        f"{source} [{name}] lists {package} under both {seen[package]} and {option}"
                    )
                seen[package] = option
            options[option] = frozenset(cast(list[str], packages))
        preferred = table.get("preferred")
        if preferred is not None and preferred not in options:
            raise ValueError(f"{source} [{name}] prefers {preferred!r}, which is not an option")
        roles.append(Role(name, ecosystem, cast(str | None, preferred), options))
    return tuple(roles)


def _latest(enrichment: Mapping[str, Mapping[str, object]], instance: Instance) -> str | None:
    entry = enrichment.get(instance.purl)
    latest = entry.get("latest") if entry is not None else None
    return latest if isinstance(latest, str) else None


def _advisories(entry: Mapping[str, object] | None) -> tuple[str, ...]:
    vulns = entry.get("vulns") if entry is not None else None
    if not isinstance(vulns, list):
        return ()
    return tuple(
        sorted(
            str(cast(Mapping[str, object], item).get("id"))
            for item in cast(list[object], vulns)
            if isinstance(item, dict)
        )
    )


def _closure(start: str, children: Mapping[str, set[str]]) -> set[str]:
    seen: set[str] = set()
    stack = list(children.get(start, ()))
    while stack:
        node = stack.pop()
        if node in seen or node == start:
            continue
        seen.add(node)
        stack.extend(children.get(node, ()))
    return seen


def _uses(component: Component, versions: Mapping[str, set[str]]) -> tuple[VersionUse, ...]:
    return tuple(
        VersionUse(version, _sorted(repositories))
        for version, repositories in sorted(
            versions.items(),
            key=lambda item: release(component.ecosystem, item[0]) or (),
            reverse=True,
        )
    )


def _sorted(values: Iterable[str]) -> tuple[str, ...]:
    return tuple(sorted(set(values), key=str.casefold))
