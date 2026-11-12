from __future__ import annotations

import urllib.parse
from collections import defaultdict
from collections.abc import Iterable, Mapping
from dataclasses import dataclass, replace
from enum import Enum
from typing import cast

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
from repomod.dependency_roles import Role

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


class TargetBasis(Enum):
    APPROVED_PIN = "approved pin"
    LATEST = "latest"
    NEWEST_IN_USE = "newest in use"
    MOST_COMMON = "most common"


@dataclass(frozen=True, slots=True)
class Split:
    component: Component
    target: str
    basis: TargetBasis
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
class RoleUse:
    role: Role
    options: Mapping[str, tuple[str, ...]]
    packages: Mapping[tuple[str, str], tuple[str, ...]]

    @property
    def in_use(self) -> tuple[str, ...]:
        return tuple(option for option, repositories in self.options.items() if repositories)

    @property
    def overlapping(self) -> tuple[str, ...]:
        return _sorted(
            repository
            for repository, chosen in self._chosen().items()
            if len(chosen) > 1 and not self.role.allows(chosen)
        )

    @property
    def off_preferred(self) -> tuple[str, ...]:
        if self.role.preferred is None:
            return ()
        return _sorted(
            repository
            for repository, chosen in self._chosen().items()
            if chosen != {self.role.preferred} and not self.role.allows(chosen)
        )

    @property
    def excepted(self) -> tuple[tuple[str, str], ...]:
        used = {repository for repositories in self.options.values() for repository in repositories}
        return tuple(
            (repository, reason)
            for repository, reason in sorted(self.role.exceptions.items(), key=lambda item: item[0].casefold())
            if repository in used
        )

    def _chosen(self) -> dict[str, frozenset[str]]:
        chosen: dict[str, set[str]] = defaultdict(set)
        for option, repositories in self.options.items():
            for repository in repositories:
                if repository not in self.role.exceptions:
                    chosen[repository].add(option)
        return {repository: frozenset(options) for repository, options in chosen.items()}


@dataclass(frozen=True, slots=True)
class Scorecard:
    repository: str
    vulnerable: int
    behind: int
    breaking: int
    off_target: int
    mixed_roles: tuple[str, ...]
    off_preferred: tuple[str, ...]
    heaviest: Weight | None

    @property
    def rank(self) -> tuple[int, ...]:
        return (
            -self.vulnerable,
            -self.breaking,
            -self.behind,
            -self.off_target,
            -len(self.mixed_roles) - len(self.off_preferred),
        )


def declarations(graph: Graph) -> tuple[Declaration, ...]:
    found: dict[tuple[str, str], tuple[Instance, Component, str, set[Scope]]] = {}
    for repository, data in graph.repositories.items():
        projects = {project.id for project in data.projects}
        for edge in data.edges:
            if edge.relation is not Relation.DIRECT or edge.from_id not in projects:
                continue
            instance = graph.instances.get(edge.to_id)
            if instance is None or instance.source_kind in _LOCAL_SOURCES:
                continue
            version = _declaration_version(instance)
            if version is None:
                continue
            component = graph.components.get(instance.component)
            if component is None:
                continue
            entry = found.setdefault((repository, instance.id), (instance, component, version, set()))
            entry[3].add(edge.scope)
    return tuple(
        Declaration(repository, instance, component, version, frozenset(scopes))
        for (repository, _), (instance, component, version, scopes) in sorted(
            found.items(), key=lambda item: (item[0][0].casefold(), item[0][1])
        )
    )


def _declaration_version(instance: Instance) -> str | None:
    component_ecosystem = instance.purl.removeprefix("pkg:").split("/", 1)[0]
    if component_ecosystem == Ecosystem.DOCKER.value:
        tag = instance.version or "latest"
        digest = _purl_digest(instance.purl)
        return f"{tag}@{digest}" if digest is not None else tag
    return instance.version


def forks(graph: Graph) -> tuple[str, ...]:
    return _sorted(name for name in graph.repositories if "fork" in graph.tags.get(name, ()))


def owned(graph: Graph, found: Iterable[Declaration]) -> tuple[Declaration, ...]:
    upstream = set(forks(graph))
    return tuple(declaration for declaration in found if declaration.repository not in upstream)


def gap(ecosystem: Ecosystem, current: str, latest: str) -> Gap | None:
    have = release(ecosystem, current)
    want = release(ecosystem, latest)
    if have is None or want is None or _release_key(ecosystem, current) >= _release_key(ecosystem, latest):
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
    pieces = version.removeprefix("v").split(".")
    if not pieces[0].isdigit():
        return None
    parts: list[int] = []
    for piece in pieces:
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
    found = tuple(found)
    targets = _latest_releases(found, enrichment)
    behind: dict[tuple[Ecosystem, str], dict[str, set[str]]] = defaultdict(lambda: defaultdict(set))
    latest_of: dict[tuple[Ecosystem, str], str] = {}
    components: dict[tuple[Ecosystem, str], Component] = {}
    for declaration in found:
        target = targets.get((declaration.component.ecosystem, declaration.component.name))
        if target is None:
            continue
        latest = target[0]
        if gap(declaration.component.ecosystem, declaration.version, latest) is None:
            continue
        key = (declaration.component.ecosystem, declaration.component.name)
        components.setdefault(key, declaration.component)
        latest_of[key] = latest
        behind[key][declaration.version].add(declaration.repository)
    rows = []
    for key, versions in behind.items():
        component = components[key]
        worst = min(
            (cast(Gap, gap(component.ecosystem, version, latest_of[key])) for version in versions),
            key=lambda item: item.rank,
        )
        rows.append(Outdated(component, latest_of[key], worst, _uses(component, versions)))
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
    rows: dict[tuple[Ecosystem, str, str], Vulnerable] = {}
    seen: set[str] = set()
    for instance in graph.instances.values():
        if instance.version is None or instance.purl in seen:
            continue
        seen.add(instance.purl)
        advisories = _advisories(enrichment.get(instance.purl))
        component = graph.components.get(instance.component)
        if not advisories or component is None:
            continue
        key = (component.ecosystem, component.name, instance.version)
        current = rows.get(key)
        declared = _sorted({*direct[instance.purl], *(current.direct_in if current else ())})
        locked = {*everywhere[instance.purl], *(current.transitive_in if current else ())} - set(declared)
        rows[key] = Vulnerable(
            current.component if current else component,
            instance.version,
            tuple(sorted({*advisories, *(current.advisories if current else ())})),
            (current.latest if current else None) or _latest(enrichment, instance),
            declared,
            _sorted(locked),
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
    found: Iterable[Declaration],
    enrichment: Mapping[str, Mapping[str, object]],
    *,
    pins: Mapping[str, str],
) -> tuple[Split, ...]:
    found = tuple(found)
    targets = _latest_releases(found, enrichment)
    versions: dict[str, dict[str, set[str]]] = defaultdict(lambda: defaultdict(set))
    components: dict[str, Component] = {}
    for declaration in found:
        if declaration.component.ecosystem in _UNALIGNED_ECOSYSTEMS:
            continue
        if declaration.component.ecosystem is Ecosystem.DOCKER:
            name = declaration.component.name.split("?", 1)[0]
            purl = f"pkg:docker/{name}"
        else:
            purl = declaration.component.purl
        if purl not in components:
            clean = declaration.component.name.split("?", 1)[0]
            components[purl] = Component(
                purl,
                declaration.component.ecosystem,
                clean,
                declaration.component.display_name.split("?", 1)[0],
            )
        versions[purl][declaration.version].add(declaration.repository)
    rows = []
    for purl, by_version in versions.items():
        component = components[purl]
        uses = _uses(component, by_version)
        pin = pins.get(component.name) if component.ecosystem is Ecosystem.GITHUB else None
        if pin is not None:
            if set(by_version) != {pin}:
                rows.append(Split(component, pin, TargetBasis.APPROVED_PIN, uses))
            continue
        repositories = {repository for found in by_version.values() for repository in found}
        if len(by_version) < 2 or len(repositories) < 2:
            continue
        target, basis = targets.get(
            (component.ecosystem, component.name), _fallback_target(component, uses)
        )
        rows.append(Split(component, target, basis, uses))
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


def drift(split_rows: Iterable[Split], outdated_rows: Iterable[Outdated]) -> tuple[Split, ...]:
    behind: dict[tuple[Ecosystem, str], set[str]] = defaultdict(set)
    for row in outdated_rows:
        for use in row.uses:
            behind[(row.component.ecosystem, row.component.name)].update(use.repositories)
    remaining = []
    for row in split_rows:
        if row.basis not in {TargetBasis.LATEST, TargetBasis.NEWEST_IN_USE}:
            remaining.append(row)
            continue
        moved = behind[(row.component.ecosystem, row.component.name)]
        uses = tuple(
            use
            if use.version == row.target
            else VersionUse(use.version, tuple(name for name in use.repositories if name not in moved))
            for use in row.uses
        )
        left = replace(row, uses=tuple(use for use in uses if use.repositories))
        if left.off_target:
            remaining.append(left)
    return tuple(remaining)


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


def scorecards(
    repositories: Iterable[str],
    outdated_rows: Iterable[Outdated],
    vulnerable_rows: Iterable[Vulnerable],
    split_rows: Iterable[Split],
    role_rows: Iterable[RoleUse],
    weight_rows: Iterable[Weight],
) -> tuple[Scorecard, ...]:
    vulnerable_count: dict[str, int] = defaultdict(int)
    for row in vulnerable_rows:
        for repository in {*row.direct_in, *row.transitive_in}:
            vulnerable_count[repository] += 1
    behind: dict[str, int] = defaultdict(int)
    breaking: dict[str, int] = defaultdict(int)
    for row in outdated_rows:
        for use in row.uses:
            severity = gap(row.component.ecosystem, use.version, row.latest)
            for repository in use.repositories:
                behind[repository] += 1
                if severity is Gap.BREAKING:
                    breaking[repository] += 1
    off_target: dict[str, int] = defaultdict(int)
    for row in split_rows:
        for repository in set(row.off_target):
            off_target[repository] += 1
    mixed: dict[str, list[str]] = defaultdict(list)
    off_preferred: dict[str, list[str]] = defaultdict(list)
    for use in role_rows:
        for repository in use.overlapping:
            mixed[repository].append(use.role.name)
        for repository in use.off_preferred:
            off_preferred[repository].append(use.role.name)
    heaviest: dict[str, Weight] = {}
    for row in weight_rows:
        current = heaviest.get(row.repository)
        if current is None or row.sole > current.sole:
            heaviest[row.repository] = row
    cards = [
        Scorecard(
            repository,
            vulnerable_count[repository],
            behind[repository],
            breaking[repository],
            off_target[repository],
            tuple(sorted(mixed[repository])),
            tuple(sorted(off_preferred[repository])),
            heaviest.get(repository),
        )
        for repository in repositories
    ]
    return tuple(sorted(cards, key=lambda card: (card.rank, card.repository.casefold())))


def _latest(enrichment: Mapping[str, Mapping[str, object]], instance: Instance) -> str | None:
    entry = enrichment.get(instance.purl)
    latest = entry.get("latest") if entry is not None else None
    return latest if isinstance(latest, str) else None


def _release_key(ecosystem: Ecosystem, version: str) -> tuple[tuple[int, ...], int, tuple[tuple[int, int, str], ...]]:
    parsed = release(ecosystem, version)
    if parsed is None:
        return (), 0, ()
    while len(parsed) > 1 and parsed[-1] == 0:
        parsed = parsed[:-1]
    if ecosystem is Ecosystem.PYPI:
        value = Version(version)
        parsed = (value.epoch, *parsed)
        stage = (
            -1 if value.dev is not None and value.pre is None and value.post is None
            else int(value.pre is None)
        )
        local = tuple(
            (1, int(part), "") if part.isdigit() else (0, 0, part)
            for part in (value.local or "").split(".")
        )
        return parsed, stage, (
            (1, 0, value.pre[0] if value.pre else ""),
            (0, value.pre[1] if value.pre else 0, ""),
            (0, value.post if value.post is not None else -1, ""),
            (int(value.dev is None), value.dev or 0, ""),
            (int(value.local is not None), 0, ""),
            *local,
        )
    base = version.split("@", 1)[0].split("+", 1)[0]
    _, separator, prerelease = base.partition("-")
    parts = prerelease.split(".")
    identifiers = tuple(
        (0, int(part), "") if part.isdigit() else (1, 0, part)
        for part in parts
        if part
    )
    return parsed, int(not separator), identifiers


def _latest_releases(
    found: tuple[Declaration, ...], enrichment: Mapping[str, Mapping[str, object]]
) -> dict[tuple[Ecosystem, str], tuple[str, TargetBasis]]:
    candidates: dict[tuple[Ecosystem, str], set[str]] = defaultdict(set)
    published: dict[tuple[Ecosystem, str], set[str]] = defaultdict(set)
    for declaration in found:
        ecosystem = declaration.component.ecosystem
        key = ecosystem, declaration.component.name
        candidates[key].add(declaration.version)
        latest = _latest(enrichment, declaration.instance)
        if latest is not None:
            candidates[key].add(latest)
            published[key].add(latest)
    targets = {}
    for key, releases in published.items():
        newest = max(candidates[key], key=lambda version: (_release_key(key[0], version), version))
        basis = TargetBasis.LATEST if newest in releases else TargetBasis.NEWEST_IN_USE
        targets[key] = newest, basis
    return targets


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


def _fallback_target(
    component: Component, uses: tuple[VersionUse, ...]
) -> tuple[str, TargetBasis]:
    comparable = [use for use in uses if release(component.ecosystem, use.version) is not None]
    if comparable:
        newest = max(
            comparable,
            key=lambda use: (_release_key(component.ecosystem, use.version), len(use.repositories), use.version),
        )
        return newest.version, TargetBasis.NEWEST_IN_USE
    common = max(uses, key=lambda use: (len(use.repositories), use.version))
    return common.version, TargetBasis.MOST_COMMON


def _purl_digest(purl: str) -> str | None:
    if "?" not in purl:
        return None
    query = purl.partition("?")[2]
    for pair in query.split("&"):
        key, _, value = pair.partition("=")
        if key == "digest":
            return urllib.parse.unquote(value) if value else None
    return None


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
            key=lambda item: _release_key(component.ecosystem, item[0]),
            reverse=True,
        )
    )


def _sorted(values: Iterable[str]) -> tuple[str, ...]:
    return tuple(sorted(set(values), key=str.casefold))
