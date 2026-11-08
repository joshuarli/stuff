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
from repomod.dependencies.ui import Column, Filter, NavGroup, NavLink, Row, Tile, Tone
from repomod.dependencies.ui import escape as _e
from repomod.rules import actions as actions_rule
from repomod.rules.python_standard import PYTHON_TOOL_NAMES

MARKER = ".repomod-dashboard"
ASSETS = Path(__file__).with_name("assets")
_ACTION_ROWS = 12
_LIST_ROWS = 25
_BLOAT_ROWS = 150
_CONSUMER_ROWS = 20
_TOP_PACKAGES = 12
_KIND_LABELS = {
    ActionKind.SECURITY: ("Security", Tone.BAD),
    ActionKind.UNREADABLE: ("Unreadable", Tone.BAD),
    ActionKind.LOCKFILE: ("Lockfile", Tone.WARN),
    ActionKind.BREAKING: ("Breaking", Tone.WARN),
    ActionKind.TOOLING: ("Tooling", Tone.NEUTRAL),
    ActionKind.ROUTINE: ("Routine", Tone.NEUTRAL),
}
_GAP_TONES = {
    insights.Gap.BREAKING: Tone.WARN,
    insights.Gap.MINOR: Tone.NEUTRAL,
    insights.Gap.PATCH: Tone.NEUTRAL,
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


def _overview(site: _Site) -> str:
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
    body.append(
        ui.panel(
            "Next",
            _action_table(site.actions)
            if site.actions
            else ui.empty_state(
                "Nothing to fix",
                "No vulnerabilities, unreadable checkouts, missing lockfiles, or pending upgrades.",
            ),
            meta="Most urgent first, then the changes that fix the most repositories",
            flush=True,
        )
    )
    exposed = sum(1 for count in _fork_exposure(site).values() if count)
    if site.forks:
        body.append(
            ui.notice(
                f"{len(site.forks)} forks keep upstream's dependency choices and are left out of "
                f'this list. <a href="security.html#forks">{exposed} carry vulnerable versions</a>.'
            )
        )
    return "\n".join(body)


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


def _action_table(actions: Sequence[triage.Action]) -> str:
    kinds = Counter(action.kind for action in actions)
    filters = [Filter("", "All", len(actions), pressed=True)]
    filters.extend(
        Filter(kind.value, _KIND_LABELS[kind][0], kinds[kind]) for kind in ActionKind if kinds[kind]
    )
    return ui.table(
        "Next actions",
        (
            Column("Type"),
            Column("Change"),
            Column("Why", wrap=True),
            Column("Where"),
            Column("Next step", wrap=True),
        ),
        [_action_row(action) for action in actions],
        filters=filters,
        limit=_ACTION_ROWS,
    )


def _action_row(action: triage.Action) -> Row:
    label, tone = _KIND_LABELS[action.kind]
    subject = (
        _package_link(action.purl, "", action.subject)
        if action.purl is not None
        else f'<span class="entity">{_e(action.subject)}</span>'
    )
    command = (
        f'<span class="command">{_e(action.command)}</span>{ui.copy_button(action.command)}'
        if action.command is not None
        else ""
    )
    return Row(
        (
            ui.badge(label, tone, dot=True),
            ui.stack(subject, _e(action.change)),
            _e(action.reason),
            _where(action.repositories, ""),
            f'<span class="step"><span>{_e(action.step)}</span>{command}</span>',
        ),
        tags=(action.kind.value,),
    )


def _where(repositories: Sequence[str], prefix: str) -> str:
    if len(repositories) == 1:
        return _repo_link(repositories[0], prefix)
    links = "".join(f"<span>{_repo_link(name, prefix)}</span>" for name in repositories)
    return (
        f"<details><summary>{len(repositories)} repositories</summary>"
        f'<div class="where-list">{links}</div></details>'
    )


def _repositories(site: _Site) -> str:
    exposure = _fork_exposure(site)
    rows = [_scorecard_row(card) for card in site.scorecards]
    rows.extend(
        _fork_row(name, exposure[name])
        for name in sorted(site.forks, key=lambda item: (-exposure[item], item.casefold()))
    )
    tags = Counter(tag for row in rows for tag in row.tags)
    filters = (
        Filter("owned", "Owned", tags["owned"], pressed=True),
        Filter("vulnerable", "Vulnerable", tags["vulnerable"]),
        Filter("breaking", "Breaking", tags["breaking"]),
        Filter("clean", "Clean", tags["clean"]),
        Filter("fork", "Forks", tags["fork"]),
    )
    return "\n".join(
        (
            ui.page_header("Repositories", "Worst first. Open one to see everything to do there."),
            ui.panel(
                "Repositories",
                ui.table(
                    "Repositories",
                    (
                        Column("Repository"),
                        Column("Vulnerable", numeric=True),
                        Column("Breaking", numeric=True),
                        Column("Behind", numeric=True),
                        Column("Off target", numeric=True),
                        Column("Role conflicts", numeric=True),
                    ),
                    rows,
                    filters=filters,
                    search="Filter repositories",
                    limit=_LIST_ROWS,
                ),
                meta=_enrichment_stamp(site),
                flush=True,
            ),
        )
    )


def _scorecard_row(card: insights.Scorecard) -> Row:
    roles = len({*card.mixed_roles, *card.off_preferred})
    tags = ["owned"]
    if card.vulnerable:
        tags.append("vulnerable")
    if card.breaking:
        tags.append("breaking")
    if not any(card.rank):
        tags.append("clean")
    return Row(
        (
            _repo_row_link(card.repository),
            _count(card.vulnerable, "bad"),
            _count(card.breaking, "warn"),
            _count(card.behind),
            _count(card.off_target),
            _count(roles),
        ),
        tags=tags,
        href=f"repos/{card.repository}/index.html",
    )


def _fork_row(name: str, vulnerable: int) -> Row:
    tags = ["fork", "vulnerable"] if vulnerable else ["fork"]
    return Row(
        (
            _repo_row_link(name),
            _count(vulnerable, "bad"),
            _count(0),
            _count(0),
            _count(0),
            _count(0),
        ),
        tags=tags,
        href=f"repos/{name}/index.html",
    )


def _repo_row_link(name: str) -> str:
    return f'<a class="row-link" href="repos/{_e(name)}/index.html">{_e(name)}</a>'


def _count(value: int, tone: str = "") -> str:
    if not value:
        return '<span class="count-quiet">·</span>'
    return f'<span class="count-{tone}">{value}</span>' if tone else str(value)


def _fork_exposure(site: _Site) -> dict[str, int]:
    exposure = dict.fromkeys(site.forks, 0)
    for row in site.vulnerable:
        for repository in {*row.direct_in, *row.transitive_in}:
            if repository in exposure:
                exposure[repository] += 1
    return exposure


def _security(site: _Site) -> str:
    actions = _of_kind(site, ActionKind.SECURITY)
    blocked = sum(1 for action in actions if action.blocked)
    body = [
        ui.page_header(
            "Security",
            f"Known vulnerable versions in your repositories · {_enrichment_stamp(site)}",
        )
    ]
    if actions:
        table = ui.table(
            "Vulnerable packages",
            (
                Column("Package"),
                Column("Advisories", wrap=True),
                Column("Where"),
                Column("Next step", wrap=True),
            ),
            [_security_row(action) for action in actions],
            filters=(
                Filter("", "All", len(actions), pressed=True),
                Filter("fixable", "Upgrade available", len(actions) - blocked),
                Filter("blocked", "No clean upgrade", blocked),
            ),
            search="Filter packages",
            limit=_LIST_ROWS,
        )
    elif _checked(site):
        table = ui.empty_state("No known vulnerabilities", "Every checked version is clear.")
    else:
        table = '<p class="unknown">not checked</p>'
    body.append(ui.panel("Your repositories", table, flush=True))
    body.append(_forks_panel(site))
    return "\n".join(body)


def _security_row(action: triage.Action) -> Row:
    if action.purl is None:
        raise ValueError(f"security action has no package: {action.subject}")
    advisories = (
        _e(action.details[0])
        if len(action.details) == 1
        else f"<details><summary>{_e(action.reason)}</summary>"
        f'<div class="where-list mono">{"".join(f"<span>{_e(item)}</span>" for item in action.details)}</div></details>'
    )
    return Row(
        (
            ui.stack(_package_link(action.purl, "", action.subject), _e(action.change)),
            advisories,
            _where(action.repositories, ""),
            _e(action.step),
        ),
        tags=("blocked" if action.blocked else "fixable",),
    )


def _forks_panel(site: _Site) -> str:
    exposure: dict[str, list[str]] = {name: [] for name in site.forks}
    for row in site.vulnerable:
        for repository in {*row.direct_in, *row.transitive_in}:
            if repository in exposure:
                exposure[repository].append(f"{row.component.display_name}@{row.version}")
    if not site.forks:
        return ui.collapsed_panel("Forks", '<p class="zero">no fork repositories</p>', panel_id="forks")
    rows = [
        Row(
            (
                _repo_link(name, ""),
                _count(len(exposure[name]), "bad"),
                f'<span class="mono">{_e(", ".join(sorted(exposure[name])))}</span>',
            ),
            href=f"repos/{name}/index.html",
        )
        for name in sorted(site.forks, key=lambda item: (-len(exposure[item]), item.casefold()))
    ]
    return ui.collapsed_panel(
        "Forks",
        ui.table(
            "Forks",
            (Column("Fork"), Column("Vulnerable", numeric=True), Column("Versions", wrap=True)),
            rows,
        ),
        meta="Tracked only. Forks keep upstream's dependency choices.",
        panel_id="forks",
        flush=True,
    )


def _outdated(site: _Site) -> str:
    counts = Counter(row.gap for row in site.outdated)
    if site.outdated:
        table = ui.table(
            "Upgrades",
            (Column("Package"), Column("Type"), Column("Latest"), Column("In use", wrap=True)),
            [_outdated_row(row) for row in site.outdated],
            filters=(
                Filter("breaking", "Breaking", counts[insights.Gap.BREAKING], pressed=True),
                Filter("minor", "Minor", counts[insights.Gap.MINOR]),
                Filter("patch", "Patch", counts[insights.Gap.PATCH]),
                Filter("", "All", len(site.outdated)),
            ),
            search="Filter packages",
            limit=_LIST_ROWS,
        )
    elif _checked(site):
        table = '<p class="zero">every checked direct dependency is on its latest release</p>'
    else:
        table = '<p class="unknown">not checked</p>'
    return "\n".join(
        (
            ui.page_header("Upgrades", f"Direct dependencies behind the registry · {_enrichment_stamp(site)}"),
            ui.notice(
                "Apply minor and patch upgrades with "
                f'<span class="mono">{triage.ROUTINE_COMMAND}</span>, which opens one pull request '
                "per repository. Breaking upgrades usually need a manifest edit."
            ),
            ui.panel(
                "Upgrades",
                table,
                meta="Breaking changes the leftmost nonzero version part",
                flush=True,
            ),
        )
    )


def _outdated_row(row: insights.Outdated) -> Row:
    uses = "".join(
        f'<div class="version-use"><span class="mono">{_e(use.version)}</span> {_repo_list(use.repositories)}</div>'
        for use in row.uses
    )
    in_use = (
        uses
        if row.repositories <= 2
        else f"<details><summary>{row.repositories} repositories</summary>{uses}</details>"
    )
    return Row(
        (
            ui.stack(
                _package_link(row.component.purl, "", row.component.display_name),
                _e(row.component.ecosystem.value),
            ),
            ui.badge(row.gap.value.capitalize(), _GAP_TONES[row.gap]),
            f'<span class="mono">{_e(row.latest)}</span>',
            in_use,
        ),
        tags=(row.gap.value,),
    )


def _repo_list(repositories: Sequence[str], prefix: str = "", limit: int = 6) -> str:
    links = [_repo_link(name, prefix) for name in repositories]
    if len(links) <= limit:
        return ", ".join(links)
    return (
        ", ".join(links[:limit])
        + f"<details><summary>{len(links) - limit} more</summary>{', '.join(links[limit:])}</details>"
    )


def _alignment(site: _Site) -> str:
    rows = sorted(site.splits, key=lambda row: (-len(set(row.off_target)), row.component.name.casefold()))
    table = (
        ui.table(
            "Version drift",
            (
                Column("Package"),
                Column("Target"),
                Column("Off target", numeric=True),
                Column("Versions in use", wrap=True),
            ),
            [_split_row(row) for row in rows],
            search="Filter packages",
            limit=_LIST_ROWS,
        )
        if rows
        else '<p class="zero">every package declared in two or more repositories uses one version</p>'
    )
    return "\n".join(
        (
            ui.page_header("Alignment", "Packages your repositories declare at different versions."),
            ui.panel(
                "Version drift",
                table,
                meta="Target is the approved pin, then latest, then the newest or most common in use",
                flush=True,
            ),
        )
    )


def _split_row(row: insights.Split) -> Row:
    uses = "".join(
        f'<div class="version-use"><span class="mono">{_e(_short_ref(use.version))}</span> '
        f"{_repo_list(use.repositories)}</div>"
        for use in row.uses
    )
    return Row(
        (
            ui.stack(
                _package_link(row.component.purl, "", row.component.display_name),
                _e(row.component.ecosystem.value),
            ),
            ui.stack(f'<span class="mono">{_e(_short_ref(row.target))}</span>', _e(row.basis.value)),
            str(len(set(row.off_target))),
            f"<details><summary>{_plural(len(row.uses), 'version')}</summary>{uses}</details>",
        )
    )


def _plural(count: int, noun: str) -> str:
    return f"{count} {noun}" if count == 1 else f"{count} {noun}s"


def _short_ref(version: str) -> str:
    if "@sha256:" in version:
        tag, digest = version.split("@sha256:", 1)
        return f"{tag}@sha256:{digest[:12]}…"
    return version


def _choices(site: _Site) -> str:
    contested = [
        use
        for use in site.roles
        if use.in_use and (len(use.in_use) > 1 or use.overlapping or use.off_preferred)
    ]
    contested.sort(
        key=lambda item: (-len(item.off_preferred), -len(item.overlapping), item.role.name.casefold())
    )
    settled = sorted(
        (use for use in site.roles if use.in_use and use not in contested),
        key=lambda item: item.role.name.casefold(),
    )
    body = [
        ui.page_header(
            "Choices",
            "Each role names packages that do the same job. Preferred is the house choice. "
            "Forks are left out.",
        )
    ]
    if contested:
        body.append(
            ui.panel(
                "Contested roles",
                ui.table(
                    "Contested roles",
                    (
                        Column("Role"),
                        Column("Preferred"),
                        Column("Adoption"),
                        Column("Off preferred", numeric=True),
                        Column("Mixed", numeric=True),
                        Column("Options", wrap=True),
                    ),
                    [_role_row(use) for use in contested],
                    limit=_LIST_ROWS,
                ),
                meta="Mixed means a repository declares more than one option",
                flush=True,
            )
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


def _role_row(use: insights.RoleUse) -> Row:
    preferred = use.role.preferred
    options = sorted(use.in_use, key=lambda option: -len(use.options[option]))
    using = len({repository for option in options for repository in use.options[option]})
    on_preferred = len(use.options.get(preferred, ())) if preferred is not None else 0
    if preferred is not None and preferred in use.in_use:
        adoption = (
            f'<span class="meter-cell start">{ui.meter(on_preferred, using)}'
            f"{ui.note(f'{on_preferred} of {using}')}</span>"
        )
    else:
        adoption = ui.note("no repository on it" if preferred is not None else "no house choice")
    return Row(
        (
            ui.stack(f'<span class="entity">{_e(use.role.name)}</span>', _e(use.role.ecosystem.value)),
            f'<span class="mono">{_e(preferred or "—")}</span>',
            adoption,
            _count(len(use.off_preferred), "warn"),
            _count(len(use.overlapping), "warn"),
            f"<details><summary>{_plural(len(options), 'option')}</summary>{_role_options(use, options)}</details>",
        ),
        row_id=f"role-{use.role.name}",
    )


def _role_options(use: insights.RoleUse, options: Sequence[str]) -> str:
    parts = []
    for option in options:
        repositories = use.options[option]
        packages = sorted(
            {
                package
                for repository in repositories
                for package in use.packages.get((repository, option), ())
                if package != option
            }
        )
        more = f" +{len(packages) - 3}" if len(packages) > 3 else ""
        extra = f" ({', '.join(packages[:3])}{more})" if packages else ""
        banned = " · banned" if option in use.role.bans else ""
        parts.append(
            f'<div class="version-use"><span class="mono">{_e(option)}</span>'
            f"{ui.note(_e(f'{extra}{banned}'))} {_repo_list(repositories, limit=12)}</div>"
        )
    if use.excepted:
        items = "".join(
            f'<div class="version-use">{_repo_link(repository, "")} {ui.note(_e(reason))}</div>'
            for repository, reason in use.excepted
        )
        parts.append(f'<div class="change-group">Accepted exceptions</div>{items}')
    return "".join(parts)


def _bloat(site: _Site) -> str:
    rows = site.weights[:_BLOAT_ROWS]
    widest = max((row.sole for row in site.weights), default=0)
    body = [
        ui.page_header(
            "Bloat",
            "Lockfile packages each direct dependency brings in. Lockfiles list every "
            "platform's packages, so counts are upper bounds.",
        ),
        ui.panel(
            "Heaviest dependencies",
            ui.table(
                "Heaviest dependencies",
                (
                    Column("Dependency"),
                    Column("Repository"),
                    Column("Only via it", numeric=True),
                    Column("Pulls in", numeric=True),
                ),
                [_weight_row(row, widest) for row in rows],
                search="Filter dependencies",
                limit=_LIST_ROWS,
            )
            if rows
            else '<p class="zero">no direct dependency brings in lockfile packages</p>',
            meta="Removing it drops the packages counted in Only via it",
            flush=True,
        ),
    ]
    duplicates = sorted(
        ((name, found) for name, data in site.graph.repositories.items() if (found := _duplicates(site, data))),
        key=lambda item: (-len(item[1]), item[0].casefold()),
    )
    body.append(
        ui.collapsed_panel(
            "Duplicate versions",
            ui.table(
                "Duplicate versions",
                (Column("Repository"), Column("Packages", numeric=True), Column("Duplicated", wrap=True)),
                [_duplicate_row(site, name, found) for name, found in duplicates],
                limit=_LIST_ROWS,
            )
            if duplicates
            else '<p class="zero">no repository carries two versions of one package</p>',
            meta="Both versions are built",
            flush=True,
        )
    )
    return "\n".join(body)


def _weight_row(row: insights.Weight, widest: int) -> Row:
    dev = " " + ui.badge("dev") if row.scopes == {Scope.DEV} else ""
    return Row(
        (
            f"{_package_link(row.component.purl, '', f'{row.component.display_name}@{row.version}')}{dev}",
            _repo_link(row.repository, ""),
            f'<span class="meter-cell">{ui.meter(row.sole, widest)}<span>{row.sole}</span></span>',
            str(row.pulls),
        )
    )


def _duplicate_row(site: _Site, name: str, found: Sequence[tuple[str, list[str]]]) -> Row:
    items = "".join(
        f"<li>{_package_link(purl, '', _display_name(site, purl))} "
        f'<span class="mono">{_e(", ".join(versions))}</span></li>'
        for purl, versions in found
    )
    return Row(
        (
            _repo_link(name, ""),
            str(len(found)),
            f'<details><summary>show</summary><ul class="plain-list">{items}</ul></details>',
        )
    )


def _display_name(site: _Site, purl: str) -> str:
    component = site.graph.components.get(purl)
    return component.display_name if component is not None else purl


def _packages(site: _Site) -> str:
    uses = _package_uses(site)
    ranked = sorted(uses, key=lambda use: (-use.repositories, -use.direct, use.label.casefold()))
    ecosystems = Counter(use.ecosystem for use in uses)
    shown: list[_PackageUse] = []
    for ecosystem in ecosystems:
        shown.extend([use for use in ranked if use.ecosystem is ecosystem][:_TOP_PACKAGES])
    shown.sort(key=lambda use: (-use.repositories, -use.direct, use.label.casefold()))
    widest = max((use.repositories for use in shown), default=0)
    filters = [Filter("", "All", len(shown), pressed=True)]
    filters.extend(
        Filter(ecosystem.value, ecosystem.value, sum(1 for use in shown if use.ecosystem is ecosystem))
        for ecosystem, _count_value in ecosystems.most_common()
    )
    index = ui.panel(
        "Most used",
        ui.table(
            "Most used packages",
            (
                Column("Package"),
                Column("Ecosystem"),
                Column("Repositories", numeric=True),
                Column("Declared in", numeric=True),
            ),
            [
                Row(
                    (
                        _package_link(use.purl, "", use.label),
                        _e(use.ecosystem.value),
                        f'<span class="meter-cell">{ui.meter(use.repositories, widest)}'
                        f"<span>{use.repositories}</span></span>",
                        str(use.direct),
                    ),
                    tags=(use.ecosystem.value,),
                )
                for use in shown
            ],
            filters=filters,
            limit=_LIST_ROWS,
        ),
        meta=f"Top {_TOP_PACKAGES} per ecosystem",
        flush=True,
    )
    header = ui.page_header(
        "Packages",
        f"{len(site.graph.components)} packages in {len(ecosystems)} ecosystems. "
        'Press <span class="mono">/</span> to find one.',
    )
    sections = "".join(
        _package_section(site, purl) for purl in sorted(site.graph.components, key=str.casefold)
    )
    return f'<div id="package-index">{header}{index}</div>{sections}'


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
        ui.panel(
            "What to fix first",
            ui.table(
                "Coverage issues",
                (
                    Column("Issue"),
                    Column("Count", numeric=True),
                    Column("What it means", wrap=True),
                    Column("What to do", wrap=True),
                ),
                [
                    _coverage_row(kind, count)
                    for kind, count in sorted(kinds.items(), key=lambda item: (item[0].severity, -item[1]))
                ],
            )
            if kinds
            else ui.empty_state("Every repository is complete", "Nothing to fix."),
            meta="Gaps are known limits with a file and a reason attached",
            flush=True,
        ),
    ]
    attention = sorted(
        ((name, data) for name, data in site.graph.repositories.items() if data.state.value != "complete"),
        key=lambda item: (
            -{"unavailable": 2, "failed": 1}.get(item[1].state.value, 0),
            -len(item[1].issues),
            item[0].casefold(),
        ),
    )
    if attention:
        body.append(
            ui.panel(
                "Repositories",
                ui.table(
                    "Repositories with coverage issues",
                    (
                        Column("Repository"),
                        Column("State"),
                        Column("Issues", numeric=True),
                        Column("What is missing", wrap=True),
                    ),
                    [_coverage_repository_row(name, data) for name, data in attention],
                    search="Filter repositories",
                    limit=_ACTION_ROWS,
                ),
                flush=True,
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
    body.append(ui.collapsed_panel("Source risk", _risk_table(site), flush=True))
    return "\n".join(body)


def _coverage_row(kind: CoverageKind, count: int) -> Row:
    label, meaning, action = _COVERAGE_HELP[kind]
    tone = Tone.BAD if kind.severity == "error" else Tone.NEUTRAL
    return Row(
        (
            ui.stack(ui.badge(label, tone), f'<span class="mono">{_e(kind.value)}</span>'),
            str(count),
            _e(meaning),
            _e(action),
        )
    )


def _coverage_repository_row(name: str, data: RepositoryData) -> Row:
    kinds = Counter(issue.kind for issue in data.issues)
    missing = "; ".join(
        f"{_COVERAGE_HELP[kind][0]} x {count}"
        for kind, count in sorted(kinds.items(), key=lambda item: -item[1])
    )
    return Row(
        (_repo_link(name, ""), _state_badge(data), str(len(data.issues)), _e(missing)),
        href=f"repos/{name}/index.html",
    )


def _state_badge(data: RepositoryData) -> str:
    value = data.state.value
    tone = {"complete": Tone.OK, "partial": Tone.NEUTRAL}.get(value, Tone.BAD)
    return ui.badge(value, tone, dot=True)


def _risk_table(site: _Site) -> str:
    risk = _table(site.report["source_risk"])
    signals = (
        ("Git sources", "git_sources"),
        ("Unpinned Actions", "unpinned_actions"),
        ("Floating image tags", "floating_images"),
        ("Missing locks", "missing_locks"),
        ("Unpinned artifacts", "unpinned_artifacts"),
        ("Vendored code", "vendored"),
    )
    return ui.table(
        "Source risk",
        (Column("Signal"), Column("Count", numeric=True)),
        [Row((_e(label), str(_value(risk.get(key))))) for label, key in signals],
    )


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


def _todo_panel(site: _Site, name: str) -> str:
    rows = _repository_todo(site, name)
    if not rows:
        return ui.panel("To do here", '<p class="zero">nothing flagged</p>', panel_id="todo", flush=True)
    return ui.panel(
        "To do here",
        ui.table("To do here", (Column("Type"), Column("Item"), Column("Detail", wrap=True)), rows, limit=_ACTION_ROWS),
        meta=str(len(rows)),
        panel_id="todo",
        flush=True,
    )


def _repository_todo(site: _Site, name: str) -> list[Row]:
    prefix = "../../../"
    rows: list[Row] = []
    for row in site.vulnerable:
        if name in row.direct_in or name in row.transitive_in:
            how = "declared" if name in row.direct_in else "locked"
            rows.append(
                Row(
                    (
                        ui.badge("Security", Tone.BAD, dot=True),
                        _package_link(row.component.purl, prefix, f"{row.component.display_name}@{row.version}"),
                        f"{how} · {_e(', '.join(row.advisories))}",
                    )
                )
            )
    behind: set[str] = set()
    for row in site.outdated:
        for use in row.uses:
            if name not in use.repositories:
                continue
            behind.add(row.component.purl)
            severity = insights.gap(row.component.ecosystem, use.version, row.latest)
            badge = ui.badge(severity.value.capitalize(), _GAP_TONES[severity]) if severity is not None else ""
            rows.append(
                Row(
                    (
                        badge,
                        _package_link(row.component.purl, prefix, row.component.display_name),
                        f'<span class="mono">{_e(use.version)} → {_e(row.latest)}</span>',
                    )
                )
            )
    for split in site.splits:
        if name in split.off_target and split.component.purl not in behind:
            version = next(use.version for use in split.uses if name in use.repositories)
            rows.append(
                Row(
                    (
                        ui.badge("Off target"),
                        _package_link(split.component.purl, prefix, split.component.display_name),
                        f'<span class="mono">{_e(_short_ref(version))} → {_e(_short_ref(split.target))}</span>',
                    )
                )
            )
    for use in site.roles:
        if name in use.overlapping or name in use.off_preferred:
            mine = [option for option, owners in use.options.items() if name in owners]
            preferred = f" · preferred {use.role.preferred}" if use.role.preferred is not None else ""
            rows.append(
                Row(
                    (
                        ui.badge("Role"),
                        f'<a href="{prefix}choices.html#role-{_e(use.role.name)}">{_e(use.role.name)}</a>',
                        f"uses {_e(', '.join(mine))}{_e(preferred)}",
                    )
                )
            )
    return rows


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
