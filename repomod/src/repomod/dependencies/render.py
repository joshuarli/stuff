from __future__ import annotations

import html
import json
import shutil
import time
from collections import Counter, defaultdict
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any, cast
from urllib.parse import quote

from packaging.version import InvalidVersion, Version

from repomod.dependencies import analysis, enrich
from repomod.dependencies import diff as diff_module
from repomod.dependencies.graph import Graph, Inventory
from repomod.dependencies.model import (
    Ecosystem,
    Edge,
    IndexEntry,
    Instance,
    Project,
    Relation,
    RepositoryData,
)

MARKER = ".repomod-dashboard"
ASSETS = Path(__file__).with_name("assets")
_NAV = (
    ("index", "Overview"),
    ("landscape", "Landscape"),
    ("divergence", "Divergence"),
    ("packages", "Packages"),
    ("changes", "Changes"),
    ("coverage", "Coverage"),
)


@dataclass(frozen=True, slots=True)
class Dashboard:
    output: Path
    pages: tuple[Path, ...]
    bytes: int


@dataclass(frozen=True, slots=True)
class _Site:
    graph: Graph
    inventory: Inventory
    entries: Mapping[str, IndexEntry]
    cache: enrich.Enrichment
    report: Mapping[str, Any]
    diff_value: diff_module.Diff | None
    index: tuple[dict[str, object], ...]
    direct_repos: Mapping[str, frozenset[str]]
    all_repos: Mapping[str, frozenset[str]]
    now: float


def render(
    inventory: Inventory,
    graph: Graph,
    output: Path,
    *,
    root: Path | None = None,
    diff_value: diff_module.Diff | None = None,
) -> Dashboard:
    site = _site(inventory, graph, root, diff_value)
    for item in (output,):
        if item.is_symlink():
            raise ValueError(f"refusing to write a dashboard through a symlink: {item}")
    if output.exists():
        if not output.is_dir() or not (output / MARKER).is_file():
            raise ValueError(f"refusing to replace a directory that is not a dashboard: {output}")
        shutil.rmtree(output)
    output.mkdir(parents=True)
    (output / MARKER).write_text("repomod deps render\n", encoding="utf-8")
    pages: list[Path] = []
    pages.append(
        _write_page(
            output / "index.html",
            _page(site, "Dependency inventory", "index", "", _overview(site)),
        )
    )
    pages.append(
        _write_page(
            output / "landscape.html",
            _page(site, "Landscape", "landscape", "", _landscape(site), _landscape_data(site)),
        )
    )
    pages.append(
        _write_page(
            output / "divergence.html",
            _page(site, "Divergence", "divergence", "", _divergence(site)),
        )
    )
    pages.append(
        _write_page(
            output / "packages.html",
            _page(site, "Packages", "packages", "", _packages(site)),
        )
    )
    pages.append(
        _write_page(
            output / "changes.html",
            _page(site, "Changes", "changes", "", _changes(site)),
        )
    )
    pages.append(
        _write_page(
            output / "coverage.html",
            _page(site, "Coverage", "coverage", "", _coverage(site)),
        )
    )
    pages.extend(
        _write_page(
            output / "repos" / name / "index.html",
            _page(site, name, "repository", "../../../", _repository(site, name)),
        )
        for name in site.graph.repositories
    )
    _write_assets(output, site)
    total = sum(path.stat().st_size for path in output.rglob("*") if path.is_file())
    return Dashboard(output, tuple(pages), total)


def _site(
    inventory: Inventory,
    graph: Graph,
    root: Path | None,
    diff_value: diff_module.Diff | None,
) -> _Site:
    cache = enrich.load_cache()
    report_value = analysis.report(graph)
    index: list[dict[str, object]] = []
    for purl, component in sorted(graph.components.items()):
        index.append(
            {
                "purl": purl,
                "name": component.name,
                "ecosystem": component.ecosystem.value,
                "label": component.display_name,
            }
        )
    direct_repos: dict[str, set[str]] = defaultdict(set)
    all_repos: dict[str, set[str]] = defaultdict(set)
    for edge in graph.edges:
        instance = graph.instances.get(edge.to_id)
        if instance is None:
            continue
        component = graph.components.get(instance.component)
        if component is None:
            continue
        repository = _edge_repository(graph, edge)
        all_repos[component.purl].add(repository)
        if edge.relation is Relation.DIRECT:
            direct_repos[component.purl].add(repository)
    if diff_value is None and root is not None:
        try:
            diff_value = diff_module.diff_inventory(
                root, inventory.path, from_rev="HEAD~1"
            )
        except ValueError:
            diff_value = None
    return _Site(
        graph,
        inventory,
        inventory.entries,
        cache,
        cast(Mapping[str, Any], report_value),
        diff_value,
        tuple(index),
        {key: frozenset(value) for key, value in direct_repos.items()},
        {key: frozenset(value) for key, value in all_repos.items()},
        time.time(),
    )


def _page(
    site: _Site,
    title: str,
    page_id: str,
    prefix: str,
    body: str,
    data: object | None = None,
) -> str:
    data_script = (
        f'<script type="application/json" id="page-data">{_json(data)}</script>'
        if data is not None
        else ""
    )
    return (
        "<!doctype html>\n"
        '<html lang="en">\n<head>\n<meta charset="utf-8">\n'
        '<meta name="viewport" content="width=device-width, initial-scale=1">\n'
        f"<title>{_e(title)} · dependencies</title>\n"
        f'<link rel="stylesheet" href="{prefix}assets/app.css">\n'
        "</head>\n"
        f'<body data-page="{page_id}" data-prefix="{prefix}">\n'
        '<header class="site">\n'
        f'<a class="title" href="{prefix}index.html">Dependency inventory</a>\n'
        f"<nav>{_nav(prefix, page_id)}</nav>\n"
        '<input id="search" type="search" placeholder="search packages  /" '
        'autocomplete="off" aria-label="search packages">\n'
        '<div id="search-results"></div>\n'
        "</header>\n"
        f"<main>\n{body}\n</main>\n"
        f"{data_script}\n"
        f'<script src="{prefix}assets/search.js"></script>\n'
        f'<script src="{prefix}assets/app.js"></script>\n'
        "</body>\n</html>\n"
    )


def _nav(prefix: str, active: str) -> str:
    return "".join(
        f'<a href="{prefix}{page}.html"'
        + (' class="active"' if page == active else "")
        + f">{label}</a>"
        for page, label in _NAV
    )


def _write_page(path: Path, content: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


def _write_assets(output: Path, site: _Site) -> None:
    (output / "assets").mkdir(parents=True, exist_ok=True)
    (output / "assets" / "app.css").write_text(
        (ASSETS / "app.css").read_text(encoding="utf-8"), encoding="utf-8"
    )
    (output / "assets" / "app.js").write_text(
        (ASSETS / "app.js").read_text(encoding="utf-8"), encoding="utf-8"
    )
    template = (ASSETS / "search.js").read_text(encoding="utf-8")
    (output / "assets" / "search.js").write_text(
        template.replace("__DEPS_INDEX__", _json(list(site.index))), encoding="utf-8"
    )


def _overview(site: _Site) -> str:
    composition = _table(site.report["composition"])
    counts = Counter(data.state.value for data in site.graph.repositories.values())
    repositories = len(site.graph.repositories)
    projects = _value(composition.get("projects"))
    packages = _value(composition.get("package_names"))
    versions = _value(composition.get("name_versions"))
    instances = _value(composition.get("instances"))
    direct = _value(composition.get("direct_edges"))
    transitive = _value(composition.get("transitive_edges"))
    issues = sum(len(data.issues) for data in site.graph.repositories.values())
    body = [
        "<h1>Dependency inventory</h1>",
        f'<p class="sub">{repositories} repositories · {projects} projects · '
        f"{packages} package names · {versions} name@version · {instances} instances · "
        f"{direct + transitive} edges ({direct} direct, {transitive} transitive) · "
        f"{issues} coverage issues</p>",
        "<h2>Coverage</h2>",
        '<div class="scroll"><table><thead><tr><th>State</th><th class="num">Repositories</th>'
        '<th class="num">Share</th></tr></thead><tbody>',
    ]
    body.extend(
        f'<tr><td class="state-{state}">{_e(state)}</td>'
        f'<td class="num">{counts.get(state, 0)}</td>'
        f'<td>{_bar(counts.get(state, 0), repositories)}</td></tr>'
        for state in ("complete", "partial", "failed", "unavailable")
    )
    body.append("</tbody></table></div>")
    body.append(_issue_summary(site))
    body.append("<h2>Ecosystems</h2>")
    body.append(_ecosystem_table(composition))
    body.append("<h2>Prevalence</h2>")
    body.append(_prevalence(site))
    body.append("<h2>Divergence hotspots</h2>")
    body.append(_divergence_table(site, limit=10))
    body.append("<h2>Source risk</h2>")
    body.append(_risk_table(site))
    body.append("<h2>Curated-role overlap</h2>")
    body.append(_roles(site))
    body.append("<h2>Repositories</h2>")
    body.append(_repository_table(site))
    body.append("<h2>Recent changes</h2>")
    body.append(_changes_summary(site))
    return "\n".join(body)


def _issue_summary(site: _Site) -> str:
    counts: Counter[str] = Counter()
    totals: Counter[str] = Counter()
    for data in site.graph.repositories.values():
        for issue in data.issues:
            counts[issue.kind.value] += 1
            totals[issue.kind.severity] += 1
    if not counts:
        return '<p class="zero">no coverage issues</p>'
    rows = "".join(
        f"<tr><td>{_e(kind)}</td><td class=\"num\">{count}</td></tr>"
        for kind, count in sorted(counts.items(), key=lambda item: (-item[1], item[0]))
    )
    return (
        f'<p class="sub">{totals["error"]} error issues and {totals["gap"]} gap issues</p>'
        '<div class="scroll"><table><thead><tr><th>Issue kind</th>'
        f'<th class="num">Issues</th></tr></thead><tbody>{rows}</tbody></table></div>'
    )


def _ecosystem_table(composition: Mapping[str, Any]) -> str:
    ecosystems = composition.get("ecosystems")
    if not isinstance(ecosystems, Mapping) or not ecosystems:
        return '<p class="zero">no ecosystem records</p>'
    rows = []
    for ecosystem, metrics in ecosystems.items():
        record = _table(metrics)
        rows.append(
            "<tr>"
            f"<td>{_e(ecosystem)}</td>"
            f'<td class="num">{_value(record.get("projects"))}</td>'
            f'<td class="num">{_value(record.get("package_names"))}</td>'
            f'<td class="num">{_value(record.get("name_versions"))}</td>'
            f'<td class="num">{_value(record.get("instances"))}</td>'
            f'<td class="num">{_value(record.get("edges"))}</td>'
            "</tr>"
        )
    return (
        '<div class="scroll"><table><thead><tr><th>Ecosystem</th>'
        '<th class="num">Projects</th><th class="num">Package names</th>'
        '<th class="num">Name@version</th><th class="num">Instances</th>'
        '<th class="num">Edges</th></tr></thead><tbody>'
        + "".join(rows)
        + "</tbody></table></div>"
    )


def _prevalence(site: _Site) -> str:
    rows = _table(site.report["concentration"]).get("rows")
    if not isinstance(rows, list) or not rows:
        return '<p class="zero">no package edges</p>'
    repositories = len(site.graph.repositories)
    body = [
        '<div class="scroll"><table><thead><tr><th>Package</th><th>Ecosystem</th>'
        '<th class="num">Direct</th><th class="num">Total</th><th>Share</th></tr></thead><tbody>'
    ]
    for raw in rows[:20]:
        record = _table(raw)
        direct = _value(record.get("direct_repositories"))
        total = _value(record.get("all_repositories"))
        purl = str(record.get("component", ""))
        body.append(
            f"<tr><td>{_package_link(purl, '', str(record.get('name') or purl))}</td>"
            f"<td>{_e(record.get('ecosystem'))}</td>"
            f'<td class="num">{direct}</td><td class="num">{total}</td>'
            f"<td>{_bar(total, repositories)}</td></tr>"
        )
    body.append("</tbody></table></div>")
    return "".join(body)


def _divergence_table(site: _Site, *, limit: int | None = None) -> str:
    rows = _table(site.report["divergence"]).get("packages")
    if not isinstance(rows, list) or not rows:
        return '<p class="zero">no package diverges across the fleet</p>'
    selected = rows if limit is None else rows[:limit]
    body = [
        '<div class="scroll"><table><thead><tr><th>Package</th><th>Versions</th>'
        '<th class="num">Consumers</th></tr></thead><tbody>'
    ]
    for raw in selected:
        record = _table(raw)
        purl = str(record.get("component", ""))
        versions = record.get("versions")
        labels = ", ".join(
            str(_table(item).get("version")) for item in versions
        ) if isinstance(versions, list) else ""
        body.append(
            f"<tr><td>{_package_link(purl, '', purl)}</td>"
            f'<td class="versions">{_e(labels)}</td>'
            f'<td class="num">{_value(record.get("consumers"))}</td></tr>'
        )
    body.append("</tbody></table></div>")
    return "".join(body)


def _risk_table(site: _Site) -> str:
    risk = _table(site.report["source_risk"])
    instances = _value(_table(site.report["composition"]).get("instances"))
    issues = _value(risk.get("issues"))
    rows = [
        ("git sources", "git_sources", instances),
        ("unpinned Actions", "unpinned_actions", None),
        ("floating image tags", "floating_images", None),
        ("missing locks", "missing_locks", issues),
        ("unpinned artifacts", "unpinned_artifacts", None),
        ("vendored code", "vendored", instances),
    ]
    body = ["<table><thead><tr><th>Signal</th><th class=\"num\">Count</th><th>Share</th></tr></thead><tbody>"]
    for label, key, denominator in rows:
        value = _value(risk.get(key))
        if denominator is None:
            body.append(f'<tr><td>{_e(label)}</td><td class="num">{value}</td><td></td></tr>')
        else:
            body.append(
                f'<tr><td>{_e(label)}</td><td class="num">{value}</td>'
                f"<td>{_bar(value, denominator, 'gap')}</td></tr>"
            )
    body.append("</tbody></table>")
    out = ["".join(body)]
    gaps = risk.get("coverage_gaps")
    if isinstance(gaps, Mapping) and gaps:
        out.append("<p class=\"sub\">coverage gaps: " + ", ".join(
            f"{_e(kind)} {count}" for kind, count in gaps.items()
        ) + "</p>")
    errors = risk.get("coverage_errors")
    if isinstance(errors, Mapping) and errors:
        out.append("<p class=\"sub\">coverage errors: " + ", ".join(
            f"{_e(kind)} {count}" for kind, count in errors.items()
        ) + "</p>")
    return "".join(out)


def _roles(site: _Site) -> str:
    rows = _table(site.report["roles"]).get("rows")
    if not isinstance(rows, list) or not rows:
        return '<p class="zero">no curated-role overlap</p>'
    body = ["<table><thead><tr><th>Repository</th><th>Role</th><th>Packages</th></tr></thead><tbody>"]
    for raw in rows:
        record = _table(raw)
        packages = record.get("packages")
        packages_text = ", ".join(str(item) for item in packages) if isinstance(packages, list) else ""
        body.append(
            f"<tr><td>{_repo_link(str(record.get('repository')), '')}</td>"
            f"<td>{_e(record.get('role'))}</td>"
            f'<td class="mono">{_e(packages_text)}</td></tr>'
        )
    body.append("</tbody></table>")
    return "".join(body)


def _repository_table(site: _Site) -> str:
    body = [
        '<div class="scroll"><table><thead><tr><th>Repository</th><th>State</th>'
        '<th class="num">Projects</th><th class="num">Direct edges</th>'
        '<th class="num">Issues</th><th>Tags</th></tr></thead><tbody>'
    ]
    for name in sorted(site.graph.repositories, key=str.casefold):
        data = site.graph.repositories[name]
        direct = sum(1 for edge in data.edges if edge.relation is Relation.DIRECT)
        body.append(
            f'<tr data-row data-href="repos/{_e(name)}/index.html">'
            f"<td>{_repo_link(name, '')}</td>"
            f'<td class="state-{data.state.value}">{_e(data.state.value)}</td>'
            f'<td class="num">{len(data.projects)}</td>'
            f'<td class="num">{direct}</td>'
            f'<td class="num">{len(data.issues)}</td>'
            f"<td>{_tags(data.tags)}</td></tr>"
        )
    body.append("</tbody></table></div>")
    return "".join(body)


def _changes_summary(site: _Site) -> str:
    value = site.diff_value
    if value is None or not value.baseline:
        return '<p class="unknown">no baseline</p>'
    summary = _summary_metrics(value)
    return (
        f'<p class="sub">{summary}</p>'
        f'<p><a href="changes.html">all changes</a></p>'
    )


def _landscape(site: _Site) -> str:
    columns = _landscape_columns(site)
    repositories = sorted(site.graph.repositories, key=str.casefold)
    body = [
        "<h1>Landscape</h1>",
        '<p class="sub">Dense cells are repositories that depend on a package. '
        "Blue is direct; pale blue is transitive. Sort by commonality, ecosystem, "
        "repository, or coverage; add a column from search.</p>",
        '<p class="sub">Sort: '
        '<button class="copy" data-sort="commonality">commonality</button> '
        '<button class="copy" data-sort="ecosystem">ecosystem</button> '
        '<button class="copy" data-sort="repo">repository</button> '
        '<button class="copy" data-sort="coverage">coverage</button></p>',
        '<div class="scroll"><table class="matrix landscape"><thead><tr><th>Repository</th>',
    ]
    for purl in columns:
        component = site.graph.components.get(purl)
        label = component.display_name if component is not None else purl
        body.append(f'<th class="rotate" title="{_e(purl)}">{_e(label)}</th>')
    body.append("</tr></thead><tbody>")
    for name in repositories:
        data = site.graph.repositories[name]
        commonality = sum(
            1
            for purl in columns
            if name in site.all_repos.get(purl, frozenset())
        )
        coverage = {"failed": 3, "unavailable": 3, "partial": 2, "complete": 1}.get(
            data.state.value, 0
        )
        body.append(
            f'<tr data-row data-repo="{_e(name)}" data-href="repos/{_e(name)}/index.html" '
            f'data-commonality="{commonality}" data-ecosystem="{_e(",".join(data.tags))}" '
            f'data-coverage="{coverage}">'
            f"<td>{_repo_link(name, '')}</td>"
        )
        for purl in columns:
            if name in site.direct_repos.get(purl, frozenset()):
                body.append('<td class="cell direct" title="direct"></td>')
            elif name in site.all_repos.get(purl, frozenset()):
                body.append('<td class="cell transitive" title="transitive"></td>')
            else:
                body.append('<td class="cell"></td>')
        body.append("</tr>")
    body.append("</tbody></table></div>")
    return "".join(body)


def _landscape_data(site: _Site) -> dict[str, object]:
    membership: dict[str, dict[str, list[str]]] = {}
    labels: dict[str, str] = {}
    for purl, component in site.graph.components.items():
        membership[purl] = {
            "direct": sorted(site.direct_repos.get(purl, frozenset())),
            "transitive": sorted(site.all_repos.get(purl, frozenset())),
        }
        labels[purl] = component.display_name
    return {"membership": membership, "labels": labels}


def _landscape_columns(site: _Site) -> tuple[str, ...]:
    ranked = sorted(
        site.all_repos,
        key=lambda purl: (
            -len(site.all_repos[purl]),
            -len(site.direct_repos.get(purl, frozenset())),
            purl,
        ),
    )
    return tuple(ranked[:60])


def _divergence(site: _Site) -> str:
    divergence = _table(site.report["divergence"])
    packages = divergence.get("packages")
    body = [
        "<h1>Divergence</h1>",
        f'<p class="sub">{_value(divergence.get("fleet"))} package names carry more than one '
        f'version across the fleet; {_value(divergence.get("within_repository"))} repository '
        "packages carry more than one version.</p>",
    ]
    if isinstance(packages, list) and packages:
        body.append("<table><thead><tr><th>Package</th><th>Versions</th><th>Dots</th></tr></thead><tbody>")
        for raw in packages:
            record = _table(raw)
            purl = str(record.get("component", ""))
            ecosystem = _ecosystem_of(purl)
            versions = record.get("versions")
            if not isinstance(versions, list):
                continue
            ordered = sorted(
                (_table(item) for item in versions),
                key=lambda item: _version_sort_key(ecosystem, str(item.get("version", ""))),
            )
            dots = "".join(
                f'<span class="dot" style="--x:{index}" title="{_e(_table(item).get("version"))} · '
                f'{len(_list(_table(item).get("repositories")))} repositories"></span>'
                for index, item in enumerate(ordered)
            )
            labels = ", ".join(
                f"{_table(item).get('version')} ({len(_list(_table(item).get('repositories')))})"
                for item in ordered
            )
            body.append(
                f"<tr><td>{_package_link(purl, '', purl)}</td>"
                f'<td class="versions">{_e(labels)}</td>'
                f'<td><span class="dots">{dots}</span></td></tr>'
            )
        body.append("</tbody></table>")
    else:
        body.append('<p class="zero">no package diverges across the fleet</p>')
    within = _table(site.report["divergence"]).get("repositories")
    body.append("<h2>Within one repository</h2>")
    if isinstance(within, list) and within:
        body.append(
            "<table><thead><tr><th>Repository</th><th>Package</th><th>Versions</th></tr></thead><tbody>"
        )
        for raw in within:
            record = _table(raw)
            body.append(
                f"<tr><td>{_repo_link(str(record.get('repository')), '')}</td>"
                f"<td>{_package_link(str(record.get('component')), '', str(record.get('component')))}</td>"
                f'<td class="versions">{_e(", ".join(str(item) for item in _list(record.get("versions"))))}</td></tr>'
            )
        body.append("</tbody></table>")
    else:
        body.append('<p class="zero">no repository carries two versions of one package</p>')
    return "".join(body)


def _packages(site: _Site) -> str:
    body = ["<h1>Packages</h1>", '<div id="package-index">']
    body.append(
        "<p class=\"sub\">Pick a package by search or from the list. "
        "Each section shows identity, versions, sources, consumers, entry paths, "
        "and enrichment state.</p>"
    )
    body.append(
        '<div class="scroll"><table><thead><tr><th>Package</th><th>Ecosystem</th>'
        '<th>Versions</th><th class="num">Direct repositories</th></tr></thead><tbody>'
    )
    for purl in sorted(site.graph.components, key=str.casefold):
        component = site.graph.components[purl]
        versions = sorted(
            {
                instance.version
                for instance in site.graph.instances.values()
                if instance.component == purl and instance.version is not None
            },
            key=lambda item: _version_sort_key(component.ecosystem, item),
        )
        body.append(
            f'<tr data-row data-href="packages.html#pkg={_fragment(purl)}">'
            f"<td>{_package_link(purl, '', component.display_name)}</td>"
            f"<td>{_e(component.ecosystem.value)}</td>"
            f'<td class="versions">{_e(", ".join(versions))}</td>'
            f'<td class="num">{len(site.direct_repos.get(purl, frozenset()))}</td></tr>'
        )
    body.append("</tbody></table></div></div>")
    body.extend(
        _package_section(site, purl)
        for purl in sorted(site.graph.components, key=str.casefold)
    )
    return "".join(body)


def _package_section(site: _Site, purl: str) -> str:
    component = site.graph.components[purl]
    instances = sorted(
        (item for item in site.graph.instances.values() if item.component == purl),
        key=lambda item: (item.version or "", item.id),
    )
    consumers: list[tuple[str, Edge]] = []
    for instance in instances:
        consumers.extend(
            (_edge_repository(site.graph, edge), edge)
            for edge in site.graph.incoming.get(instance.id, ())
        )
    consumer_total = len(consumers)
    consumers = consumers[:20]
    body = [
        f'<section class="package" data-purl="{_e(purl)}">',
        f"<h2>{_e(component.display_name)}</h2>",
        f'<p class="sub mono">{_e(purl)} '
        f'<button class="copy" data-copy="{_e(purl)}">copy</button></p>',
        f'<p class="sub">{_e(component.ecosystem.value)}'
        + (" · first-party" if any(instance.first_party for instance in instances) else "")
        + f" · {len(instances)} instance(s)</p>",
        "<h3>Versions</h3>",
        '<div class="scroll"><table><thead><tr><th>Version</th><th>Source</th>'
        '<th>Repositories</th><th>Enrichment</th></tr></thead><tbody>',
    ]
    for instance in instances:
        repositories = sorted(
            site.graph.instance_repositories.get(instance.id, frozenset())
        )
        body.append(
            f"<tr><td class=\"mono\">{_e(instance.version or 'unknown')}</td>"
            f"<td>{_source_label(instance)}</td>"
            f"<td>{_e(', '.join(repositories) or '—')}</td>"
            f"<td>{_enrichment_cell(site, instance)}</td></tr>"
        )
    body.append("</tbody></table></div>")
    body.append("<h3>Entry paths</h3>")
    if consumer_total > len(consumers):
        body.append(
            f'<p class="sub">showing {len(consumers)} of {consumer_total} consumers</p>'
        )
    if consumers:
        body.append('<div class="scroll"><table><thead><tr><th>Repository</th><th>From</th>'
                    '<th>Relation</th><th>Scope</th><th>Resolution</th></tr></thead><tbody>')
        for repository, edge in consumers:
            body.append(
                f"<tr><td>{_repo_link(repository, '')}</td>"
                f"<td>{_e(_edge_label(site.graph, edge))}</td>"
                f'<td class="{"direct" if edge.relation is Relation.DIRECT else "transitive"}">'
                f"{_e(edge.relation.value)}</td>"
                f"<td>{_e(edge.scope.value)}</td>"
                f"<td>{_e(edge.resolution.value)}</td></tr>"
            )
        body.append("</tbody></table></div>")
    else:
        body.append('<p class="zero">no consumers</p>')
    body.append("</section>")
    return "".join(body)


def _changes(site: _Site) -> str:
    value = site.diff_value
    body = ["<h1>Changes</h1>"]
    if value is None or not value.baseline:
        body.append(
            '<p class="unknown">no baseline: the previous revision has no dependency inventory</p>'
        )
        return "".join(body)
    body.append(f'<p class="sub">{_summary_metrics(value)}</p>')
    for change in value.repositories:
        body.append(f"<h2>{_repo_link(change.repository, '')} <span class=\"sub\">{_e(change.status)}</span></h2>")
        body.append(
            f'<p class="sub mono" style="font-size:12px">{_e(change.old_commit or "?")} → '
            f"{_e(change.new_commit or '?')}</p>"
        )
        if change.added:
            body.append("<h3>Added</h3><ul>")
            body.extend(f'<li class="mono">+ {_e(item.purl)}</li>' for item in change.added)
            body.append("</ul>")
        if change.removed:
            body.append("<h3>Removed</h3><ul>")
            body.extend(f'<li class="mono">- {_e(item.purl)}</li>' for item in change.removed)
            body.append("</ul>")
        if change.versions:
            body.append("<h3>Versions</h3><ul>")
            body.extend(
                f'<li><span class="mono">{_e(item.name)}</span>: '
                f"{_e(item.old or '—')} → {_e(item.new or '—')} "
                f'<span class="{"risk" if item.direction != "upgrade" else "direct"}">'
                f"{_e(item.direction)}</span></li>"
                for item in change.versions
            )
            body.append("</ul>")
        if change.sources:
            body.append("<h3>Sources</h3><ul>")
            body.extend(
                f'<li><span class="mono">{_e(item.component)}</span>: '
                f"{_e(item.old.value)} → {_e(item.new.value)}</li>"
                for item in change.sources
            )
            body.append("</ul>")
        if change.new_issues or change.resolved_issues:
            body.append("<h3>Issues</h3><ul>")
            body.extend(
                f'<li class="finding">+ {_e(item.kind.value)} {_e(item.path or "")}</li>'
                for item in change.new_issues
            )
            body.extend(
                f'<li>- {_e(item.kind.value)} {_e(item.path or "")}</li>'
                for item in change.resolved_issues
            )
            body.append("</ul>")
    return "".join(body)


def _coverage(site: _Site) -> str:
    counts: Counter[str] = Counter()
    for data in site.graph.repositories.values():
        for issue in data.issues:
            counts[issue.kind.value] += 1
    body = [
        "<h1>Coverage</h1>",
        '<p class="sub">Every recognized input is either parsed or recorded as a typed '
        "coverage issue. A repository is complete with no issues, partial with only gaps, "
        "failed with an error issue, and unavailable when its checkout cannot be read.</p>",
        "<h2>Repositories</h2>",
        '<div class="scroll"><table><thead><tr><th>Repository</th><th>State</th>'
        '<th class="num">Projects</th><th class="num">Instances</th>'
        '<th class="num">Issues</th><th>Issue kinds</th></tr></thead><tbody>',
    ]
    for name in sorted(site.graph.repositories, key=str.casefold):
        data = site.graph.repositories[name]
        kinds = Counter(issue.kind.value for issue in data.issues)
        body.append(
            f'<tr data-row data-href="repos/{_e(name)}/index.html">'
            f"<td>{_repo_link(name, '')}</td>"
            f'<td class="state-{data.state.value}">{_e(data.state.value)}</td>'
            f'<td class="num">{len(data.projects)}</td>'
            f'<td class="num">{len(data.instances)}</td>'
            f'<td class="num">{len(data.issues)}</td>'
            f'<td class="mono">{_e(", ".join(f"{kind} {count}" for kind, count in sorted(kinds.items())))}</td></tr>'
        )
    body.append("</tbody></table></div>")
    body.append("<h2>Issue kinds</h2>")
    body.append(
        '<table><thead><tr><th>Kind</th><th class="num">Issues</th></tr></thead><tbody>'
        + "".join(
            f'<tr><td>{_e(kind)}</td><td class="num">{count}</td></tr>'
            for kind, count in sorted(counts.items(), key=lambda item: (-item[1], item[0]))
        )
        + "</tbody></table>"
    )
    unsupported = [
        (name, issue.path or "", issue.message)
        for name, data in site.graph.repositories.items()
        for issue in data.issues
        if issue.kind.value == "unsupported_format"
    ]
    body.append("<h2>Detection-only formats</h2>")
    if unsupported:
        body.append(
            '<div class="scroll"><table><thead><tr><th>Repository</th><th>Path</th>'
            '<th>Message</th></tr></thead><tbody>'
            + "".join(
                f"<tr><td>{_repo_link(name, '')}</td>"
                f'<td class="mono">{_e(path)}</td><td>{_e(message)}</td></tr>'
                for name, path, message in unsupported
            )
            + "</tbody></table></div>"
        )
    else:
        body.append('<p class="zero">none</p>')
    return "".join(body)


def _repository(site: _Site, name: str) -> str:
    data = site.graph.repositories[name]
    entry = site.entries.get(name)
    commit = entry.commit if entry is not None else None
    body = [
        f"<h1>{_e(name)}</h1>",
        f'<p class="sub"><a href="../../index.html">← overview</a> · '
        f'<span class="state-{data.state.value}">{_e(data.state.value)}</span> · '
        f"commit <span class=\"mono\">{_e(commit or 'unknown')}</span></p>",
        f'<p>{_tags(data.tags)}</p>',
    ]
    if not data.edges:
        body.append(
            '<p class="zero">complete: zero dependencies</p>'
            if data.state.value == "complete"
            else '<p class="unknown">no dependency edges recorded</p>'
        )
    projects = {project.id: project for project in data.projects}
    body.append("<h2>Projects</h2>")
    if projects:
        body.append(
            '<div class="scroll"><table><thead><tr><th>Project</th><th>Ecosystem</th>'
            '<th>Directory</th><th class="num">Direct edges</th></tr></thead><tbody>'
        )
        for project in sorted(projects.values(), key=lambda item: item.id):
            direct = sum(
                1
                for edge in data.edges
                if edge.from_id == project.id and edge.relation is Relation.DIRECT
            )
            body.append(
                f'<tr><td class="mono">{_e(project.id)}</td>'
                f"<td>{_e(project.ecosystem.value)}</td>"
                f'<td class="mono">{_e(project.directory)}</td>'
                f'<td class="num">{direct}</td></tr>'
            )
        body.append("</tbody></table></div>")
    else:
        body.append('<p class="zero">no package projects</p>')
    scopes = Counter(edge.scope.value for edge in data.edges)
    body.append("<h2>Scope</h2>")
    body.append(
        '<table><thead><tr><th>Scope</th><th class="num">Edges</th></tr></thead><tbody>'
        + "".join(
            f'<tr><td>{_e(scope)}</td><td class="num">{count}</td></tr>'
            for scope, count in sorted(scopes.items())
        )
        + "</tbody></table>"
    )
    body.append("<h2>Direct dependencies</h2>")
    direct_edges = [edge for edge in data.edges if edge.relation is Relation.DIRECT]
    if direct_edges:
        body.append(
            '<div class="scroll"><table><thead><tr><th>Project</th><th>Dependency</th>'
            '<th>Scope</th><th>Constraint</th><th>Resolution</th><th>Evidence</th>'
            "</tr></thead><tbody>"
        )
        for edge in sorted(direct_edges, key=lambda item: (item.from_id, item.to_id, item.scope.value)):
            target = site.graph.instances.get(edge.to_id)
            transitive = [
                item
                for item in data.edges
                if item.from_id == edge.to_id and item.relation is Relation.TRANSITIVE
            ]
            details = ""
            if transitive:
                details = (
                    "<details><summary>transitive</summary><ul>"
                    + "".join(
                        f'<li class="mono">{_e(_edge_label(site.graph, item))}</li>'
                        for item in sorted(transitive, key=lambda item: item.to_id)[:8]
                    )
                    + "</ul></details>"
                )
            target_cell = (
                _package_link(target.component, "../../../", site.graph.label(edge.to_id))
                if target is not None
                else _e(edge.to_id)
            )
            body.append(
                f'<tr data-row><td class="mono">{_e(_project_label(projects, edge.from_id))}</td>'
                f'<td class="mono">{target_cell}{details}</td>'
                f"<td>{_e(edge.scope.value)}</td>"
                f'<td class="mono">{_e(edge.constraint or "")}</td>'
                f"<td>{_e(edge.resolution.value)}</td>"
                f"<td>{_evidence(site, name, commit, edge)}</td></tr>"
            )
        body.append("</tbody></table></div>")
    else:
        body.append('<p class="zero">no direct dependencies</p>')
    sources = Counter(instance.source_kind.value for instance in data.instances)
    body.append("<h2>Sources</h2>")
    body.append(
        '<div class="scroll"><table><thead><tr><th>Source</th>'
        '<th class="num">Instances</th></tr></thead><tbody>'
        + "".join(
            f'<tr><td>{_e(source)}</td><td class="num">{count}</td></tr>'
            for source, count in sorted(sources.items())
        )
        + "</tbody></table></div>"
    )
    duplicates = _duplicates(site, data)
    body.append("<h2>Duplicates</h2>")
    if duplicates:
        body.append(
            '<div class="scroll"><table><thead><tr><th>Package</th><th>Versions</th>'
            "</tr></thead><tbody>"
            + "".join(
                f"<tr><td>{_package_link(purl, '../../../', purl)}</td>"
                f'<td class="versions">{_e(", ".join(versions))}</td></tr>'
                for purl, versions in duplicates
            )
            + "</tbody></table></div>"
        )
    else:
        body.append('<p class="zero">no duplicate versions</p>')
    body.append("<h2>Gaps and findings</h2>")
    if data.issues:
        body.append(
            '<div class="scroll"><table><thead><tr><th>Kind</th><th>Path</th>'
            '<th>Message</th></tr></thead><tbody>'
            + "".join(
                f'<tr><td class="{"finding" if issue.kind.severity == "error" else "risk"}">'
                f"{_e(issue.kind.value)}</td>"
                f"<td>{_evidence_path(name, commit, issue.path)}</td>"
                f"<td>{_e(issue.message)}</td></tr>"
                for issue in sorted(
                    data.issues, key=lambda item: (item.path or "", item.kind.value, item.message)
                )
            )
            + "</tbody></table></div>"
        )
    else:
        body.append('<p class="zero">no coverage issues</p>')
    return "".join(body)


def _duplicates(site: _Site, data: RepositoryData) -> list[tuple[str, list[str]]]:
    versions: dict[str, set[str]] = defaultdict(set)
    for instance in data.instances:
        if instance.version is not None:
            versions[instance.component].add(instance.version)
    return [
        (purl, sorted(found, key=lambda item: _version_sort_key(_ecosystem_of(purl), item)))
        for purl, found in sorted(versions.items())
        if len(found) > 1
    ]


def _enrichment_cell(site: _Site, instance: Instance) -> str:
    entry = site.cache.results.get(instance.purl)
    if entry is None:
        return '<span class="unknown">not checked</span>'
    if not enrich.is_fresh(entry, site.now):
        return '<span class="unknown">stale</span>'
    latest = entry.get("latest")
    vulns = entry.get("vulns")
    parts = []
    if isinstance(latest, str):
        parts.append(f"latest {_e(latest)}")
    if isinstance(vulns, list) and vulns:
        ids = ", ".join(
            str(item.get("id")) for item in vulns if isinstance(item, dict)
        )
        parts.append(f'<span class="finding">{len(vulns)} vulnerabilities: {_e(ids)}</span>')
    else:
        parts.append("no known vulnerabilities")
    return "; ".join(parts)


def _source_label(instance: Instance) -> str:
    label = instance.source_kind.value
    url = instance.registry_url or instance.vcs_url or instance.repository_url
    if url is not None:
        return f'{_e(label)} <span class="mono">{_e(url)}</span>'
    if instance.commit is not None:
        return f'{_e(label)} <span class="mono">{_e(instance.commit)}</span>'
    return _e(label)


def _evidence(
    site: _Site, repository: str, commit: str | None, edge: Edge
) -> str:
    if not edge.evidence:
        return ""
    return ", ".join(
        _evidence_path(repository, commit, item.path, item.locator) for item in edge.evidence
    )


def _evidence_path(
    repository: str, commit: str | None, path: str | None, locator: str | None = None
) -> str:
    if path is None:
        return ""
    label = _e(path) + (f" <span class=\"mono\">{_e(locator)}</span>" if locator is not None and locator != "" else "")
    if commit is None:
        return label
    url = (
        f"https://github.com/{quote(repository, safe='/')}/blob/{quote(commit, safe='')}/"
        f"{quote(path, safe='/')}"
    )
    return f'<a href="{_e(url)}">{label}</a>'


def _summary_metrics(value: diff_module.Diff) -> str:
    summary = diff_module.diff_dict(value)["summary"]
    record = _table(summary)
    keys = (
        "repositories",
        "instances_added",
        "instances_removed",
        "versions_upgraded",
        "versions_downgraded",
        "versions_changed",
        "sources_changed",
        "issues_new",
        "issues_resolved",
    )
    if sum(_value(record.get(key)) for key in keys) == 0:
        return "no changes"
    parts = [
        f"{_value(_table(record.get(key)))} {_table(record.get(key)).get('unit')}"
        for key in keys
        if _value(_table(record.get(key))) > 0
    ]
    return " · ".join(parts) if parts else "no changes"


def _tags(tags: Sequence[str]) -> str:
    return "".join(f'<span class="tag">{_e(tag)}</span>' for tag in tags)


def _repo_link(name: str, prefix: str) -> str:
    return f'<a href="{prefix}repos/{_e(name)}/index.html" class="mono">{_e(name)}</a>'


def _package_link(purl: str, prefix: str, text: str) -> str:
    return f'<a href="{prefix}packages.html#pkg={_fragment(purl)}" class="mono">{_e(text)}</a>'


def _fragment(purl: str) -> str:
    return _e(quote(purl, safe=""))


def _bar(value: int, denominator: int, kind: str = "") -> str:
    if denominator <= 0 or value <= 0:
        return ""
    width = max(0, min(100, value / denominator * 100))
    return f'<span class="bar {kind}" style="width:{width:.1f}%"></span>'


def _project_label(projects: Mapping[str, Project], node: str) -> str:
    project = projects.get(node)
    return project.id if project is not None else node


def _edge_repository(graph: Graph, edge: Edge) -> str:
    if edge.from_id in graph.projects:
        return graph.project_repositories.get(edge.from_id, "")
    repositories = sorted(graph.instance_repositories.get(edge.from_id, frozenset[str]()))
    return repositories[0] if repositories else ""


def _edge_label(graph: Graph, edge: Edge) -> str:
    return f"{graph.label(edge.from_id)} → {graph.label(edge.to_id)}"


def _ecosystem_of(purl: str) -> Ecosystem:
    value = purl.removeprefix("pkg:").split("/", 1)[0]
    try:
        return Ecosystem(value)
    except ValueError:
        return Ecosystem.GENERIC


def _version_sort_key(ecosystem: Ecosystem, version: str) -> tuple[int, object]:
    if ecosystem is Ecosystem.PYPI:
        try:
            return (0, Version(version))
        except InvalidVersion:
            return (1, version)
    parts: list[int] = []
    for piece in version.split("."):
        digits = ""
        for character in piece:
            if not character.isdigit():
                break
            digits += character
        if not digits:
            break
        parts.append(int(digits))
    return (0, tuple(parts)) if parts else (1, version)


def _json(value: object) -> str:
    text = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return (
        text.replace("<", "\\u003c")
        .replace(">", "\\u003e")
        .replace("&", "\\u0026")
        .replace("\u2028", "\\u2028")
        .replace("\u2029", "\\u2029")
    )


def _e(value: object) -> str:
    return html.escape(str(value), quote=True)


def _table(value: object) -> Mapping[str, Any]:
    if isinstance(value, Mapping):
        return cast(Mapping[str, Any], value)
    return {}


def _list(value: object) -> list[Any]:
    return cast(list[Any], value) if isinstance(value, list) else []


def _value(value: object) -> int:
    if isinstance(value, Mapping):
        item = cast(Mapping[str, Any], value).get("value", 0)
        return item if isinstance(item, int) else 0
    return value if isinstance(value, int) else 0
