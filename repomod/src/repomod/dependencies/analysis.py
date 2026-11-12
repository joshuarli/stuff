from __future__ import annotations

import time
from collections import Counter, defaultdict
from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from typing import Any, cast

from repomod import dependency_roles
from repomod.catalog import LOCKFILE_IGNORE_PREFIX
from repomod.dependencies import enrich as enrich_module
from repomod.dependencies import insights
from repomod.dependencies.graph import Graph, RepositoryEdge, is_first_party_edge
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


@dataclass(frozen=True, slots=True)
class MissingLock:
    repository: str
    path: str | None
    message: str


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
    located = tuple(
        item for item in graph.edges if item.repository.casefold() in selected_names
    )
    edges = tuple(item.edge for item in located)
    repositories_of = {
        instance_id: frozenset(
            repository
            for repository in graph.instance_repositories.get(instance_id, frozenset())
            if repository.casefold() in selected_names
        )
        for instance_id in instances
    }
    component_of = {
        instance_id: graph.components.get(instance.component)
        for instance_id, instance in instances.items()
    }
    fork_names = frozenset(name for name in names if "fork" in graph.tags.get(name, ()))
    fleet_names = frozenset(names) - fork_names
    data = tuple(graph.repositories[name] for name in names)
    return {
        "repositories": list(names),
        "composition": _composition(names, data, instances, edges, component_of),
        "concentration": _concentration(graph, names, instances, located, component_of, top),
        "divergence": _divergence(graph, instances, located, component_of),
        "source_risk": _source_risk_for(
            graph, fleet_names, instances, located, component_of, repositories_of
        ),
        "source_risk_forks": _source_risk_for(
            graph, fork_names, instances, located, component_of, repositories_of
        ),
        "scopes": _scopes(edges),
        "first_party": _first_party(graph, instances, located, component_of),
        "roles": _roles(graph, names),
        "freshness": _freshness(data, component_of),
    }


def _source_risk_for(
    graph: Graph,
    group: frozenset[str],
    instances: Mapping[str, Instance],
    located: tuple[RepositoryEdge, ...],
    component_of: Mapping[str, Component | None],
    repositories_of: Mapping[str, frozenset[str]],
) -> dict[str, object]:
    ignored = (
        {str(repository.id).casefold(): repository.ignored_rules for repository in graph.catalog.repositories}
        if graph.catalog is not None
        else {}
    )
    issues: list[Issue] = []
    missing_locks: list[MissingLock] = []
    for name in sorted(group, key=str.casefold):
        for issue in graph.repositories[name].issues:
            if issue.kind is CoverageKind.MISSING_LOCK and _lock_ignored(
                issue.path, ignored.get(name.casefold(), frozenset())
            ):
                continue
            issues.append(issue)
            if issue.kind is CoverageKind.MISSING_LOCK:
                missing_locks.append(MissingLock(name, issue.path, issue.message))
    group_repositories = {
        instance_id: repositories & group for instance_id, repositories in repositories_of.items()
    }
    return _source_risk(
        {
            instance_id: instance
            for instance_id, instance in instances.items()
            if group_repositories[instance_id]
        },
        tuple(item for item in located if item.repository in group),
        component_of,
        tuple(issues),
        tuple(missing_locks),
        group_repositories,
    )


def _lock_ignored(path: str | None, ignored_rules: frozenset[str]) -> bool:
    if path is None:
        return False
    directory = path.rsplit("/", 1)[0] if "/" in path else "."
    return f"{LOCKFILE_IGNORE_PREFIX}{directory}" in ignored_rules


def _selected(repositories: Iterable[str] | None) -> set[str] | None:
    if repositories is None:
        return None
    return {repository.casefold() for repository in repositories}


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


_CHECKED = frozenset(
    {Ecosystem.CARGO, Ecosystem.PYPI, Ecosystem.NPM, Ecosystem.GOLANG}
)


def _freshness(
    data: tuple[RepositoryData, ...],
    component_of: Mapping[str, Component | None],
) -> dict[str, object]:
    cache = enrich_module.load_cache()
    now = time.time()
    checked = stale = not_checked = vulnerable = 0
    total = 0
    for repository in data:
        components = {component.purl: component for component in repository.components}
        for instance in repository.instances:
            total += 1
            component = components.get(instance.component)
            if (
                component is None
                or component.ecosystem not in _CHECKED
                or instance.version is None
            ):
                not_checked += 1
                continue
            entry = cache.results.get(instance.purl)
            if entry is None or not isinstance(entry.get("vulns"), list):
                not_checked += 1
            elif not enrich_module.is_fresh(entry, now):
                stale += 1
            else:
                checked += 1
                vulns = entry.get("vulns")
                if isinstance(vulns, list) and vulns:
                    vulnerable += 1
    return {
        "available": cache.fetched_at is not None,
        "fetched_at": cache.fetched_at,
        "total": _metric(total, "instances", "all instances"),
        "checked": _metric(checked, "instances", "all instances"),
        "stale": _metric(stale, "instances", "all instances"),
        "not_checked": _metric(not_checked, "instances", "all instances"),
        "vulnerable": _metric(vulnerable, "instances", "checked instances"),
    }


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
    edges: tuple[RepositoryEdge, ...],
    component_of: Mapping[str, Component | None],
    top: int,
) -> dict[str, object]:
    direct: dict[str, set[str]] = defaultdict(set)
    any_edge: dict[str, set[str]] = defaultdict(set)
    for repository, edge in ((item.repository, item.edge) for item in edges):
        if edge.to_id not in instances:
            continue
        component = _component(component_of, edge.to_id)
        if component is None:
            continue
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
    edges: tuple[RepositoryEdge, ...],
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
    for item in edges:
        instance = instances.get(item.edge.to_id)
        if instance is None:
            continue
        consumers[instance.component].add(item.repository)
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
    located: tuple[RepositoryEdge, ...],
    component_of: Mapping[str, Component | None],
    issues: tuple[Issue, ...],
    missing_locks: tuple[MissingLock, ...],
    repositories_of: Mapping[str, frozenset[str]],
) -> dict[str, object]:
    incoming: dict[str, list[Edge]] = defaultdict(list)
    evidence: dict[str, set[tuple[str, str]]] = defaultdict(set)
    for item in located:
        incoming[item.edge.to_id].append(item.edge)
        for entry in item.edge.evidence:
            evidence[item.edge.to_id].add((item.repository, entry.path))
    git: list[str] = []
    vendored: list[str] = []
    actions: list[str] = []
    images: list[str] = []
    artifacts: list[str] = []
    for instance_id, instance in instances.items():
        component = _component(component_of, instance_id)
        if instance.source_kind is SourceKind.GIT and (
            component is None or component.ecosystem is not Ecosystem.GITHUB
        ):
            git.append(instance_id)
        if instance.source_kind is SourceKind.VENDORED:
            vendored.append(instance_id)
        if component is None:
            continue
        if component.ecosystem is Ecosystem.GITHUB:
            actions.append(instance_id)
        elif component.ecosystem is Ecosystem.DOCKER:
            images.append(instance_id)
        elif (
            instance.source_kind is SourceKind.URL
            and component.ecosystem is Ecosystem.GENERIC
        ):
            artifacts.append(instance_id)
    unpinned_actions = [
        instance_id
        for instance_id in actions
        if not _has_resolved_edge(incoming.get(instance_id, ()))
    ]
    floating_images = [
        instance_id for instance_id in images if "digest=" not in instances[instance_id].purl
    ]
    unpinned_artifacts = [
        instance_id
        for instance_id in artifacts
        if not _has_resolved_edge(incoming.get(instance_id, ()))
    ]
    gaps = Counter(issue.kind.value for issue in issues if issue.kind.severity == "gap")
    errors = Counter(issue.kind.value for issue in issues if issue.kind.severity == "error")
    return {
        "git_sources": _metric(len(git), "instances", "all instances"),
        "unpinned_actions": _metric(len(unpinned_actions), "instances", "GitHub Actions instances"),
        "floating_images": _metric(len(floating_images), "instances", "container image instances"),
        "missing_locks": _metric(len(missing_locks), "issues", "all coverage issues"),
        "unpinned_artifacts": _metric(
            len(unpinned_artifacts), "instances", "download artifact instances"
        ),
        "vendored": _metric(len(vendored), "instances", "all instances"),
        "coverage_gaps": dict(sorted(gaps.items())),
        "coverage_errors": dict(sorted(errors.items())),
        "issues": _metric(len(issues), "issues", "selected repositories"),
        "details": {
            "git_sources": tuple(git),
            "unpinned_actions": tuple(unpinned_actions),
            "floating_images": tuple(floating_images),
            "missing_locks": tuple(
                {
                    "repository": lock.repository,
                    "path": lock.path,
                    "message": lock.message,
                }
                for lock in missing_locks
            ),
            "unpinned_artifacts": tuple(unpinned_artifacts),
            "vendored": tuple(vendored),
        },
        "locations": {
            "git_sources": _location_rows(git, instances, repositories_of, evidence),
            "unpinned_actions": _location_rows(
                unpinned_actions, instances, repositories_of, evidence
            ),
            "floating_images": _location_rows(
                floating_images, instances, repositories_of, evidence
            ),
            "unpinned_artifacts": _location_rows(
                unpinned_artifacts, instances, repositories_of, evidence
            ),
        },
    }


def _location_rows(
    instance_ids: Iterable[str],
    instances: Mapping[str, Instance],
    repositories_of: Mapping[str, frozenset[str]],
    evidence: Mapping[str, set[tuple[str, str]]],
) -> list[dict[str, str]]:
    rows: set[tuple[str, str, str]] = set()
    for instance_id in instance_ids:
        instance = instances[instance_id]
        repositories = repositories_of.get(instance_id, frozenset())
        pairs = {
            (repository, path)
            for repository, path in evidence.get(instance_id, set())
            if repository in repositories
        }
        if pairs:
            for repository, path in pairs:
                rows.add((repository, path, instance.purl))
        else:
            for repository in repositories:
                rows.add((repository, "", instance.purl))
    ordered = sorted(rows, key=lambda row: (row[0].casefold(), row[0], row[1], row[2]))
    return [
        {"repository": repository, "path": path, "purl": purl_value}
        for repository, path, purl_value in ordered
    ]


def _has_resolved_edge(edges: Iterable[Edge]) -> bool:
    return any(edge.resolution is Resolution.RESOLVED for edge in edges)


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
    edges: tuple[RepositoryEdge, ...],
    component_of: Mapping[str, Component | None],
) -> dict[str, object]:
    linked = [item for item in edges if is_first_party_edge(graph, item.edge)]
    packages: dict[str, Component] = {}
    repositories: dict[str, set[str]] = defaultdict(set)
    for item in linked:
        instance = instances.get(item.edge.to_id)
        if instance is None:
            continue
        component = _component(component_of, instance.id)
        if component is None:
            continue
        packages.setdefault(component.purl, component)
        repositories[component.purl].add(item.repository)
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


def _roles(graph: Graph, names: Iterable[str]) -> dict[str, object]:
    selected = set(names)
    found = tuple(
        declaration
        for declaration in insights.owned(graph, insights.declarations(graph))
        if declaration.repository in selected
    )
    roles: list[dict[str, object]] = [
        {
            "repository": repository,
            "role": use.role.name,
            "packages": sorted(
                package
                for (owner, _), packages in use.packages.items()
                if owner == repository
                for package in packages
            ),
        }
        for use in insights.role_uses(dependency_roles.load_roles(), found)
        for repository in use.overlapping
    ]
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
    lines.append("")
    lines.append("source risk")
    lines.extend(_risk_lines(_table(value["source_risk"])))
    lines.append("")
    lines.append("source risk in forks (upstream workflows stay outside local policy)")
    lines.extend(_risk_lines(_table(value["source_risk_forks"])))
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
    if freshness.get("available"):
        fetched_at = freshness.get("fetched_at")
        stamp = (
            time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(float(fetched_at)))
            if isinstance(fetched_at, (int, float))
            else "unknown"
        )
        lines.append(
            f"freshness: {_value(freshness['checked'])} checked, "
            f"{_value(freshness['stale'])} stale, "
            f"{_value(freshness['not_checked'])} not checked, "
            f"{_value(freshness['vulnerable'])} vulnerable of "
            f"{_value(freshness['total'])} instances (fetched_at {stamp})"
        )
    else:
        lines.append("freshness: not checked (run repomod deps enrich)")
    return lines


_LOCATION_KEYS = (
    "git_sources",
    "unpinned_actions",
    "floating_images",
    "unpinned_artifacts",
)


def _risk_lines(risk: Mapping[str, Any]) -> list[str]:
    lines: list[str] = []
    locations = _table(risk.get("locations"))
    details = _table(risk.get("details"))
    for key, value in risk.items():
        if not isinstance(value, Mapping) or "value" not in value:
            continue
        metric = _table(value)
        lines.append(
            f"  {key}: {metric['value']} {metric['unit']} "
            f"(denominator: {metric['denominator']})"
        )
        if key == "missing_locks":
            rows = sorted(
                (_table(item) for item in _list(details.get("missing_locks"))),
                key=lambda row: (
                    str(row.get("repository") or "").casefold(),
                    str(row.get("repository") or ""),
                    str(row.get("path") or ""),
                ),
            )
            for row in rows:
                fields = [str(row.get("repository") or "")]
                path = row.get("path")
                fields.append(path if isinstance(path, str) and path else str(row.get("message") or ""))
                lines.append(f"    {' '.join(fields)}")
        elif key in _LOCATION_KEYS:
            for item in _list(locations.get(key)):
                row = _table(item)
                fields = [str(row.get("repository") or "")]
                path = row.get("path")
                if isinstance(path, str) and path:
                    fields.append(path)
                fields.append(str(row.get("purl") or ""))
                lines.append(f"    {' '.join(fields)}")
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
    if isinstance(value, (list, tuple)):
        return list(value)
    return []
