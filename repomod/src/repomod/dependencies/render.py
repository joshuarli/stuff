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

from repomod.dependencies import analysis, enrich, insights
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
    ("outdated", "Outdated"),
    ("bloat", "Bloat"),
    ("alignment", "Alignment"),
    ("choices", "Choices"),
    ("packages", "Packages"),
    ("changes", "Changes"),
    ("coverage", "Coverage"),
)
_OVERVIEW_ROWS = 10
_BLOAT_ROWS = 150
_HEAVY_SOLE = 50


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
    now: float
    outdated: tuple[insights.Outdated, ...]
    vulnerable: tuple[insights.Vulnerable, ...]
    weights: tuple[insights.Weight, ...]
    splits: tuple[insights.Split, ...]
    roles: tuple[insights.RoleUse, ...]
    scorecards: tuple[insights.Scorecard, ...]


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
    pages.extend(
        _write_page(output / f"{page}.html", _page(site, title, page, "", body(site)))
        for page, title, body in (
            ("outdated", "Outdated", _outdated),
            ("bloat", "Bloat", _bloat),
            ("alignment", "Alignment", _alignment),
            ("choices", "Choices", _choices),
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
    for repository, edge in ((item.repository, item.edge) for item in graph.edges):
        instance = graph.instances.get(edge.to_id)
        if instance is None:
            continue
        component = graph.components.get(instance.component)
        if component is None:
            continue
        if edge.relation is Relation.DIRECT:
            direct_repos[component.purl].add(repository)
    if diff_value is None and root is not None:
        try:
            diff_value = diff_module.diff_inventory(
                root, inventory.path, from_rev="HEAD~1"
            )
        except ValueError:
            diff_value = None
    declared = insights.declarations(graph)
    results = cache.results
    outdated = insights.outdated(declared, results)
    vulnerable = insights.vulnerable(graph, declared, results)
    weights = insights.weights(graph, declared)
    splits = insights.splits(declared, results)
    roles = insights.role_uses(insights.load_roles(), declared)
    return _Site(
        graph,
        inventory,
        inventory.entries,
        cache,
        cast(Mapping[str, Any], report_value),
        diff_value,
        tuple(index),
        {key: frozenset(value) for key, value in direct_repos.items()},
        time.time(),
        outdated,
        vulnerable,
        weights,
        splits,
        roles,
        insights.scorecards(graph.repositories, outdated, vulnerable, splits, roles, weights),
    )


def _page(
    site: _Site,
    title: str,
    page_id: str,
    prefix: str,
    body: str,
) -> str:
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
    overlaps = sum(1 for use in site.roles if use.overlapping or use.off_preferred)
    breaking = sum(1 for row in site.outdated if row.gap is insights.Gap.BREAKING)
    direct_vulnerable = sum(1 for row in site.vulnerable if row.direct_in)
    body = [
        "<h1>Dependencies</h1>",
        f'<p class="sub">{len(site.graph.repositories)} repositories · '
        f"{_enrichment_stamp(site)} · {_changes_line(site)}</p>",
        '<div class="tiles">',
        _tile(
            "outdated.html#vulnerable",
            len(site.vulnerable),
            "vulnerable versions",
            f"{direct_vulnerable} declared directly",
            "finding" if site.vulnerable else "",
        ),
        _tile(
            "outdated.html",
            len(site.outdated),
            "direct dependencies behind",
            f"{breaking} need a breaking upgrade",
            "risk" if breaking else "",
        ),
        _tile(
            "bloat.html",
            sum(1 for row in site.weights if row.sole >= _HEAVY_SOLE),
            "heavy direct dependencies",
            f"each alone brings in {_HEAVY_SOLE}+ lockfile packages",
        ),
        _tile(
            "alignment.html",
            len(site.splits),
            "packages on split versions",
            f"{len({repo for row in site.splits for repo in row.off_target})} repositories off target",
        ),
        _tile(
            "choices.html",
            overlaps,
            "roles with competing choices",
            f"of {len(site.roles)} curated roles",
        ),
        "</div>",
        _section("Vulnerable", "outdated.html#vulnerable", len(site.vulnerable)),
        _vulnerable_table(site, site.vulnerable[:_OVERVIEW_ROWS]),
        _section("Furthest behind", "outdated.html", len(site.outdated)),
        _outdated_table(site.outdated[:_OVERVIEW_ROWS]),
        _section("Heaviest direct dependencies", "bloat.html", len(site.weights)),
        _weight_table(site.weights[:_OVERVIEW_ROWS], site.weights),
        _section("Version splits", "alignment.html", len(site.splits)),
        _split_table(site.splits[:_OVERVIEW_ROWS]),
        _section("Competing choices", "choices.html", overlaps),
        _choices_summary(site),
        "<h2>Repositories</h2>",
        '<p class="sub">Worst first: vulnerable versions, breaking upgrades, versions behind, '
        "split versions, then role conflicts.</p>",
        _scorecard_table(site),
    ]
    return "\n".join(body)


def _scorecard_table(site: _Site) -> str:
    body = [
        '<div class="scroll"><table><thead><tr><th>Repository</th>'
        '<th class="num">Vulnerable</th><th class="num">Behind</th>'
        '<th class="num">Breaking</th><th class="num">Off target</th>'
        "<th>Role conflicts</th><th>Heaviest dependency</th></tr></thead><tbody>"
    ]
    for card in site.scorecards:
        quiet = card.rank == (0, 0, 0, 0, 0)
        roles = ", ".join(
            f'<a href="choices.html#role-{_e(role)}">{_e(role)}</a>'
            for role in sorted({*card.mixed_roles, *card.off_preferred})
        )
        heaviest = (
            f"{_package_link(card.heaviest.component.purl, '', card.heaviest.component.display_name)}"
            f' <span class="sub">{card.heaviest.sole} only via it</span>'
            if card.heaviest is not None and card.heaviest.sole > 0
            else ""
        )
        body.append(
            f'<tr data-row data-href="repos/{_e(card.repository)}/index.html"'
            + (' class="quiet"' if quiet else "")
            + f"><td>{_repo_link(card.repository, '')}</td>"
            f'<td class="num{" finding" if card.vulnerable else ""}">{_count(card.vulnerable)}</td>'
            f'<td class="num">{_count(card.behind)}</td>'
            f'<td class="num{" risk" if card.breaking else ""}">{_count(card.breaking)}</td>'
            f'<td class="num">{_count(card.off_target)}</td>'
            f"<td>{roles}</td><td>{heaviest}</td></tr>"
        )
    body.append("</tbody></table></div>")
    return "".join(body)


def _count(value: int) -> str:
    return str(value) if value else '<span class="sub">·</span>'


def _attention(site: _Site, name: str) -> str:
    prefix = "../../../"
    items: list[str] = []
    for row in site.vulnerable:
        if name in row.direct_in or name in row.transitive_in:
            how = "declared" if name in row.direct_in else "locked"
            items.append(
                f'<li><span class="finding">vulnerable</span> '
                f"{_package_link(row.component.purl, prefix, f'{row.component.display_name}@{row.version}')} "
                f'<span class="sub">{how} · {_e(", ".join(row.advisories))}</span></li>'
            )
    for row in site.outdated:
        for use in row.uses:
            if name not in use.repositories:
                continue
            severity = insights.gap(row.component.ecosystem, use.version, row.latest)
            badge = _badge(severity) if severity is not None else ""
            items.append(
                f"<li>{badge} {_package_link(row.component.purl, prefix, row.component.display_name)} "
                f'<span class="mono">{_e(use.version)} → {_e(row.latest)}</span></li>'
            )
    behind = {
        row.component.purl
        for row in site.outdated
        if any(name in use.repositories for use in row.uses)
    }
    for row in site.splits:
        if name in row.off_target and row.component.purl not in behind:
            version = next(use.version for use in row.uses if name in use.repositories)
            items.append(
                f'<li>off target {_package_link(row.component.purl, prefix, row.component.display_name)} '
                f'<span class="mono">{_e(version)} → {_e(row.target)}</span></li>'
            )
    for use in site.roles:
        mine = [option for option, owners in use.options.items() if name in owners]
        if name in use.overlapping or name in use.off_preferred:
            preferred = f" · preferred {use.role.preferred}" if use.role.preferred is not None else ""
            items.append(
                f'<li>role <a href="{prefix}choices.html#role-{_e(use.role.name)}">'
                f"{_e(use.role.name)}</a> uses "
                f'<span class="mono">{_e(", ".join(mine))}</span>'
                f'<span class="sub">{_e(preferred)}</span></li>'
            )
    if not items:
        return '<h2>Needs attention</h2><p class="zero">nothing flagged</p>'
    return (
        f'<h2>Needs attention <span class="sub">{len(items)}</span></h2>'
        f'<ul class="attention">{"".join(items)}</ul>'
    )


def _tile(href: str, value: int, label: str, detail: str, kind: str = "") -> str:
    return (
        f'<a class="tile" href="{href}"><span class="value {kind}">{value}</span>'
        f'<span class="label">{_e(label)}</span><span class="detail">{_e(detail)}</span></a>'
    )


def _section(title: str, href: str, total: int) -> str:
    return f'<h2>{_e(title)} <a class="more" href="{href}">all {total} →</a></h2>'


def _enrichment_stamp(site: _Site) -> str:
    fetched = site.cache.fetched_at
    if fetched is None:
        return '<span class="unknown">no registry data; run repomod deps enrich</span>'
    stamp = time.strftime("%Y-%m-%d", time.gmtime(fetched))
    return f"registry data from {stamp}"


def _changes_line(site: _Site) -> str:
    value = site.diff_value
    if value is None or not value.baseline:
        return '<a href="changes.html">no change baseline</a>'
    return f'<a href="changes.html">{_summary_metrics(value)}</a>'


def _repo_list(repositories: Sequence[str], prefix: str = "", limit: int = 6) -> str:
    links = [_repo_link(name, prefix) for name in repositories]
    if len(links) <= limit:
        return ", ".join(links)
    return (
        ", ".join(links[:limit])
        + f"<details><summary>{len(links) - limit} more</summary>{', '.join(links[limit:])}</details>"
    )


def _badge(gap: insights.Gap) -> str:
    return f'<span class="badge {gap.value}">{gap.value}</span>'


def _vulnerable_table(site: _Site, rows: Sequence[insights.Vulnerable]) -> str:
    if not rows:
        if site.cache.fetched_at is None:
            return '<p class="unknown">not checked</p>'
        return '<p class="zero">no known vulnerabilities in checked versions</p>'
    body = [
        '<div class="scroll"><table><thead><tr><th>Package</th><th>Advisories</th>'
        "<th>Latest</th><th>Declared in</th><th>Locked in</th></tr></thead><tbody>"
    ]
    for row in rows:
        fixed = row.latest is not None and row.latest != row.version
        latest = (
            f'<span class="mono">{_e(row.latest)}</span>'
            if fixed
            else '<span class="unknown">no newer release</span>'
        )
        body.append(
            "<tr>"
            f"<td>{_package_link(row.component.purl, '', f'{row.component.display_name}@{row.version}')}</td>"
            f'<td class="mono finding">{_e(", ".join(row.advisories))}</td>'
            f"<td>{latest}</td>"
            f"<td>{_repo_list(row.direct_in)}</td>"
            f"<td>{_repo_list(row.transitive_in)}</td></tr>"
        )
    body.append("</tbody></table></div>")
    return "".join(body)


def _outdated_table(rows: Sequence[insights.Outdated]) -> str:
    if not rows:
        return '<p class="zero">every checked direct dependency is on its latest release</p>'
    body = [
        '<div class="scroll"><table><thead><tr><th>Package</th><th>Upgrade</th>'
        '<th>Latest</th><th>In use</th><th class="num">Repos</th></tr></thead><tbody>'
    ]
    for row in rows:
        uses = "".join(
            f'<div><span class="mono">{_e(use.version)}</span> {_repo_list(use.repositories)}</div>'
            for use in row.uses
        )
        body.append(
            "<tr>"
            f"<td>{_package_link(row.component.purl, '', row.component.display_name)}"
            f' <span class="sub">{_e(row.component.ecosystem.value)}</span></td>'
            f"<td>{_badge(row.gap)}</td>"
            f'<td class="mono">{_e(row.latest)}</td>'
            f"<td>{uses}</td>"
            f'<td class="num">{row.repositories}</td></tr>'
        )
    body.append("</tbody></table></div>")
    return "".join(body)


def _weight_table(
    rows: Sequence[insights.Weight], scale: Sequence[insights.Weight]
) -> str:
    if not rows:
        return '<p class="zero">no direct dependency brings in lockfile packages</p>'
    widest = max(row.pulls for row in scale)
    body = [
        '<div class="scroll"><table><thead><tr><th>Repository</th><th>Dependency</th>'
        '<th>Scope</th><th class="num">Only via it</th><th class="num">Pulls in</th>'
        "<th></th></tr></thead><tbody>"
    ]
    body.extend(
        "<tr>"
        f"<td>{_repo_link(row.repository, '')}</td>"
        f"<td>{_package_link(row.component.purl, '', f'{row.component.display_name}@{row.version}')}</td>"
        f"<td>{_e(', '.join(sorted(scope.value for scope in row.scopes)))}</td>"
        f'<td class="num">{row.sole}</td><td class="num">{row.pulls}</td>'
        f'<td class="weight">{_bar(row.sole, widest)}{_bar(row.pulls - row.sole, widest, "shared")}</td></tr>'
        for row in rows
    )
    body.append("</tbody></table></div>")
    return "".join(body)


def _split_table(rows: Sequence[insights.Split]) -> str:
    if not rows:
        return '<p class="zero">every package declared in two or more repositories uses one version</p>'
    body = [
        '<div class="scroll"><table><thead><tr><th>Package</th><th>Target</th>'
        '<th>Versions in use</th><th class="num">Off target</th></tr></thead><tbody>'
    ]
    for row in rows:
        uses = "".join(
            f'<div class="{"on-target" if use.version == row.target else ""}">'
            f'<span class="mono">{_e(use.version)}</span> {_repo_list(use.repositories)}</div>'
            for use in row.uses
        )
        target = (
            f'<span class="mono">{_e(row.target)}</span> '
            f'<span class="sub">{_e(row.basis.value)}</span>'
        )
        body.append(
            "<tr>"
            f"<td>{_package_link(row.component.purl, '', row.component.display_name)}"
            f' <span class="sub">{_e(row.component.ecosystem.value)}</span></td>'
            f"<td>{target}</td><td>{uses}</td>"
            f'<td class="num">{len(set(row.off_target))}</td></tr>'
        )
    body.append("</tbody></table></div>")
    return "".join(body)


def _choices_summary(site: _Site) -> str:
    contested = [use for use in site.roles if use.overlapping or use.off_preferred]
    if not contested:
        return '<p class="zero">every curated role has one choice in use</p>'
    body = [
        '<div class="scroll"><table><thead><tr><th>Role</th><th>Options in use</th>'
        '<th>Preferred</th><th class="num">Mixed repos</th><th class="num">Off preferred</th>'
        "</tr></thead><tbody>"
    ]
    for use in contested[:_OVERVIEW_ROWS]:
        options = ", ".join(
            f"{_e(option)} <span class=\"sub\">{len(use.options[option])}</span>" for option in use.in_use
        )
        body.append(
            "<tr>"
            f'<td><a href="choices.html#role-{_e(use.role.name)}">{_e(use.role.name)}</a></td>'
            f"<td>{options}</td>"
            f"<td>{_e(use.role.preferred or '—')}</td>"
            f'<td class="num">{len(use.overlapping)}</td>'
            f'<td class="num">{len(use.off_preferred)}</td></tr>'
        )
    body.append("</tbody></table></div>")
    return "".join(body)


def _outdated(site: _Site) -> str:
    body = [
        "<h1>Outdated</h1>",
        f'<p class="sub">Direct dependencies compared with their registry\'s latest release '
        f"({_enrichment_stamp(site)}). A breaking upgrade changes the leftmost nonzero "
        "version component.</p>",
        '<h2 id="vulnerable">Vulnerable versions</h2>',
        _vulnerable_table(site, site.vulnerable),
    ]
    for gap in insights.Gap:
        rows = [row for row in site.outdated if row.gap is gap]
        body.append(f'<h2 id="{gap.value}">{gap.value.capitalize()} upgrades <span class="sub">{len(rows)}</span></h2>')
        body.append(_outdated_table(rows))
    return "\n".join(body)


def _bloat(site: _Site) -> str:
    body = [
        "<h1>Bloat</h1>",
        '<p class="sub">Each direct dependency with the lockfile packages it brings in. '
        "<em>Only via it</em> counts packages no other direct dependency in that repository "
        "needs, so removing or replacing it drops them. Lockfiles list every platform's "
        "packages, so these are upper bounds on what one target compiles.</p>",
        _weight_table(site.weights[:_BLOAT_ROWS], site.weights),
        f'<p class="sub">{max(0, len(site.weights) - _BLOAT_ROWS)} lighter dependencies not shown.</p>'
        if len(site.weights) > _BLOAT_ROWS
        else "",
        "<h2>Duplicate versions in one repository</h2>",
        '<p class="sub">Two versions of one package in a lockfile means both are built.</p>',
    ]
    duplicates = [
        (name, _duplicates(site, data))
        for name, data in site.graph.repositories.items()
    ]
    duplicates = sorted(
        ((name, found) for name, found in duplicates if found),
        key=lambda item: (-len(item[1]), item[0].casefold()),
    )
    if not duplicates:
        body.append('<p class="zero">no repository carries two versions of one package</p>')
        return "\n".join(body)
    body.append(
        '<div class="scroll"><table><thead><tr><th>Repository</th><th class="num">Packages</th>'
        "<th>Duplicated</th></tr></thead><tbody>"
    )
    for name, found in duplicates:
        items = "".join(
            f"<li>{_package_link(purl, '', _display_name(site, purl))} "
            f'<span class="versions">{_e(", ".join(versions))}</span></li>'
            for purl, versions in found
        )
        body.append(
            f"<tr><td>{_repo_link(name, '')}</td><td class=\"num\">{len(found)}</td>"
            f"<td><details><summary>show</summary><ul>{items}</ul></details></td></tr>"
        )
    body.append("</tbody></table></div>")
    return "\n".join(body)


def _display_name(site: _Site, purl: str) -> str:
    component = site.graph.components.get(purl)
    return component.display_name if component is not None else purl


def _alignment(site: _Site) -> str:
    return "\n".join(
        [
            "<h1>Alignment</h1>",
            '<p class="sub">Packages declared directly by two or more repositories at different '
            "versions. The target is the registry's latest release when known, otherwise the "
            "newest version in use, or the most common ref when versions are tags or commits. "
            "Alpine packages follow their base image and are left out.</p>",
            _split_table(site.splits),
        ]
    )


def _choices(site: _Site) -> str:
    body = [
        "<h1>Choices</h1>",
        '<p class="sub">Curated roles from <span class="mono">dependency-roles.toml</span>: '
        "packages that do the same job. A mixed repository declares more than one option for "
        "a role; off preferred means it uses an option other than the preferred one.</p>",
    ]
    settled = []
    for use in site.roles:
        if not use.in_use:
            continue
        if len(use.in_use) == 1 and not use.off_preferred:
            settled.append(use)
            continue
        body.append(
            f'<h2 id="role-{_e(use.role.name)}">{_e(use.role.name)} '
            f'<span class="sub">{_e(use.role.ecosystem.value)} · preferred '
            f"{_e(use.role.preferred or 'not chosen')}</span></h2>"
        )
        body.append(
            '<div class="scroll"><table><thead><tr><th>Option</th><th class="num">Repos</th>'
            "<th>Repositories</th></tr></thead><tbody>"
        )
        for option in use.in_use:
            repositories = use.options[option]
            preferred = option == use.role.preferred
            body.append(
                f'<tr class="{"on-target" if preferred else ""}"><td class="mono">{_e(option)}</td>'
                f'<td class="num">{len(repositories)}</td>'
                f"<td>{_repo_list(repositories, limit=12)}</td></tr>"
            )
        body.append("</tbody></table></div>")
        if use.overlapping:
            body.append(
                f'<p class="sub risk">mixed: {_repo_list(use.overlapping, limit=12)}</p>'
            )
    if settled:
        body.append("<h2>Settled</h2>")
        body.append(
            "<ul>"
            + "".join(
                f"<li>{_e(use.role.name)}: <span class=\"mono\">{_e(use.in_use[0])}</span> "
                f'<span class="sub">{len(use.options[use.in_use[0]])} repositories</span></li>'
                for use in settled
            )
            + "</ul>"
        )
    return "\n".join(body)


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
            (repository, edge)
            for repository in sorted(site.graph.instance_repositories.get(instance.id, ()))
            for edge in site.graph.incoming.get((repository, instance.id), ())
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
    body.append("<h2>Ecosystems</h2>")
    body.append(_ecosystem_table(_table(site.report["composition"])))
    body.append("<h2>Source risk</h2>")
    body.append(_risk_table(site))
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
        _attention(site, name),
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
