from __future__ import annotations

from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from math import floor

from repomod.audit import PackageSize
from repomod.dependencies import enrich
from repomod.dependencies.graph import Graph
from repomod.dependencies.model import Ecosystem, Edge, Instance, Relation, Scope
from repomod.dependencies.world import Origin, World

TOP_PER_LANGUAGE = 12
DEV_SCOPES = frozenset({Scope.DEV, Scope.TEST, Scope.BUILD, Scope.CI})


@dataclass(frozen=True, slots=True)
class Language:
    key: str
    label: str
    short: str
    unit: str
    measure: str
    sized: bool


LANGUAGES = (
    Language("npm", "TypeScript/JavaScript", "TS/JS", "unpacked", "npm unpacked size, the installed size", True),
    Language("cargo", "Rust", "Rust", "crate archives", "crates.io .crate archive, compressed", True),
    Language(
        "pypi",
        "Python",
        "Python",
        "wheel downloads",
        "wheel download (macOS arm64, else Linux, else pure Python) or the sdist when there is no wheel",
        True,
    ),
    Language("go", "Go", "Go", "", "no size reported", False),
    Language("other", "Other", "Other", "", "no size reported (actions, images, system packages, toolchains)", False),
)


@dataclass(frozen=True, slots=True)
class Platform:
    key: str
    label: str
    os: str | None
    cpu: str | None
    libc: str | None


PLATFORMS = (
    Platform("darwin-arm64", "macOS arm64", "darwin", "arm64", None),
    Platform("linux-x64-musl", "Linux x64 (musl)", "linux", "x64", "musl"),
    Platform("linux-arm64-musl", "Linux arm64 (musl)", "linux", "arm64", "musl"),
    Platform("all", "All platforms (upper bound)", None, None, None),
)
DEFAULT_PLATFORM = PLATFORMS[0].key
type Variant = tuple[str, bool]
DEFAULT_VARIANT: Variant = (DEFAULT_PLATFORM, False)
VARIANTS: tuple[Variant, ...] = tuple((platform.key, dev) for platform in PLATFORMS for dev in (False, True))
_KEYS = {Ecosystem.NPM: "npm", Ecosystem.CARGO: "cargo", Ecosystem.PYPI: "pypi", Ecosystem.GOLANG: "go"}
_KINDS = {"unpacked": "unpacked", "crate": "crate archive", "wheel": "wheel", "sdist": "sdist"}


@dataclass(frozen=True, slots=True)
class Package:
    language: str
    ecosystem: Ecosystem
    purl: str
    name: str
    version: str | None
    bytes: int | None
    kind: str | None
    direct: bool
    dev_only: bool = False
    platforms: frozenset[str] = frozenset(platform.key for platform in PLATFORMS)

    def counts(self, variant: Variant) -> bool:
        return variant[0] in self.platforms and (variant[1] or not self.dev_only)


@dataclass(frozen=True, slots=True)
class Total:
    bytes: int
    packages: int
    unknown: int


@dataclass(frozen=True, slots=True)
class Dependency:
    """A direct dependency and what removing it would drop, per variant.

    `frees` covers the dependency itself and the packages only it brings in; `shared` covers
    packages it brings in that another counted direct dependency also reaches.
    """

    repository: str
    package: Package
    frees: Mapping[Variant, Total]
    shared: Mapping[Variant, Total]

    def counts(self, variant: Variant) -> bool:
        return self.frees[variant].packages > 0


@dataclass(frozen=True, slots=True)
class Repository:
    repository: str
    totals: Mapping[Variant, Mapping[str, Total]]
    heaviest: tuple[Package, ...]
    dependencies: tuple[Dependency, ...] = ()

    def total(self, languages: Iterable[str], variant: Variant = DEFAULT_VARIANT) -> Total:
        found = [self.totals[variant][key] for key in languages]
        return Total(sum(t.bytes for t in found), sum(t.packages for t in found), sum(t.unknown for t in found))


def language_key(ecosystem: Ecosystem) -> str:
    return _KEYS.get(ecosystem, "other")


def kind_label(kind: str | None) -> str:
    return _KINDS.get(kind or "", "")


def format_bytes(value: int) -> str:
    """Format a byte count in decimal units with at most three significant digits."""
    units = ("B", "kB", "MB", "GB", "TB")
    if value < 1000:
        return f"{value} B"
    amount, index = float(value), 0
    while amount >= 1000 and index < len(units) - 1:
        amount /= 1000
        index += 1
    rounded = floor(amount * 10 + 0.5) / 10 if amount < 10 else float(floor(amount + 0.5))
    if rounded >= 1000 and index < len(units) - 1:
        rounded, index = floor(rounded / 1000 * 10 + 0.5) / 10, index + 1
    return f"{rounded:.1f} {units[index]}" if rounded < 10 else f"{rounded:.0f} {units[index]}"


def _allowed(value: str | None, allowed: Iterable[str] | None) -> bool:
    """npm's check of one platform field: a list of allowed values, where `!x` forbids x."""
    names = list(allowed or ())
    if not names or value is None:
        return True
    matched = denied = False
    for name in names:
        if name.startswith("!"):
            if name[1:] == value:
                return False
            denied = True
        elif name == value:
            matched = True
    return matched or denied


def npm_allows(platform: Platform, size: PackageSize | None) -> bool:
    """Whether npm would install a version with these os, cpu and libc lists on the platform."""
    if size is None:
        return True
    return (
        _allowed(platform.os, size.os)
        and _allowed(platform.cpu, size.cpu)
        and (platform.os != "linux" or _allowed(platform.libc, size.libc))
    )


def dependency_rank(dependency: Dependency, variant: Variant) -> tuple[int, int, int, str, str]:
    frees, shared = dependency.frees[variant], dependency.shared[variant]
    package = dependency.package
    return (-frees.bytes, -frees.packages, -shared.bytes, package.name.casefold(), dependency.repository.casefold())


def build(
    graph: Graph,
    repositories: Iterable[str],
    results: Mapping[str, Mapping[str, object]],
    value: World,
) -> tuple[Repository, ...]:
    """Total each repository's third-party packages by language, platform, and scope.

    A package counts once per repository however many lockfiles, targets, or projects list its
    version. Packages the world page classifies as yours are left out. A package is dev-only when
    it is reached from the repository's direct dependencies only through ones whose scope is in
    DEV_SCOPES, following transitive edges, which lockfiles record as runtime. A package is left
    out for a platform when every edge reaching it is optional and its npm os, cpu or libc lists
    exclude that platform; the all-platforms variant counts everything.

    Each direct dependency frees itself and the packages only it reaches among the counted direct
    dependencies of the variant; packages another one also reaches are shared. A package that is
    itself declared directly is never freed by removing something else. Your own direct packages
    hold what they reach but are not listed.
    """
    yours = {package.component.purl for package in value.packages if package.origin is Origin.YOURS}
    rows = []
    for name in sorted(set(repositories), key=str.casefold):
        data = graph.repositories[name]
        projects = {project.id for project in data.projects}
        by_id = {instance.id: instance for instance in data.instances}
        direct = {
            by_id[edge.to_id].id
            for edge in data.edges
            if edge.relation is Relation.DIRECT and edge.from_id in projects and edge.to_id in by_id
        }
        runtime = _reach(data.edges, projects, by_id, dev=False)
        development = _reach(data.edges, projects, by_id, dev=True)
        incoming: dict[str, list[bool]] = {}
        for edge in data.edges:
            incoming.setdefault(edge.to_id, []).append(edge.optional)
        components = {component.purl: component for component in data.components}
        groups: dict[tuple[str, str | None], list[Instance]] = {}
        for instance in data.instances:
            if instance.component in components and instance.component not in yours:
                groups.setdefault((instance.component, instance.version), []).append(instance)
        packages: dict[tuple[str, str | None], Package] = {}
        for (purl, version), members in groups.items():
            component = components[purl]
            found = [
                size for instance in members if (size := enrich.cached_size(results.get(instance.purl))) is not None
            ]
            sized = next((size for size in found if size.bytes is not None), None)
            described = sized or next((size for size in found if size.os is not None), None)
            edges = [optional for instance in members for optional in incoming.get(instance.id, ())]
            optional_only = bool(edges) and all(edges)
            packages[(purl, version)] = (
                Package(
                    language_key(component.ecosystem),
                    component.ecosystem,
                    purl,
                    component.display_name,
                    version,
                    sized.bytes if sized else None,
                    sized.kind if sized else None,
                    any(instance.id in direct for instance in members),
                    not any(instance.id in runtime for instance in members)
                    and any(instance.id in development for instance in members),
                    frozenset(
                        platform.key
                        for platform in PLATFORMS
                        if platform.key == "all" or not optional_only or npm_allows(platform, described)
                    ),
                )
            )
        dependencies = _dependencies(name, data.edges, by_id, direct, packages)
        rows.append(_repository(name, tuple(packages.values()), dependencies))
    return tuple(rows)


type _Key = tuple[str, str | None]


def _dependencies(
    repository: str,
    edges: Iterable[Edge],
    by_id: Mapping[str, Instance],
    direct: set[str],
    packages: Mapping[_Key, Package],
) -> tuple[Dependency, ...]:
    def key(instance_id: str) -> _Key:
        instance = by_id[instance_id]
        return (instance.component, instance.version)

    children: dict[_Key, set[_Key]] = {}
    for edge in edges:
        if edge.from_id in by_id and edge.to_id in by_id:
            children.setdefault(key(edge.from_id), set()).add(key(edge.to_id))
    roots = {key(instance_id) for instance_id in direct}
    reach = {root: _closure(root, children) - (roots - {root}) for root in roots}
    rows: dict[_Key, Dependency] = {}
    frees: dict[_Key, dict[Variant, Total]] = {root: {} for root in roots if root in packages}
    shared: dict[_Key, dict[Variant, Total]] = {root: {} for root in frees}
    for variant in VARIANTS:
        holding = {
            root: [packages[member] for member in reached if member in packages and packages[member].counts(variant)]
            for root, reached in reach.items()
            if root not in packages or packages[root].counts(variant)
        }
        holders: dict[int, int] = {}
        for members in holding.values():
            for package in members:
                holders[id(package)] = holders.get(id(package), 0) + 1
        for root in frees:
            members = holding.get(root, [])
            frees[root][variant] = _total([package for package in members if holders[id(package)] == 1])
            shared[root][variant] = _total([package for package in members if holders[id(package)] > 1])
    for root in frees:
        rows[root] = Dependency(repository, packages[root], frees[root], shared[root])
    kept: dict[_Key, Dependency] = {}
    for variant in VARIANTS:
        for language in LANGUAGES:
            ranked = sorted(
                (
                    dependency
                    for dependency in rows.values()
                    if dependency.package.language == language.key and dependency.counts(variant)
                ),
                key=lambda dependency: dependency_rank(dependency, variant),
            )
            kept.update(
                ((dependency.package.purl, dependency.package.version), dependency)
                for dependency in ranked[:TOP_PER_LANGUAGE]
            )
    return tuple(sorted(kept.values(), key=lambda dependency: dependency_rank(dependency, DEFAULT_VARIANT)))


def _closure(start: _Key, children: Mapping[_Key, set[_Key]]) -> set[_Key]:
    reached = {start}
    pending = [start]
    while pending:
        for target in children.get(pending.pop(), ()):
            if target not in reached:
                reached.add(target)
                pending.append(target)
    return reached


def _total(members: Iterable[Package]) -> Total:
    found = list(members)
    return Total(
        sum(package.bytes or 0 for package in found),
        len(found),
        sum(1 for package in found if package.bytes is None),
    )


def _reach(
    edges: Iterable[Edge], projects: set[str], by_id: Mapping[str, Instance], *, dev: bool
) -> set[str]:
    """Instances reached from direct edges whose scope is (dev) or is not (not dev) in DEV_SCOPES."""
    following: dict[str, list[str]] = {}
    seeds = []
    for edge in edges:
        if edge.to_id not in by_id:
            continue
        if edge.from_id in projects:
            if (edge.scope in DEV_SCOPES) is dev:
                seeds.append(edge.to_id)
        elif edge.from_id in by_id:
            following.setdefault(edge.from_id, []).append(edge.to_id)
    reached = set(seeds)
    pending = list(seeds)
    while pending:
        for target in following.get(pending.pop(), ()):
            if target not in reached:
                reached.add(target)
                pending.append(target)
    return reached


def _repository(name: str, packages: tuple[Package, ...], dependencies: tuple[Dependency, ...]) -> Repository:
    totals: dict[Variant, dict[str, Total]] = {}
    chosen: dict[int, Package] = {}
    for variant in VARIANTS:
        totals[variant] = {}
        for language in LANGUAGES:
            members = [
                package for package in packages if package.language == language.key and package.counts(variant)
            ]
            sized = sorted(
                (package for package in members if package.bytes is not None),
                key=lambda package: (-(package.bytes or 0), package.name.casefold(), package.version or ""),
            )
            totals[variant][language.key] = Total(
                sum(package.bytes or 0 for package in sized), len(members), len(members) - len(sized)
            )
            chosen.update((id(package), package) for package in sized[:TOP_PER_LANGUAGE])
    heaviest = sorted(
        chosen.values(), key=lambda package: (-(package.bytes or 0), package.name.casefold(), package.version or "")
    )
    return Repository(name, totals, tuple(heaviest), dependencies)
