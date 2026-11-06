from __future__ import annotations

from collections import Counter, defaultdict
from collections.abc import Iterable, Mapping
from typing import Any, cast

from repomod.audit import _BANNED_RUST_ALTERNATIVES, _BANNED_RUST_HTTP
from repomod.dependencies.graph import Graph, is_first_party_edge
from repomod.dependencies.model import (
    Component,
    CoverageKind,
    Ecosystem,
    Edge,
    Instance,
    Issue,
    Relation,
    RepositoryData,
    Resolution,
    Scope,
    SourceKind,
)

type Metric = dict[str, object]


def report(
    graph: Graph, repositories: Iterable[str] | None = None, *, top: int = 20
) -> dict[str, object]:
    selected = _selected(repositories)
    names = tuple(
        name
        for name in sorted(graph.repositories, key=str.casefold)
        if selected is None or name.casefold() in selected
    )
    selected_names = {name.casefold() for name in names}
    instances = {
        instance_id: instance
        for instance_id, instance in graph.instances.items()
        if {
            repository.casefold()
            for repository in graph.instance_repositories.get(instance_id, frozenset())
        }
        & selected_names
    }
    edges = tuple(
        edge
        for edge in graph.edges
        if _edge_repository(graph, edge).casefold() in selected_names
    )
    component_of = {
        instance_id: graph.components.get(instance.component)
        for instance_id, instance in instances.items()
    }
    issues = tuple(
        issue for name in names for issue in graph.repositories[name].issues
    )
    data = tuple(graph.repositories[name] for name in names)
    return {
        "repositories": list(names),
        "composition": _composition(names, data, instances, edges, component_of),
        "concentration": _concentration(graph, names, instances, edges, component_of, top),
        "divergence": _divergence(graph, instances, edges, component_of),
        "source_risk": _source_risk(instances, edges, component_of, issues),
        "scopes": _scopes(edges),
        "first_party": _first_party(graph, instances, edges, component_of),
        "roles": _roles(graph, names, instances, component_of),
        "freshness": {
            "available": False,
            "unit": "instances",
            "denominator": "all instances",
            "note": "run repomod deps enrich",
        },
    }


def _selected(repositories: Iterable[str] | None) -> set[str] | None:
    if repositories is None:
        return None
    return {repository.casefold() for repository in repositories}


def _edge_repository(graph: Graph, edge: Edge) -> str:
    if edge.from_id in graph.projects:
        return graph.project_repositories.get(edge.from_id, "")
    return next(
        iter(sorted(graph.instance_repositories.get(edge.from_id, frozenset[str]()))), ""
    )


def _metric(value: int, unit: str, denominator: str) -> Metric:
    return {"value": value, "unit": unit, "denominator": denominator}


def _value(metric: object) -> int:
    table = _table(metric)
    value = table.get("value", 0)
    return value if isinstance(value, int) else 0


def _component(
    component_of: Mapping[str, Component | None], instance_id: str
) -> Component | None:
    return component_of.get(instance_id)


def _composition(
    names: Iterable[str],
    data: tuple[RepositoryData, ...],
    instances: Mapping[str, Instance],
    edges: tuple[Edge, ...],
    component_of: Mapping[str, Component | None],
) -> dict[str, object]:
    repositories = tuple(names)
    components = {
        component.purl: component
        for component in component_of.values()
        if component is not None
    }
    versions = {
        (instance.component, instance.version)
        for instance in instances.values()
        if instance.version is not None
    }
    projects = 0
    instance_records = 0
    for repository in data:
        projects += len(repository.projects)
        instance_records += len(repository.instances)
    direct = sum(1 for edge in edges if edge.relation is Relation.DIRECT)
    transitive = sum(1 for edge in edges if edge.relation is Relation.TRANSITIVE)
    ecosystems: dict[str, dict[str, Metric]] = {}
    for ecosystem in Ecosystem:
        ecosystem_components = {
            component.purl
            for component in components.values()
            if component.ecosystem is ecosystem
        }
        ecosystem_instances = 0
        ecosystem_projects = 0
        ecosystem_versions: set[tuple[str, str | None]] = set()
        for repository in data:
            for project in repository.projects:
                if project.ecosystem is ecosystem:
                    ecosystem_projects += 1
            for instance in repository.instances:
                component = _component(component_of, instance.id)
                if component is None or component.ecosystem is not ecosystem:
                    continue
                ecosystem_instances += 1
                if instance.version is not None:
                    ecosystem_versions.add((instance.component, instance.version))
        ecosystem_edges = 0
        for edge in edges:
            component = _component(component_of, edge.to_id)
            if component is not None and component.ecosystem is ecosystem:
                ecosystem_edges += 1
        if not ecosystem_components and not ecosystem_instances and not ecosystem_projects:
            continue
        ecosystems[ecosystem.value] = {
            "projects": _metric(ecosystem_projects, "projects", "all projects"),
            "package_names": _metric(
                len(ecosystem_components), "package names", "all package names"
            ),
            "name_versions": _metric(
                len(ecosystem_versions), "name@version", "all name@version pairs"
            ),
            "instances": _metric(ecosystem_instances, "instances", "all instances"),
            "edges": _metric(ecosystem_edges, "edges", "all edges"),
        }
    return {
        "repositories": _metric(len(repositories), "repositories", "selected repositories"),
        "projects": _metric(projects, "projects", "selected repositories"),
        "package_names": _metric(len(components), "package names", "selected repositories"),
        "name_versions": _metric(len(versions), "name@version", "selected repositories"),
        "instances": _metric(instance_records, "instances", "selected repositories"),
        "direct_edges": _metric(direct, "edges", "all edges"),
        "transitive_edges": _metric(transitive, "edges", "all edges"),
        "ecosystems": ecosystems,
    }


def _concentration(
    graph: Graph,
    names: Iterable[str],
    instances: Mapping[str, Instance],
    edges: tuple[Edge, ...],
    component_of: Mapping[str, Component | None],
    top: int,
) -> dict[str, object]:
    direct: dict[str, set[str]] = defaultdict(set)
    any_edge: dict[str, set[str]] = defaultdict(set)
    for edge in edges:
        if edge.to_id not in instances:
            continue
        component = _component(component_of, edge.to_id)
        if component is None:
            continue
        repository = _edge_repository(graph, edge)
        any_edge[component.purl].add(repository)
        if edge.relation is Relation.DIRECT:
            direct[component.purl].add(repository)
    repositories = tuple(names)
    rows = []
    for purl, component in graph.components.items():
        if purl not in any_edge:
            continue
        rows.append(
            {
                "component": purl,
                "name": component.name,
                "ecosystem": component.ecosystem.value,
                "direct_repositories": _metric(
                    len(direct[purl]), "repositories", "selected repositories"
                ),
                "all_repositories": _metric(
                    len(any_edge[purl]), "repositories", "selected repositories"
                ),
                "transitive_only_repositories": _metric(
                    len(any_edge[purl] - direct[purl]), "repositories", "selected repositories"
                ),
            }
        )
    rows.sort(
        key=lambda row: (
            -_value(row["direct_repositories"]),
            -_value(row["all_repositories"]),
            str(row["component"]).casefold(),
        )
    )
    return {
        "package_names": _metric(len(rows), "package names", "selected repositories"),
        "repositories": _metric(len(repositories), "repositories", "selected repositories"),
        "top": top,
        "rows": rows,
    }


def _divergence(
    graph: Graph,
    instances: Mapping[str, Instance],
    edges: tuple[Edge, ...],
    component_of: Mapping[str, Component | None],
) -> dict[str, object]:
    versions: dict[str, set[str]] = defaultdict(set)
    sources: dict[tuple[str, str], set[str]] = defaultdict(set)
    repositories: dict[tuple[str, str], set[str]] = defaultdict(set)
    consumers: dict[str, set[str]] = defaultdict(set)
    within: dict[tuple[str, str], set[str]] = defaultdict(set)
    for instance in instances.values():
        component = _component(component_of, instance.id)
        if component is None or instance.version is None:
            continue
        memberships = graph.instance_repositories.get(instance.id, frozenset())
        versions[component.purl].add(instance.version)
        sources[(component.purl, instance.version)].add(instance.source_kind.value)
        for repository in memberships:
            repositories[(component.purl, instance.version)].add(repository)
            within[(repository, component.purl)].add(instance.version)
    for edge in edges:
        instance = instances.get(edge.to_id)
        if instance is None:
            continue
        consumers[instance.component].add(_edge_repository(graph, edge))
    fleet = [
        _divergence_row(purl, versions[purl], sources, repositories, consumers)
        for purl in sorted(versions)
        if len(versions[purl]) > 1
    ]
    repository_rows = [
        {
            "repository": repository,
            "component": purl,
            "versions": sorted(found),
        }
        for (repository, purl), found in sorted(within.items())
        if len(found) > 1
    ]
    return {
        "fleet": _metric(len(fleet), "package names", "selected package names"),
        "within_repository": _metric(
            len(repository_rows), "repository packages", "selected repositories"
        ),
        "packages": fleet,
        "repositories": repository_rows,
    }


def _divergence_row(
    purl: str,
    found: set[str],
    sources: Mapping[tuple[str, str], set[str]],
    repositories: Mapping[tuple[str, str], set[str]],
    consumers: Mapping[str, set[str]],
) -> dict[str, object]:
    return {
        "component": purl,
        "versions": [
            {
                "version": version,
                "sources": sorted(sources[(purl, version)]),
                "repositories": sorted(repositories[(purl, version)]),
            }
            for version in sorted(found)
        ],
        "consumers": _metric(
            len(consumers[purl]), "repositories", "selected repositories"
        ),
    }


def _source_risk(
    instances: Mapping[str, Instance],
    edges: tuple[Edge, ...],
    component_of: Mapping[str, Component | None],
    issues: tuple[Issue, ...],
) -> dict[str, object]:
    incoming: dict[str, list[Edge]] = defaultdict(list)
    for edge in edges:
        incoming[edge.to_id].append(edge)
    git = 0
    vendored = 0
    actions: list[Instance] = []
    images: list[Instance] = []
    artifacts: list[Instance] = []
    for instance in instances.values():
        if instance.source_kind is SourceKind.GIT:
            git += 1
        if instance.source_kind is SourceKind.VENDORED:
            vendored += 1
        component = _component(component_of, instance.id)
        if component is None:
            continue
        if component.ecosystem is Ecosystem.GITHUB:
            actions.append(instance)
        elif component.ecosystem is Ecosystem.DOCKER:
            images.append(instance)
        elif (
            instance.source_kind is SourceKind.URL
            and component.ecosystem is Ecosystem.GENERIC
        ):
            artifacts.append(instance)
    unpinned_actions = sum(
        1
        for instance in actions
        if not any(
            edge.resolution is Resolution.RESOLVED for edge in incoming.get(instance.id, ())
        )
    )
    floating_images = sum(1 for instance in images if "digest=" not in instance.purl)
    unpinned_artifacts = sum(
        1
        for instance in artifacts
        if not any(
            edge.resolution is Resolution.RESOLVED for edge in incoming.get(instance.id, ())
        )
    )
    gaps = Counter(issue.kind.value for issue in issues if issue.kind.severity == "gap")
    errors = Counter(issue.kind.value for issue in issues if issue.kind.severity == "error")
    missing = sum(1 for issue in issues if issue.kind is CoverageKind.MISSING_LOCK)
    return {
        "git_sources": _metric(git, "instances", "all instances"),
        "unpinned_actions": _metric(unpinned_actions, "instances", "GitHub Actions instances"),
        "floating_images": _metric(floating_images, "instances", "container image instances"),
        "missing_locks": _metric(missing, "issues", "all coverage issues"),
        "unpinned_artifacts": _metric(
            unpinned_artifacts, "instances", "download artifact instances"
        ),
        "vendored": _metric(vendored, "instances", "all instances"),
        "coverage_gaps": dict(sorted(gaps.items())),
        "coverage_errors": dict(sorted(errors.items())),
        "issues": _metric(len(issues), "issues", "selected repositories"),
    }


def _scopes(edges: tuple[Edge, ...]) -> dict[str, object]:
    counts = Counter(edge.scope.value for edge in edges)
    return {
        "edges": _metric(len(edges), "edges", "all edges"),
        "by_scope": {
            scope.value: _metric(counts[scope.value], "edges", "all edges")
            for scope in Scope
            if counts[scope.value]
        },
    }


def _first_party(
    graph: Graph,
    instances: Mapping[str, Instance],
    edges: tuple[Edge, ...],
    component_of: Mapping[str, Component | None],
) -> dict[str, object]:
    linked = [edge for edge in edges if is_first_party_edge(graph, edge)]
    packages: dict[str, Component] = {}
    repositories: dict[str, set[str]] = defaultdict(set)
    for edge in linked:
        instance = instances.get(edge.to_id)
        if instance is None:
            continue
        component = _component(component_of, instance.id)
        if component is None:
            continue
        packages.setdefault(component.purl, component)
        repositories[component.purl].add(_edge_repository(graph, edge))
    return {
        "edges": _metric(len(linked), "edges", "all edges"),
        "package_names": _metric(len(packages), "package names", "selected package names"),
        "packages": [
            {
                "component": purl,
                "name": packages[purl].name,
                "ecosystem": packages[purl].ecosystem.value,
                "repositories": sorted(repositories[purl]),
            }
            for purl in sorted(packages)
        ],
    }


def _roles(
    graph: Graph,
    names: Iterable[str],
    instances: Mapping[str, Instance],
    component_of: Mapping[str, Component | None],
) -> dict[str, object]:
    http = set(_BANNED_RUST_HTTP)
    forks = ("h12tiny-", "laputa-")
    benchmarks = set(_BANNED_RUST_ALTERNATIVES) | set(_BANNED_RUST_ALTERNATIVES.values())
    roles: list[dict[str, object]] = []
    for repository in names:
        packages: set[str] = set()
        for instance in instances.values():
            if repository not in graph.instance_repositories.get(instance.id, frozenset()):
                continue
            component = _component(component_of, instance.id)
            if component is not None and component.ecosystem is Ecosystem.CARGO:
                packages.add(component.name)
        sets = {
            "http-client": sorted(
                name
                for name in packages
                if any(name == item or name.startswith(item + "-") for item in http)
            ),
            "http-client-fork": sorted(
                name
                for name in packages
                if name.startswith(forks) and any(item in name for item in http)
            ),
            "benchmark": sorted(name for name in packages if name in benchmarks),
        }
        for role, found in sets.items():
            if len(found) >= 2:
                roles.append({"repository": repository, "role": role, "packages": found})
    return {
        "repositories": _metric(
            len({str(role["repository"]) for role in roles}),
            "repositories",
            "selected repositories",
        ),
        "rows": roles,
    }


def report_text(value: Mapping[str, Any]) -> list[str]:
    lines: list[str] = []
    composition = _table(value["composition"])
    lines.append("composition")
    lines.extend(_metric_lines(composition, prefix="  "))
    ecosystems = composition.get("ecosystems")
    if isinstance(ecosystems, Mapping):
        for ecosystem, metrics in ecosystems.items():
            lines.append(f"  {ecosystem}")
            lines.extend(_metric_lines(_table(metrics), prefix="    "))
    concentration = _table(value["concentration"])
    rows = concentration.get("rows")
    lines.append("")
    lines.append(
        f"concentration (top {concentration['top']} of "
        f"{_value(concentration['package_names'])} package names)"
    )
    if isinstance(rows, list):
        for row in rows[: _value({"value": concentration["top"]})]:
            record = _table(row)
            lines.append(
                f"  {record['name']} ({record['ecosystem']}) "
                f"{_value(record['direct_repositories'])} direct, "
                f"{_value(record['all_repositories'])} total of "
                f"{_table(record['all_repositories'])['denominator']}"
            )
    divergence = _table(value["divergence"])
    lines.append("")
    lines.append(
        f"divergence: {_value(divergence['fleet'])} package names across the fleet, "
        f"{_value(divergence['within_repository'])} within one repository"
    )
    packages = divergence.get("packages")
    if isinstance(packages, list):
        for row in packages[:20]:
            record = _table(row)
            versions = ", ".join(
                str(_table(version)["version"])
                for version in _list(record.get("versions"))
            )
            lines.append(f"  {record['component']}: {versions}")
    risk = _table(value["source_risk"])
    lines.append("")
    lines.append("source risk")
    lines.extend(_metric_lines(risk, prefix="  "))
    lines.append("")
    lines.append("scopes")
    lines.extend(_metric_lines(_table(_table(value["scopes"]).get("by_scope")), prefix="  "))
    first = _table(value["first_party"])
    lines.append("")
    lines.append(
        f"first-party links: {_value(first['edges'])} edges, "
        f"{_value(first['package_names'])} package names"
    )
    roles = _table(value["roles"])
    lines.append("")
    lines.append("curated-role overlap")
    role_rows = roles.get("rows")
    if isinstance(role_rows, list) and role_rows:
        for row in role_rows:
            record = _table(row)
            lines.append(
                f"  {record['repository']} [{record['role']}]: "
                + ", ".join(str(item) for item in _list(record.get("packages")))
            )
    else:
        lines.append("  none")
    freshness = _table(value["freshness"])
    lines.append("")
    lines.append(f"freshness: {freshness['note']}")
    return lines


def _metric_lines(values: Mapping[str, Any], *, prefix: str) -> list[str]:
    lines: list[str] = []
    for key, value in values.items():
        if isinstance(value, Mapping) and "value" in value:
            metric = _table(value)
            lines.append(
                f"{prefix}{key}: {metric['value']} {metric['unit']} "
                f"(denominator: {metric['denominator']})"
            )
    return lines


def _table(value: object) -> Mapping[str, Any]:
    if isinstance(value, Mapping):
        return cast(Mapping[str, Any], value)
    return {}


def _list(value: object) -> list[Any]:
    return cast(list[Any], value) if isinstance(value, list) else []
