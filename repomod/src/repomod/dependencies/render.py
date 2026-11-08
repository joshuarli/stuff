from __future__ import annotations

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

from repomod import dependency_roles
from repomod.dependencies import analysis, enrich, insights, triage, ui
from repomod.dependencies import diff as diff_module
from repomod.dependencies.graph import Graph, Inventory
from repomod.dependencies.model import (
    CoverageKind,
    Ecosystem,
    Edge,
    IndexEntry,
    Instance,
    Project,
    Relation,
    RepositoryData,
    Scope,
)
from repomod.dependencies.triage import ActionKind
from repomod.dependencies.ui import Bar, Column, Line, Mark, NavGroup, NavLink, Row, Segment, Stop, Tile, Tone
from repomod.dependencies.ui import escape as _e
from repomod.rules import actions as actions_rule
from repomod.rules.python_standard import PYTHON_TOOL_NAMES

MARKER = ".repomod-dashboard"
ASSETS = Path(__file__).with_name("assets")
_LIST_ROWS = 25
_PREVIEW_ROWS = 8
_SHOWN_REPOSITORIES = 2
_CLEAN_TIER = "Clean"
_TIERS = (
    ("Vulnerable", "Security issues first"),
    ("Breaking upgrades", "No known vulnerabilities, but breaking upgrades are waiting"),
    ("Maintenance", "Routine upgrades, version drift, or role conflicts"),
)
_BLOAT_ROWS = 150
_CONSUMER_ROWS = 20
_TOP_PACKAGES = 5
_KIND_LABELS = {
    ActionKind.SECURITY: ("Security", Tone.BAD),
    ActionKind.UNREADABLE: ("Unreadable", Tone.BAD),
    ActionKind.LOCKFILE: ("Lockfile", Tone.WARN),
    ActionKind.BREAKING: ("Breaking", Tone.WARN),
    ActionKind.TOOLING: ("Tooling", Tone.NEUTRAL),
    ActionKind.ROUTINE: ("Routine", Tone.NEUTRAL),
}


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
    forks: tuple[str, ...]
    actions: tuple[triage.Action, ...]


@dataclass(frozen=True, slots=True)
class _PackageUse:
    purl: str
    label: str
    ecosystem: Ecosystem
    repositories: int
    direct: int


def render(
    inventory: Inventory,
    graph: Graph,
    output: Path,
    *,
    root: Path | None = None,
    diff_value: diff_module.Diff | None = None,
) -> Dashboard:
    site = _site(inventory, graph, root, diff_value)
    if output.is_symlink():
        raise ValueError(f"refusing to write a dashboard through a symlink: {output}")
    if output.exists():
        if not output.is_dir() or not (output / MARKER).is_file():
            raise ValueError(f"refusing to replace a directory that is not a dashboard: {output}")
        shutil.rmtree(output)
    output.mkdir(parents=True)
    (output / MARKER).write_text("repomod deps render\n", encoding="utf-8")
    pages = [
        _write_page(output / f"{page}.html", _page(site, title, page, "", body(site)))
        for page, title, body in (
            ("index", "To do", _overview),
            ("repositories", "Repositories", _repositories),
            ("security", "Security", _security),
            ("outdated", "Upgrades", _outdated),
            ("alignment", "Alignment", _alignment),
            ("choices", "Choices", _choices),
            ("bloat", "Bloat", _bloat),
            ("packages", "Packages", _packages),
            ("changes", "Changes", _changes),
            ("coverage", "Coverage", _coverage),
        )
    ]
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
    mine = insights.owned(graph, declared)
    forks = insights.forks(graph)
    results = cache.results
    outdated = insights.outdated(mine, results)
    vulnerable = insights.vulnerable(graph, declared, results)
    weights = insights.weights(graph, mine)
    splits = insights.splits(
        mine, results, pins={name: pin.sha for name, pin in actions_rule.load_pins().items()}
    )
    roles = insights.role_uses(dependency_roles.load_roles(), mine)
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
        insights.scorecards(
            [name for name in graph.repositories if name not in forks],
            outdated,
            vulnerable,
            splits,
            roles,
            weights,
        ),
        forks,
        _triage(graph, vulnerable, outdated, forks),
    )


def _triage(
    graph: Graph,
    vulnerable: Sequence[insights.Vulnerable],
    outdated: Sequence[insights.Outdated],
    forks: Sequence[str],
) -> tuple[triage.Action, ...]:
    unreadable = [
        name
        for name, data in graph.repositories.items()
        if data.state.value in ("failed", "unavailable")
    ]
    missing_locks = sorted(
        (
            name
            for name, data in graph.repositories.items()
            if name not in forks
            and any(issue.kind is CoverageKind.MISSING_LOCK for issue in data.issues)
        ),
        key=str.casefold,
    )
    return triage.actions(vulnerable, outdated, forks, unreadable, missing_locks, PYTHON_TOOL_NAMES)


def _page(site: _Site, title: str, page_id: str, prefix: str, body: str) -> str:
    active = "repositories" if page_id == "repository" else page_id
    return ui.shell(title, page_id, active, prefix, _navigation(site), body)


def _navigation(site: _Site) -> tuple[NavGroup, ...]:
    return (
        NavGroup(
            "Main",
            (
                NavLink("index", "To do", len(site.actions)),
                NavLink("repositories", "Repositories"),
                NavLink("packages", "Packages"),
            ),
            heading=False,
        ),
        NavGroup(
            "Review",
            (
                NavLink("security", "Security", len(_of_kind(site, ActionKind.SECURITY))),
                NavLink("outdated", "Upgrades", len(site.outdated)),
                NavLink("alignment", "Alignment", len(site.splits)),
                NavLink("choices", "Choices", len(_contested_roles(site))),
                NavLink("bloat", "Bloat"),
            ),
        ),
        NavGroup("Inventory", (NavLink("coverage", "Coverage"), NavLink("changes", "Changes"))),
    )


def _write_page(path: Path, content: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


def _write_assets(output: Path, site: _Site) -> None:
    (output / "assets" / "ui-dashboard").mkdir(parents=True)
    for name in ("app.css", "app.js", "ui-dashboard/styles.css", "ui-dashboard/tokens.css"):
        shutil.copyfile(ASSETS / name, output / "assets" / name)
    template = (ASSETS / "search.js").read_text(encoding="utf-8")
    (output / "assets" / "search.js").write_text(
        template.replace("__DEPS_INDEX__", _json(list(site.index))), encoding="utf-8"
    )


def _of_kind(site: _Site, kind: ActionKind) -> list[triage.Action]:
    return [action for action in site.actions if action.kind is kind]


def _contested_roles(site: _Site) -> list[insights.RoleUse]:
    return [use for use in site.roles if use.overlapping or use.off_preferred]


def _checked(site: _Site) -> bool:
    return site.cache.fetched_at is not None


def _owned_count(site: _Site) -> int:
    return len(site.graph.repositories) - len(site.forks)


def _overview_tiles(site: _Site) -> tuple[Tile, ...]:
    checked = _checked(site)
    security = _of_kind(site, ActionKind.SECURITY)
    blocked = sum(1 for action in security if action.blocked)
    affected = len({name for action in security for name in action.repositories})
    breaking = sum(1 for row in site.outdated if row.gap is insights.Gap.BREAKING)
    routine = len(site.outdated) - breaking
    locks = _of_kind(site, ActionKind.LOCKFILE)
    missing = len(locks[0].repositories) if locks else 0
    contested = _contested_roles(site)
    off_preferred = sum(len(use.off_preferred) for use in contested)
    return (
        Tile(
            str(len(security)) if checked else "—",
            "Security",
            f"{affected} repositories · {blocked} with no clean upgrade" if checked else "not checked",
            "security.html",
            Tone.BAD if security else None,
        ),
        Tile(
            str(breaking) if checked else "—",
            "Breaking upgrades",
            f"plus {routine} minor and patch" if checked else "not checked",
            "outdated.html",
            Tone.WARN if breaking else None,
        ),
        Tile(
            str(missing),
            "No lockfile",
            "repositories with unknown versions",
            "coverage.html",
            Tone.WARN if missing else None,
        ),
        Tile(str(len(site.splits)), "Version drift", "packages on 2 or more versions", "alignment.html"),
        Tile(str(len(contested)), "Role conflicts", f"{off_preferred} repositories off preferred", "choices.html"),
    )


def _fork_exposure(site: _Site) -> dict[str, int]:
    exposure = dict.fromkeys(site.forks, 0)
    for row in site.vulnerable:
        for repository in {*row.direct_in, *row.transitive_in}:
            if repository in exposure:
                exposure[repository] += 1
    return exposure


def _repo_list(repositories: Sequence[str], prefix: str = "", limit: int = 6) -> str:
    links = [_repo_link(name, prefix) for name in repositories]
    if len(links) <= limit:
        return ", ".join(links)
    return (
        ", ".join(links[:limit])
        + f"<details><summary>{len(links) - limit} more</summary>{', '.join(links[limit:])}</details>"
    )


def _plural(count: int, noun: str, plural: str | None = None) -> str:
    return f"{count} {noun}" if count == 1 else f"{count} {plural or noun + 's'}"


def _short_ref(version: str) -> str:
    if "@sha256:" in version:
        tag, digest = version.split("@sha256:", 1)
        return f"{tag}@sha256:{digest[:12]}…"
    return version


def _display_name(site: _Site, purl: str) -> str:
    component = site.graph.components.get(purl)
    return component.display_name if component is not None else purl


def _package_uses(site: _Site) -> list[_PackageUse]:
    instances: dict[str, list[Instance]] = defaultdict(list)
    for instance in site.graph.instances.values():
        instances[instance.component].append(instance)
    uses = []
    for purl, component in site.graph.components.items():
        versions = instances.get(purl, [])
        repositories = {
            repository
            for instance in versions
            for repository in site.graph.instance_repositories.get(instance.id, ())
        }
        label = component.display_name
        if component.ecosystem is Ecosystem.DOCKER:
            tags = sorted({instance.version or "latest" for instance in versions})
            label = f"{label}@{','.join(tags)}"
        uses.append(
            _PackageUse(purl, label, component.ecosystem, len(repositories), len(site.direct_repos.get(purl, ())))
        )
    return uses


def _package_section(site: _Site, purl: str) -> str:
    component = site.graph.components[purl]
    instances = sorted(
        (item for item in site.graph.instances.values() if item.component == purl),
        key=lambda item: (item.version or "", item.id),
    )
    consumers = [
        (repository, edge)
        for instance in instances
        for repository in sorted(site.graph.instance_repositories.get(instance.id, ()))
        for edge in site.graph.incoming.get((repository, instance.id), ())
    ]
    first_party = " · first-party" if any(instance.first_party for instance in instances) else ""
    identity = (
        f'<div class="package-identity"><span class="mono">{_e(purl)}</span>'
        f"{ui.copy_button(purl)}<span>{_e(component.ecosystem.value)}{first_party}</span></div>"
    )
    versions = ui.static_table(
        (Column("Version", mono=True), Column("Source", wrap=True), Column("Registry", wrap=True)),
        [
            (_e(instance.version or "unknown"), _source_label(instance), _enrichment_cell(site, instance))
            for instance in instances
        ],
    )
    used_by = (
        ui.static_table(
            (Column("Repository"), Column("From", wrap=True, mono=True), Column("Relation"), Column("Scope")),
            [
                (
                    _repo_link(repository, ""),
                    _e(_edge_label(site.graph, edge)),
                    _e(edge.relation.value),
                    _e(edge.scope.value),
                )
                for repository, edge in consumers[:_CONSUMER_ROWS]
            ],
        )
        if consumers
        else '<p class="zero">no consumers</p>'
    )
    meta = f"showing {_CONSUMER_ROWS} of {len(consumers)}" if len(consumers) > _CONSUMER_ROWS else ""
    heading = f"Used by · {meta}" if meta else "Used by"
    return (
        f'<section class="package" data-purl="{_e(purl)}">'
        f"{ui.page_header(component.display_name)}{identity}"
        f"{ui.panel('Versions', f'{versions}<div class="package-heading">{heading}</div>{used_by}', flush=True)}"
        "</section>"
    )


def _changes(site: _Site) -> str:
    value = site.diff_value
    if value is None or not value.baseline:
        return ui.page_header("Changes") + (
            '<p class="unknown">no baseline: the previous revision has no dependency inventory</p>'
        )
    body = [ui.page_header("Changes", _summary_metrics(value))]
    if not value.repositories:
        body.append(ui.panel("Repositories", ui.empty_state("No changes since the previous inventory"), flush=True))
    for change in value.repositories:
        groups = [
            _change_group("Added", [f'<span class="mono">+ {_e(item.purl)}</span>' for item in change.added]),
            _change_group("Removed", [f'<span class="mono">- {_e(item.purl)}</span>' for item in change.removed]),
            _change_group(
                "Versions",
                [
                    f'<span class="mono">{_e(item.name)}</span>: {_e(item.old or "—")} → {_e(item.new or "—")} '
                    f"{ui.badge(item.direction, Tone.NEUTRAL if item.direction == 'upgrade' else Tone.WARN)}"
                    for item in change.versions
                ],
            ),
            _change_group(
                "Sources",
                [
                    f'<span class="mono">{_e(item.component)}</span>: {_e(item.old.value)} → {_e(item.new.value)}'
                    for item in change.sources
                ],
            ),
            _change_group(
                "Issues",
                [
                    *(
                        f'<span class="finding">+ {_e(item.kind.value)} {_e(item.path or "")}</span>'
                        for item in change.new_issues
                    ),
                    *(f"- {_e(item.kind.value)} {_e(item.path or '')}" for item in change.resolved_issues),
                ],
            ),
        ]
        body.append(
            ui.panel(
                change.repository,
                "".join(groups) or '<p class="panel-text">no dependency changes</p>',
                meta=f'{_e(change.status)} · <span class="mono">{_e((change.old_commit or "?")[:12])} → '
                f'{_e((change.new_commit or "?")[:12])}</span>',
            )
        )
    return "\n".join(body)


def _change_group(title: str, items: Sequence[str]) -> str:
    if not items:
        return ""
    rendered = "".join(f"<li>{item}</li>" for item in items)
    return f'<div class="change-group">{title}</div><ul class="plain-list">{rendered}</ul>'


_COVERAGE_HELP = {
    CoverageKind.CHECKOUT_MISSING: (
        "No checkout",
        "There is no local checkout at the catalogued path, so nothing could be read.",
        "Run `repomod sync` for that repository, or fix its path in the catalog.",
    ),
    CoverageKind.ORIGIN_MISMATCH: (
        "Wrong remote",
        "The checkout's git remote does not point at the catalogued GitHub repository.",
        "Fix the remote (or the catalog entry), re-sync, then re-snapshot.",
    ),
    CoverageKind.DEFAULT_REF_MISSING: (
        "Default branch missing",
        "The default branch cannot be read, so the inventory has no commit to record.",
        "Fetch the default branch and set `origin/HEAD`, then re-snapshot.",
    ),
    CoverageKind.GIT_FAILURE: (
        "Git read failed",
        "Git could not read the checkout, so its dependencies are not recorded.",
        "Run the git command in that checkout to see the raw error, then re-snapshot.",
    ),
    CoverageKind.PARSE_ERROR: (
        "File does not parse",
        "A dependency file exists but the parser rejected it (bad syntax or structure).",
        "Open the linked file and fix the syntax, then re-snapshot. Details are on the repository page.",
    ),
    CoverageKind.LOCK_MISMATCH: (
        "Manifest and lock disagree",
        "The lockfile does not match the manifest (stale or hand-edited).",
        "Regenerate the lockfile and commit it, then re-snapshot.",
    ),
    CoverageKind.MISSING_LOCK: (
        "No lockfile",
        "The project declares dependencies but commits no lockfile, so exact versions are unknown.",
        "Generate the lockfile for the ecosystem in use and commit it.",
    ),
    CoverageKind.UNSUPPORTED_FORMAT: (
        "Detected, not parsed",
        "A dependency file was found but no parser covers its format yet (see the list below).",
        "Nothing to fix in the application. Covering it means adding an adapter, not guessing.",
    ),
    CoverageKind.DYNAMIC_VALUE: (
        "Version is computed",
        "A version comes from a variable, function, or template instead of a literal version.",
        "Pin a literal version where you can. Computed values stay `unknown` on purpose, never guessed.",
    ),
    CoverageKind.SYMLINK_SKIPPED: (
        "Symlinked file skipped",
        "A dependency file is a symlink, so its contents were not read.",
        "Replace the symlink with the real file, or commit the target, then re-snapshot.",
    ),
    CoverageKind.SUBMODULE: (
        "Submodule not inspected",
        "A gitlink (submodule) is present; its contents were not read.",
        "Init the submodule if you want it counted, otherwise ignore it. Recorded as a gap, not a failure.",
    ),
    CoverageKind.VENDORED_UNKNOWN: (
        "Vendored tree, no manifest",
        "A vendored tree has no recognised manifest, so it was not parsed.",
        "Add a manifest to the vendored tree, or leave it: vendored code is reported, not chased.",
    ),
    CoverageKind.NON_UTF8: (
        "Not UTF-8 text",
        "A dependency file is not UTF-8 text, so it could not be decoded.",
        "Convert the file to UTF-8 if it is meant to be read as text, then re-snapshot.",
    ),
}


def _state_badge(data: RepositoryData) -> str:
    value = data.state.value
    tone = {"complete": Tone.OK, "partial": Tone.NEUTRAL}.get(value, Tone.BAD)
    return ui.badge(value, tone, dot=True)


def _repository(site: _Site, name: str) -> str:
    data = site.graph.repositories[name]
    entry = site.entries.get(name)
    commit = entry.commit if entry is not None else None
    card = next((item for item in site.scorecards if item.repository == name), None)
    short = (commit or "unknown")[:12]
    fork = " · fork, tracked only" if name in site.forks else ""
    body = [
        ui.page_header(
            name,
            f'<a href="../../../repositories.html">Repositories</a> · <a href="../../../index.html">To do</a> · '
            f'commit <span class="mono">{_e(short)}</span>{fork}',
        ),
        f'<div class="badges">{_state_badge(data)}{"".join(ui.badge(tag) for tag in data.tags)}</div>',
    ]
    if card is not None:
        body.append(
            ui.status_strip(
                (
                    Tile(
                        str(card.vulnerable),
                        "Vulnerable",
                        "packages",
                        "#todo",
                        Tone.BAD if card.vulnerable else None,
                    ),
                    Tile(str(card.breaking), "Breaking", "upgrades", "#todo", Tone.WARN if card.breaking else None),
                    Tile(str(card.behind), "Behind", "direct dependencies", "#todo"),
                    Tile(str(card.off_target), "Off target", "version drift", "#todo"),
                )
            )
        )
    body.append(_todo_panel(site, name))
    if not data.edges:
        body.append(
            '<p class="zero">complete: zero dependencies</p>'
            if data.state.value == "complete"
            else '<p class="unknown">no dependency edges recorded</p>'
        )
    projects = {project.id: project for project in data.projects}
    body.append(_direct_panel(site, name, commit, data, projects))
    body.append(_projects_panel(data, projects))
    duplicates = _duplicates(site, data)
    if duplicates:
        body.append(
            ui.collapsed_panel(
                "Duplicate versions",
                ui.table(
                    "Duplicate versions",
                    (Column("Package"), Column("Versions")),
                    [
                        Row(
                            (
                                _package_link(purl, "../../../", purl),
                                f'<span class="mono">{_e(", ".join(versions))}</span>',
                            )
                        )
                        for purl, versions in duplicates
                    ],
                ),
                meta=str(len(duplicates)),
                flush=True,
            )
        )
    body.append(_gaps_panel(name, commit, data))
    return "\n".join(body)


def _direct_panel(
    site: _Site, name: str, commit: str | None, data: RepositoryData, projects: Mapping[str, Project]
) -> str:
    edges = sorted(
        (edge for edge in data.edges if edge.relation is Relation.DIRECT),
        key=lambda item: (item.from_id, item.to_id, item.scope.value),
    )
    scopes = Counter(edge.scope.value for edge in data.edges)
    meta = " · ".join(f"{count} {scope}" for scope, count in sorted(scopes.items()))
    rows = [_direct_row(site, name, commit, data, projects, edge) for edge in edges]
    return ui.collapsed_panel(
        "Direct dependencies",
        ui.table(
            "Direct dependencies",
            (
                Column("Dependency"),
                Column("Project"),
                Column("Scope"),
                Column("Constraint"),
                Column("Evidence", wrap=True),
            ),
            rows,
            search="Filter dependencies",
            limit=_LIST_ROWS,
        )
        if rows
        else '<p class="zero">no direct dependencies</p>',
        meta=_e(meta),
        flush=True,
    )


def _direct_row(
    site: _Site,
    name: str,
    commit: str | None,
    data: RepositoryData,
    projects: Mapping[str, Project],
    edge: Edge,
) -> Row:
    target = site.graph.instances.get(edge.to_id)
    transitive = sorted(
        (item for item in data.edges if item.from_id == edge.to_id and item.relation is Relation.TRANSITIVE),
        key=lambda item: item.to_id,
    )
    details = ""
    if transitive:
        listed = "".join(
            f'<li class="mono">{_e(_edge_label(site.graph, item))}</li>' for item in transitive[:8]
        )
        details = f'<details><summary>transitive</summary><ul class="plain-list">{listed}</ul></details>'
    link = (
        _package_link(target.component, "../../../", site.graph.label(edge.to_id))
        if target is not None
        else f'<span class="mono">{_e(edge.to_id)}</span>'
    )
    return Row(
        (
            f"{link}{details}",
            f'<span class="mono">{_e(_project_label(projects, edge.from_id))}</span>',
            _e(edge.scope.value),
            f'<span class="mono">{_e(edge.constraint or "")}</span>',
            _evidence(site, name, commit, edge),
        )
    )


def _projects_panel(data: RepositoryData, projects: Mapping[str, Project]) -> str:
    rows = [
        Row(
            (
                f'<span class="mono">{_e(project.id)}</span>',
                _e(project.ecosystem.value),
                f'<span class="mono">{_e(project.directory)}</span>',
                str(sum(1 for edge in data.edges if edge.from_id == project.id and edge.relation is Relation.DIRECT)),
            )
        )
        for project in sorted(projects.values(), key=lambda item: item.id)
    ]
    return ui.collapsed_panel(
        "Projects",
        ui.table(
            "Projects",
            (Column("Project"), Column("Ecosystem"), Column("Directory"), Column("Direct edges", numeric=True)),
            rows,
        )
        if rows
        else '<p class="zero">no package projects</p>',
        meta=str(len(rows)),
        flush=True,
    )


def _gaps_panel(name: str, commit: str | None, data: RepositoryData) -> str:
    rows = [
        Row(
            (
                ui.badge(issue.kind.value, Tone.BAD if issue.kind.severity == "error" else Tone.NEUTRAL),
                _evidence_path(name, commit, issue.path),
                _e(issue.message),
            )
        )
        for issue in sorted(data.issues, key=lambda item: (item.path or "", item.kind.value, item.message))
    ]
    return ui.collapsed_panel(
        "Coverage gaps",
        ui.table(
            "Coverage gaps",
            (Column("Kind"), Column("Path", wrap=True), Column("Message", wrap=True)),
            rows,
            limit=_LIST_ROWS,
        )
        if rows
        else '<p class="zero">no coverage issues</p>',
        meta=str(len(rows)),
        flush=True,
    )


def _overview(site: _Site) -> str:
    security = _of_kind(site, ActionKind.SECURITY)
    body = [
        ui.page_header(
            "To do",
            f"{_owned_count(site)} repositories · {_enrichment_stamp(site)} · {_changes_line(site)}",
        ),
        ui.status_strip(_overview_tiles(site)),
    ]
    if not _checked(site):
        body.append(
            ui.notice(
                'Vulnerabilities and upgrades are not checked yet. Run '
                '<span class="mono">uv run repomod deps enrich</span>, then render again.',
                Tone.WARN,
            )
        )
    if not site.actions:
        body.append(
            ui.panel(
                "Next",
                ui.empty_state(
                    "Nothing to fix",
                    "No vulnerabilities, unreadable checkouts, missing lockfiles, or pending upgrades.",
                ),
                flush=True,
            )
        )
    else:
        body.append(_work_panel(site))
        body.extend(_security_groups(security))
        body.append(_command_group(site))
        body.append(_plan_group(site))
    exposed = sum(1 for count in _fork_exposure(site).values() if count)
    if site.forks:
        body.append(
            ui.notice(
                f"{len(site.forks)} forks keep upstream's dependency choices and are left out of "
                f'this list. <a href="security.html#forks">{exposed} carry vulnerable versions</a>.'
            )
        )
    return "\n".join(body)


def _work_parts(card: insights.Scorecard) -> tuple[Segment, ...]:
    return (
        Segment("Vulnerable", card.vulnerable, Mark.BAD),
        Segment("Breaking", card.breaking, Mark.WARN),
        Segment("Minor and patch", card.behind - card.breaking, Mark.GHOST),
    )


def _work_total(card: insights.Scorecard) -> int:
    return sum(part.value for part in _work_parts(card))


_WORK_LEGEND = (("Vulnerable", Mark.BAD), ("Breaking", Mark.WARN), ("Minor and patch", Mark.GHOST))


def _work_panel(site: _Site) -> str:
    cards = [card for card in site.scorecards if _work_total(card)][:_PREVIEW_ROWS]
    scale = max((_work_total(card) for card in cards), default=0)
    bars = [
        Bar(
            f'<a href="repos/{_e(card.repository)}/index.html">{_e(_short_repository(card.repository))}</a>',
            _work_parts(card),
        )
        for card in cards
    ]
    return ui.panel(
        "Where the work is",
        ui.legend(_WORK_LEGEND) + ui.bar_list(bars, scale, "Open work by repository"),
        meta='<a href="repositories.html">All repositories</a>',
    )


def _security_groups(actions: Sequence[triage.Action]) -> list[str]:
    ready = [action for action in actions if not action.blocked]
    blocked = [action for action in actions if action.blocked]
    groups = []
    if ready:
        groups.append(
            ui.group(
                "Fix now",
                ui.card_grid(_security_card(action) for action in ready),
                count=len(ready),
                note="Vulnerable, with a clean upgrade available",
            )
        )
    if blocked:
        groups.append(
            ui.group(
                "Decide",
                ui.card_grid(_security_card(action) for action in blocked),
                count=len(blocked),
                note="Vulnerable, with no clean upgrade. Replace the package or accept the risk.",
            )
        )
    return groups


def _security_card(action: triage.Action) -> str:
    if action.purl is None:
        raise ValueError(f"security action has no package: {action.subject}")
    return ui.card(
        _package_link(action.purl, "", action.subject),
        f'<div class="card-change">{_e(action.change)}</div>'
        f'<div class="card-foot">{_repository_names(action.repositories)} · {_e(action.step)}</div>',
        meta=ui.badge(_advisories_label(len(action.details)), Tone.BAD if not action.blocked else Tone.WARN),
    )


def _advisories_label(count: int) -> str:
    return f"{count} advisory" if count == 1 else f"{count} advisories"


def _repository_names(repositories: Sequence[str], prefix: str = "") -> str:
    shown = ", ".join(
        f'<a href="{prefix}repos/{_e(name)}/index.html">{_e(_short_repository(name))}</a>'
        for name in repositories[:_SHOWN_REPOSITORIES]
    )
    hidden = repositories[_SHOWN_REPOSITORIES:]
    if not hidden:
        return shown
    return f'{shown} <span data-tip="{_e(", ".join(hidden))}">+{len(hidden)} more</span>'


def _short_repository(name: str) -> str:
    return name.rsplit("/", 1)[-1]


def _command_group(site: _Site) -> str:
    actions = [
        action
        for action in site.actions
        if action.kind in (ActionKind.UNREADABLE, ActionKind.LOCKFILE, ActionKind.TOOLING, ActionKind.ROUTINE)
    ]
    if not actions:
        return ""
    return ui.group(
        "Clear in one pass",
        ui.card_grid((_command_card(action) for action in actions), wide=True),
        count=len(actions),
        note="Each of these is one task across many repositories",
    )


def _command_card(action: triage.Action) -> str:
    label, tone = _KIND_LABELS[action.kind]
    command = (
        f'<div class="command-line"><code>{_e(action.command)}</code>{ui.copy_button(action.command)}</div>'
        if action.command is not None
        else f'<div class="card-change">{_e(action.step)}</div>'
    )
    return ui.card(
        f"<span>{_e(action.subject)}</span>",
        f'{command}<div class="card-foot">{_plural(len(action.repositories), "repository", "repositories")}: '
        f"{_repository_names(action.repositories)}</div>",
        meta=ui.badge(label, tone, dot=True),
    )


def _plan_group(site: _Site) -> str:
    actions = _of_kind(site, ActionKind.BREAKING)
    if not actions:
        return ""
    scale = max(len(action.repositories) for action in actions)
    bars = [
        Bar(
            _package_link(action.purl, "", action.subject) if action.purl is not None else _e(action.subject),
            (Segment("Repositories", len(action.repositories), Mark.WARN),),
            note=_e(action.change),
            tip=", ".join(_short_repository(name) for name in action.repositories),
        )
        for action in actions
    ]
    return ui.group(
        "Plan",
        '<div class="ui-panel card">'
        + ui.scope(ui.bar_list(bars, scale, "Breaking upgrades"), limit=_PREVIEW_ROWS, total=len(bars))
        + "</div>",
        count=len(actions),
        note="Breaking upgrades by repositories affected. These usually need a manifest edit.",
    )


def _repositories(site: _Site) -> str:
    scale = max((_work_total(card) for card in site.scorecards), default=0)
    tiers: dict[str, list[insights.Scorecard]] = defaultdict(list)
    for card in site.scorecards:
        tiers[_tier(card)].append(card)
    groups = [
        ui.group(
            title,
            ui.card_grid(_repository_card(card, scale) for card in tiers[title]),
            count=len(tiers[title]),
            note=note,
        )
        for title, note in _TIERS
        if tiers[title]
    ]
    if tiers[_CLEAN_TIER]:
        groups.append(
            ui.group(
                _CLEAN_TIER,
                ui.chip_links(_repository_chip(card.repository, 0) for card in tiers[_CLEAN_TIER]),
                count=len(tiers[_CLEAN_TIER]),
                note="Nothing flagged",
            )
        )
    exposure = _fork_exposure(site)
    if site.forks:
        groups.append(
            ui.group(
                "Forks",
                ui.chip_links(
                    _repository_chip(name, exposure[name])
                    for name in sorted(site.forks, key=lambda item: (-exposure[item], item.casefold()))
                ),
                count=len(site.forks),
                note="Tracked only. The number is vulnerable versions.",
            )
        )
    return "\n".join(
        (
            ui.page_header("Repositories", f"Worst first · {_enrichment_stamp(site)}"),
            ui.legend(_WORK_LEGEND),
            ui.scope("".join(groups), search="Filter repositories"),
        )
    )


def _tier(card: insights.Scorecard) -> str:
    if card.vulnerable:
        return "Vulnerable"
    if card.breaking:
        return "Breaking upgrades"
    if any(card.rank):
        return "Maintenance"
    return _CLEAN_TIER


def _repository_card(card: insights.Scorecard, scale: int) -> str:
    owner, _, _name = card.repository.rpartition("/")
    extras = [
        (card.vulnerable, "vulnerable"),
        (card.breaking, "breaking"),
        (card.behind - card.breaking, "minor or patch"),
    ]
    summary = " · ".join(f"{count} {label}" for count, label in extras if count)
    roles = len({*card.mixed_roles, *card.off_preferred})
    off_target = f"{card.off_target} off target" if card.off_target else ""
    drift = " · ".join(part for part in (off_target, _plural_if(roles, "role conflict")) if part)
    return ui.card(
        f'<a class="row-link" href="repos/{_e(card.repository)}/index.html">'
        f"{_e(_short_repository(card.repository))}</a>",
        ui.segments(_work_parts(card), scale)
        + f'<div class="card-foot">{_e(summary or "No upgrades pending")}</div>'
        + (f'<div class="card-foot quiet">{_e(drift)}</div>' if drift else ""),
        meta=ui.note(_e(owner)),
        href=f"repos/{card.repository}/index.html",
    )


def _plural_if(count: int, noun: str) -> str:
    return "" if not count else _plural(count, noun)


def _repository_chip(name: str, count: int) -> str:
    return ui.chip_link(_short_repository(name), f"repos/{name}/index.html", count, tip=name)


def _security(site: _Site) -> str:
    actions = _of_kind(site, ActionKind.SECURITY)
    blocked = sum(1 for action in actions if action.blocked)
    by_repository: dict[str, list[triage.Action]] = defaultdict(list)
    for action in actions:
        for repository in action.repositories:
            by_repository[repository].append(action)
    body = [ui.page_header("Security", f"Known vulnerable versions, by repository · {_enrichment_stamp(site)}")]
    if actions:
        body.append(
            ui.status_strip(
                (
                    Tile(str(len(actions)), "Packages", "with known advisories", "#repositories", Tone.BAD),
                    Tile(str(len(by_repository)), "Repositories", "need a change", "#repositories"),
                    Tile(
                        str(blocked),
                        "No clean upgrade",
                        "replace or accept",
                        "#repositories",
                        Tone.WARN if blocked else None,
                    ),
                )
            )
        )
        ordered = sorted(by_repository.items(), key=lambda item: (-len(item[1]), item[0].casefold()))
        body.append(
            ui.group(
                "Your repositories",
                ui.card_grid(_security_repository_card(name, items) for name, items in ordered),
                count=len(ordered),
                group_id="repositories",
            )
        )
    elif _checked(site):
        body.append(ui.panel("Your repositories", ui.empty_state("No known vulnerabilities"), flush=True))
    else:
        body.append(ui.panel("Your repositories", '<p class="unknown">not checked</p>', flush=True))
    body.append(_forks_panel(site))
    return "\n".join(body)


def _security_repository_card(name: str, actions: Sequence[triage.Action]) -> str:
    lines = [
        Line(
            _package_link(action.purl, "", action.subject) if action.purl is not None else _e(action.subject),
            f"{_e(action.change)} · {_e(action.step)}",
            ui.badge("Blocked", Tone.WARN) if action.blocked else "",
            ", ".join(action.details),
        )
        for action in actions
    ]
    return ui.card(
        f'<a href="repos/{_e(name)}/index.html">{_e(_short_repository(name))}</a>',
        ui.lines(lines),
        meta=ui.badge(_plural(len(actions), "package"), Tone.BAD),
    )


def _forks_panel(site: _Site) -> str:
    if not site.forks:
        return ui.collapsed_panel("Forks", '<p class="zero">no fork repositories</p>', panel_id="forks")
    exposure: dict[str, list[str]] = {name: [] for name in site.forks}
    for row in site.vulnerable:
        for repository in {*row.direct_in, *row.transitive_in}:
            if repository in exposure:
                exposure[repository].append(f"{row.component.display_name}@{row.version}")
    chips = ui.chip_links(
        ui.chip_link(
            _short_repository(name),
            f"repos/{name}/index.html",
            len(exposure[name]),
            tip=", ".join(sorted(exposure[name])) or name,
        )
        for name in sorted(site.forks, key=lambda item: (-len(exposure[item]), item.casefold()))
    )
    return ui.collapsed_panel(
        "Forks",
        chips,
        meta="Tracked only. Forks keep upstream's dependency choices.",
        panel_id="forks",
    )


def _outdated(site: _Site) -> str:
    body = [ui.page_header("Upgrades", f"Direct dependencies behind the registry · {_enrichment_stamp(site)}")]
    if not site.outdated:
        message = (
            '<p class="zero">every checked direct dependency is on its latest release</p>'
            if _checked(site)
            else '<p class="unknown">not checked</p>'
        )
        body.append(ui.panel("Upgrades", message, flush=True))
        return "\n".join(body)
    body.append(_ecosystem_upgrades_panel(site))
    body.append(
        ui.notice(
            f'Apply minor and patch upgrades with <span class="mono">{triage.ROUTINE_COMMAND}</span>, which '
            "opens one pull request per repository."
        )
    )
    scale = max(row.repositories for row in site.outdated)
    for gap, note in (
        (insights.Gap.BREAKING, "Changes the leftmost nonzero version part. Usually needs a manifest edit."),
        (insights.Gap.MINOR, "Compatible new features"),
        (insights.Gap.PATCH, "Compatible fixes"),
    ):
        rows = sorted(
            (row for row in site.outdated if row.gap is gap),
            key=lambda row: (-row.repositories, row.component.name.casefold()),
        )
        if not rows:
            continue
        bars = [_upgrade_bar(row) for row in rows]
        listing = ui.scope(ui.bar_list(bars, scale, f"{gap.value} upgrades"), limit=_PREVIEW_ROWS, total=len(bars))
        title = f"{gap.value.capitalize()} upgrades"
        body.append(
            ui.group(
                title,
                f'<div class="ui-panel card">{listing}</div>',
                count=len(rows),
                note=note,
                group_id=gap.value,
            )
        )
    return "\n".join(body)


def _ecosystem_upgrades_panel(site: _Site) -> str:
    counts: dict[Ecosystem, Counter[insights.Gap]] = defaultdict(Counter)
    for row in site.outdated:
        counts[row.component.ecosystem][row.gap] += 1
    ordered = sorted(counts.items(), key=lambda item: (-sum(item[1].values()), item[0].value))
    scale = max(sum(value.values()) for _ecosystem, value in ordered)
    bars = [
        Bar(
            _e(ecosystem.value),
            (
                Segment("Breaking", value[insights.Gap.BREAKING], Mark.WARN),
                Segment("Minor", value[insights.Gap.MINOR], Mark.ACCENT),
                Segment("Patch", value[insights.Gap.PATCH], Mark.GHOST),
            ),
        )
        for ecosystem, value in ordered
    ]
    return ui.panel(
        "By ecosystem",
        ui.legend((("Breaking", Mark.WARN), ("Minor", Mark.ACCENT), ("Patch", Mark.GHOST)))
        + ui.bar_list(bars, scale, "Upgrades by ecosystem"),
    )


def _upgrade_bar(row: insights.Outdated) -> Bar:
    versions = ", ".join(use.version for use in row.uses)
    names = sorted(_short_repository(name) for use in row.uses for name in use.repositories)
    mark = {insights.Gap.BREAKING: Mark.WARN, insights.Gap.MINOR: Mark.ACCENT, insights.Gap.PATCH: Mark.GHOST}[row.gap]
    return Bar(
        _package_link(row.component.purl, "", row.component.display_name),
        (Segment("Repositories", row.repositories, mark),),
        note=f"{_e(versions)} → {_e(row.latest)}",
        tip=", ".join(names),
    )


def _alignment(site: _Site) -> str:
    header = ui.page_header("Alignment", "Packages your repositories declare at different versions.")
    if not site.splits:
        return header + ui.panel(
            "Version drift",
            '<p class="zero">every package declared in two or more repositories uses one version</p>',
            flush=True,
        )
    groups: dict[Ecosystem, list[insights.Split]] = defaultdict(list)
    for row in site.splits:
        groups[row.component.ecosystem].append(row)
    sections = [
        ui.group(
            ecosystem.value,
            ui.card_grid(
                _lane_card(row)
                for row in sorted(rows, key=lambda row: (-len(set(row.off_target)), row.component.name.casefold()))
            ),
            count=len(rows),
        )
        for ecosystem, rows in sorted(groups.items(), key=lambda item: (-len(item[1]), item[0].value))
    ]
    return "\n".join(
        (
            header,
            ui.legend(
                (("Target version", Mark.OK), ("Off target", Mark.GHOST)),
                "Oldest to newest. Bigger dots mean more repositories.",
            ),
            ui.scope("".join(sections), search="Filter packages"),
        )
    )


def _lane_card(row: insights.Split) -> str:
    in_use = {use.version for use in row.uses}
    uses = row.uses if row.target in in_use else (*row.uses, insights.VersionUse(row.target, ()))
    ordered = sorted(
        uses,
        key=lambda use: (
            (parsed := insights.release(row.component.ecosystem, use.version)) is None,
            parsed or (),
        ),
    )
    stops = [
        Stop(
            _lane_label(use.version),
            len(use.repositories),
            use.version == row.target,
            f"{_short_ref(use.version)}: "
            f"{', '.join(_short_repository(name) for name in use.repositories) or 'not in use yet'}",
        )
        for use in ordered
    ]
    off_target = len(set(row.off_target))
    return ui.card(
        _package_link(row.component.purl, "", row.component.display_name),
        ui.lane(stops) + f'<div class="card-foot quiet">Target is {_e(row.basis.value)}</div>',
        meta=ui.note(f"{off_target} off target"),
    )


def _lane_label(version: str) -> str:
    if "@sha256:" in version:
        tag, digest = version.split("@sha256:", 1)
        return f"{tag}@{digest[:7]}"
    if len(version) == 40 and all(character in "0123456789abcdef" for character in version):
        return version[:7]
    return version


def _choices(site: _Site) -> str:
    contested = [
        use
        for use in site.roles
        if use.in_use and (len(use.in_use) > 1 or use.overlapping or use.off_preferred)
    ]
    settled = sorted(
        (use for use in site.roles if use.in_use and use not in contested),
        key=lambda item: item.role.name.casefold(),
    )
    body = [
        ui.page_header(
            "Choices",
            "Each role names packages that do the same job. Preferred is the house choice. Forks are left out.",
        )
    ]
    if contested:
        groups: dict[Ecosystem, list[insights.RoleUse]] = defaultdict(list)
        for use in contested:
            groups[use.role.ecosystem].append(use)
        body.append(ui.legend((("Preferred", Mark.OK), ("Other option", Mark.WARN), ("Banned", Mark.BAD))))
        body.extend(
            ui.group(
                ecosystem.value,
                ui.card_grid(
                    _role_card(use)
                    for use in sorted(
                        uses,
                        key=lambda item: (
                            -len(item.off_preferred),
                            -len(item.overlapping),
                            item.role.name.casefold(),
                        ),
                    )
                ),
                count=len(uses),
            )
            for ecosystem, uses in sorted(groups.items(), key=lambda item: (-len(item[1]), item[0].value))
        )
    if settled:
        badges = "".join(
            ui.badge(f"{use.role.name}: {use.in_use[0]} · {len(use.options[use.in_use[0]])}")
            for use in settled
        )
        body.append(ui.collapsed_panel("Settled roles", f'<div class="badges">{badges}</div>', meta=str(len(settled))))
    if not contested and not settled:
        body.append('<p class="zero">no roles in use</p>')
    return "\n".join(body)


def _role_card(use: insights.RoleUse) -> str:
    options = sorted(use.in_use, key=lambda option: -len(use.options[option]))
    scale = max(len(use.options[option]) for option in options)
    bars = [_role_bar(use, option) for option in options]
    pressure = [
        label
        for count, label in (
            (len(use.off_preferred), f"{len(use.off_preferred)} off preferred"),
            (len(use.overlapping), f"{len(use.overlapping)} mixed"),
        )
        if count
    ]
    badge = ui.badge(" · ".join(pressure), Tone.WARN) if pressure else ""
    excepted = ""
    if use.excepted:
        items = "".join(
            f'<div class="version-use">{_repo_link(repository, "")} {ui.note(_e(reason))}</div>'
            for repository, reason in use.excepted
        )
        excepted = f"<details><summary>Accepted exceptions</summary>{items}</details>"
    preferred = use.role.preferred or "no house choice"
    listing = "".join(
        f'<div class="version-use"><span class="card-option">{_e(option)}</span> '
        f"{_repo_list(use.options[option], limit=12)}</div>"
        for option in options
    )
    return ui.card(
        f"<span>{_e(use.role.name)}</span>",
        f'<div class="card-foot quiet">Preferred: {_e(preferred)}</div>'
        + ui.bar_list(bars, scale, f"{use.role.name} adoption")
        + f"<details><summary>Repositories</summary>{listing}</details>"
        + excepted,
        meta=badge,
        card_id=f"role-{use.role.name}",
    )


def _role_bar(use: insights.RoleUse, option: str) -> Bar:
    repositories = use.options[option]
    packages = sorted(
        {
            package
            for repository in repositories
            for package in use.packages.get((repository, option), ())
            if package != option
        }
    )
    more = f" +{len(packages) - 2}" if len(packages) > 2 else ""
    note = _e(f"{', '.join(packages[:2])}{more}") if packages else ""
    mark = Mark.OK if option == use.role.preferred else Mark.BAD if option in use.role.bans else Mark.WARN
    return Bar(
        _e(option),
        (Segment("Repositories", len(repositories), mark),),
        note=note,
    )


def _bloat(site: _Site) -> str:
    rows = site.weights[:_BLOAT_ROWS]
    scale = max((row.pulls for row in rows), default=0)
    bars = [_weight_bar(row) for row in rows]
    body = [
        ui.page_header(
            "Bloat",
            "Lockfile packages each direct dependency brings in. Lockfiles list every "
            "platform's packages, so counts are upper bounds.",
        ),
        ui.panel(
            "Heaviest dependencies",
            ui.legend(
                (("Only via it", Mark.ACCENT), ("Shared", Mark.GHOST)),
                "Sorted by packages only it brings in. Removing it drops those.",
            )
            + ui.scope(
                ui.bar_list(bars, scale, "Heaviest dependencies"),
                search="Filter dependencies",
                limit=_LIST_ROWS,
                total=len(bars),
            )
            if rows
            else '<p class="zero">no direct dependency brings in lockfile packages</p>',
        ),
    ]
    duplicates = sorted(
        ((name, found) for name, data in site.graph.repositories.items() if (found := _duplicates(site, data))),
        key=lambda item: (-len(item[1]), item[0].casefold()),
    )
    widest = max((len(found) for _name, found in duplicates), default=0)
    body.append(
        ui.collapsed_panel(
            "Duplicate versions",
            ui.scope(
                ui.bar_list(
                    [_duplicate_bar(site, name, found) for name, found in duplicates],
                    widest,
                    "Duplicate versions",
                ),
                limit=_LIST_ROWS,
                total=len(duplicates),
            )
            if duplicates
            else '<p class="zero">no repository carries two versions of one package</p>',
            meta="Both versions are built",
        )
    )
    return "\n".join(body)


def _weight_bar(row: insights.Weight) -> Bar:
    dev = " " + ui.badge("dev") if row.scopes == {Scope.DEV} else ""
    return Bar(
        f"{_package_link(row.component.purl, '', f'{row.component.display_name}@{row.version}')}{dev}",
        (
            Segment("Only via it", row.sole, Mark.ACCENT),
            Segment("Shared", row.pulls - row.sole, Mark.GHOST),
        ),
        note=_e(_short_repository(row.repository)),
    )


def _duplicate_bar(site: _Site, name: str, found: Sequence[tuple[str, list[str]]]) -> Bar:
    items = "".join(
        f"<li>{_package_link(purl, '', _display_name(site, purl))} "
        f'<span class="mono">{_e(", ".join(versions))}</span></li>'
        for purl, versions in found
    )
    return Bar(
        f'<a href="repos/{_e(name)}/index.html">{_e(_short_repository(name))}</a>',
        (Segment("Packages", len(found), Mark.ACCENT),),
        tail=f'<details><summary>Packages</summary><ul class="plain-list">{items}</ul></details>',
    )


def _packages(site: _Site) -> str:
    uses = _package_uses(site)
    ranked = sorted(uses, key=lambda use: (-use.repositories, -use.direct, use.label.casefold()))
    ecosystems = Counter(use.ecosystem for use in uses)
    bars = [
        Bar(_e(ecosystem.value), (Segment("Packages", count, Mark.ACCENT),))
        for ecosystem, count in ecosystems.most_common()
    ]
    mix = ui.panel(
        "Packages by ecosystem",
        ui.bar_list(bars, max(ecosystems.values(), default=0), "Packages by ecosystem"),
    )
    cards = []
    for ecosystem, _count_value in ecosystems.most_common():
        top = [use for use in ranked if use.ecosystem is ecosystem][:_TOP_PACKAGES]
        widest = max((use.repositories for use in top), default=0)
        cards.append(
            ui.card(
                f"<span>{_e(ecosystem.value)}</span>",
                ui.bar_list(
                    [
                        Bar(
                            _package_link(use.purl, "", use.label),
                            (Segment("Repositories", use.repositories, Mark.ACCENT),),
                        )
                        for use in top
                    ],
                    widest,
                    f"Most used {ecosystem.value} packages",
                ),
                meta=ui.note("repositories using each"),
            )
        )
    header = ui.page_header(
        "Packages",
        f"{len(site.graph.components)} packages in {len(ecosystems)} ecosystems. "
        'Press <span class="mono">/</span> to find one.',
    )
    grid = ui.group("Most used", ui.card_grid(cards, wide=True), note=f"Top {_TOP_PACKAGES} per ecosystem")
    sections = "".join(
        _package_section(site, purl) for purl in sorted(site.graph.components, key=str.casefold)
    )
    return f'<div id="package-index" class="page-stack">{header}{mix}{grid}</div>{sections}'


def _coverage(site: _Site) -> str:
    states: Counter[str] = Counter(data.state.value for data in site.graph.repositories.values())
    kinds: Counter[CoverageKind] = Counter(
        issue.kind for data in site.graph.repositories.values() for issue in data.issues
    )
    body = [
        ui.page_header(
            "Coverage",
            f"{len(site.graph.repositories)} repositories · {states['complete']} complete · "
            f"{states['partial']} partial · {states['failed']} failed · "
            f"{states['unavailable']} unavailable · {sum(kinds.values())} issues",
        ),
    ]
    if kinds:
        body.append(
            ui.group(
                "What to fix first",
                ui.card_grid(
                    (
                        _coverage_card(kind, count)
                        for kind, count in sorted(kinds.items(), key=lambda item: (item[0].severity, -item[1]))
                    ),
                    wide=True,
                ),
                count=len(kinds),
                note="Gaps are known limits with a file and a reason attached",
            )
        )
    else:
        body.append(
            ui.panel("What to fix first", ui.empty_state("Every repository is complete", "Nothing to fix."), flush=True)
        )
    attention = sorted(
        ((name, data) for name, data in site.graph.repositories.items() if data.state.value != "complete"),
        key=lambda item: (
            -{"unavailable": 2, "failed": 1}.get(item[1].state.value, 0),
            -len(item[1].issues),
            item[0].casefold(),
        ),
    )
    if attention:
        widest = max(len(data.issues) for _name, data in attention)
        bars = [_coverage_repository_bar(name, data) for name, data in attention]
        body.append(
            ui.panel(
                "Repositories",
                ui.scope(
                    ui.bar_list(bars, widest, "Coverage issues by repository"),
                    search="Filter repositories",
                    limit=_PREVIEW_ROWS,
                    total=len(bars),
                ),
                meta="Issues per repository",
            )
        )
    unsupported = [
        (name, issue.path or "")
        for name, data in site.graph.repositories.items()
        for issue in data.issues
        if issue.kind is CoverageKind.UNSUPPORTED_FORMAT
    ]
    body.append(
        ui.collapsed_panel(
            "Detected, not parsed",
            ui.table(
                "Detected, not parsed",
                (Column("Repository"), Column("Path", wrap=True)),
                [Row((_repo_link(name, ""), f'<span class="mono">{_e(path)}</span>')) for name, path in unsupported],
                limit=_LIST_ROWS,
            )
            if unsupported
            else '<p class="zero">none</p>',
            meta=f"{len(unsupported)} files with no parser yet",
            flush=True,
        )
    )
    body.append(ui.collapsed_panel("Source risk", _risk_bars(site)))
    return "\n".join(body)


def _coverage_card(kind: CoverageKind, count: int) -> str:
    label, meaning, action = _COVERAGE_HELP[kind]
    tone = Tone.BAD if kind.severity == "error" else Tone.NEUTRAL
    return ui.card(
        f"<span>{_e(label)}</span>",
        f'<div class="card-foot">{_e(meaning)}</div><div class="card-change">{_e(action)}</div>',
        meta=ui.badge(str(count), tone),
    )


def _coverage_repository_bar(name: str, data: RepositoryData) -> Bar:
    kinds = Counter(issue.kind for issue in data.issues)
    missing = "; ".join(
        f"{_COVERAGE_HELP[kind][0]} x {count}"
        for kind, count in sorted(kinds.items(), key=lambda item: -item[1])
    )
    tone = Mark.BAD if data.state.value in ("failed", "unavailable") else Mark.GHOST
    return Bar(
        f'<a href="repos/{_e(name)}/index.html">{_e(_short_repository(name))}</a>',
        (Segment("Issues", len(data.issues), tone),),
        tip=missing,
    )


def _risk_bars(site: _Site) -> str:
    risk = _table(site.report["source_risk"])
    signals = (
        ("Git sources", "git_sources"),
        ("Unpinned Actions", "unpinned_actions"),
        ("Floating image tags", "floating_images"),
        ("Missing locks", "missing_locks"),
        ("Unpinned artifacts", "unpinned_artifacts"),
        ("Vendored code", "vendored"),
    )
    counted = [(label, _value(risk.get(key))) for label, key in signals]
    bars = [
        Bar(_e(label), (Segment("Count", value, Mark.GHOST),))
        for label, value in sorted(counted, key=lambda item: -item[1])
    ]
    return ui.bar_list(bars, max(value for _label, value in counted), "Source risk")


def _todo_panel(site: _Site, name: str) -> str:
    sections = _repository_todo(site, name)
    if not sections:
        return ui.panel("To do here", '<p class="zero">nothing flagged</p>', panel_id="todo", flush=True)
    return ui.group(
        "To do here",
        ui.card_grid(
            (
                ui.card(f"<span>{_e(title)}</span>", ui.lines(lines), meta=ui.badge(str(len(lines)), tone))
                for title, tone, lines in sections
            ),
            wide=True,
        ),
        count=sum(len(lines) for _title, _tone, lines in sections),
        group_id="todo",
    )


def _repository_todo(site: _Site, name: str) -> list[tuple[str, Tone, list[Line]]]:
    prefix = "../../../"
    security = [
        Line(
            _package_link(row.component.purl, prefix, f"{row.component.display_name}@{row.version}"),
            f"{'declared' if name in row.direct_in else 'locked'} · {_advisories_label(len(row.advisories))}",
            tip=", ".join(row.advisories),
        )
        for row in site.vulnerable
        if name in row.direct_in or name in row.transitive_in
    ]
    breaking: list[Line] = []
    routine: list[Line] = []
    behind: set[str] = set()
    for row in site.outdated:
        for use in row.uses:
            if name not in use.repositories:
                continue
            behind.add(row.component.purl)
            severity = insights.gap(row.component.ecosystem, use.version, row.latest)
            target = breaking if severity is insights.Gap.BREAKING else routine
            target.append(
                Line(
                    _package_link(row.component.purl, prefix, row.component.display_name),
                    f"{_e(use.version)} → {_e(row.latest)}",
                )
            )
    drift = [
        Line(
            _package_link(split.component.purl, prefix, split.component.display_name),
            f"{_e(_short_ref(next(use.version for use in split.uses if name in use.repositories)))} → "
            f"{_e(_short_ref(split.target))}",
        )
        for split in site.splits
        if name in split.off_target and split.component.purl not in behind
    ]
    roles = [
        Line(
            f'<a href="{prefix}choices.html#role-{_e(use.role.name)}">{_e(use.role.name)}</a>',
            f"uses {_e(', '.join(option for option, owners in use.options.items() if name in owners))}"
            + (f" · preferred {_e(use.role.preferred)}" if use.role.preferred is not None else ""),
        )
        for use in site.roles
        if name in use.overlapping or name in use.off_preferred
    ]
    return [
        (title, tone, lines)
        for title, tone, lines in (
            ("Security", Tone.BAD, security),
            ("Breaking upgrades", Tone.WARN, breaking),
            ("Minor and patch upgrades", Tone.NEUTRAL, routine),
            ("Version drift", Tone.NEUTRAL, drift),
            ("Role conflicts", Tone.NEUTRAL, roles),
        )
        if lines
    ]


def _enrichment_stamp(site: _Site) -> str:
    fetched = site.cache.fetched_at
    if fetched is None:
        return "no registry data"
    return f"registry data from {time.strftime('%Y-%m-%d', time.gmtime(fetched))}"


def _changes_line(site: _Site) -> str:
    value = site.diff_value
    if value is None or not value.baseline:
        return '<a href="changes.html">no change baseline</a>'
    return f'<a href="changes.html">{_summary_metrics(value)}</a>'


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


def _repo_link(name: str, prefix: str) -> str:
    return f'<a href="{prefix}repos/{_e(name)}/index.html" class="mono">{_e(name)}</a>'


def _package_link(purl: str, prefix: str, text: str) -> str:
    return f'<a href="{prefix}packages.html#pkg={_fragment(purl)}" class="mono">{_e(text)}</a>'


def _fragment(purl: str) -> str:
    return _e(quote(purl, safe=""))


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


def _table(value: object) -> Mapping[str, Any]:
    if isinstance(value, Mapping):
        return cast(Mapping[str, Any], value)
    return {}


def _value(value: object) -> int:
    if isinstance(value, Mapping):
        item = cast(Mapping[str, Any], value).get("value", 0)
        return item if isinstance(item, int) else 0
    return value if isinstance(value, int) else 0
