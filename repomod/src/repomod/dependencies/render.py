from __future__ import annotations

import json
import shutil
import time
from collections import Counter, defaultdict
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Any, cast
from urllib.parse import parse_qs, quote, urlsplit

from packaging.version import InvalidVersion, Version

from repomod import dependency_roles
from repomod.dependencies import analysis, enrich, insights, triage, ui, world
from repomod.dependencies import diff as diff_module
from repomod.dependencies.graph import Graph, Inventory
from repomod.dependencies.model import (
    CoverageKind,
    Ecosystem,
    Edge,
    IndexEntry,
    Instance,
    Issue,
    RepositoryData,
    Scope,
    SourceKind,
)
from repomod.dependencies.triage import ActionKind
from repomod.dependencies.ui import Bar, Block, Cell, Column, Line, Mark, NavGroup, NavLink, Segment, Stop, Tile, Tone
from repomod.dependencies.ui import escape as _e
from repomod.rules import actions as actions_rule
from repomod.rules.python_standard import PYTHON_TOOL_NAMES
from repomod.types import StatusItem, StatusLevel

MARKER = ".repomod-dashboard"
ASSETS = Path(__file__).with_name("assets")
UI_DASHBOARD = Path(__file__).resolve().parents[3] / "node_modules" / "@laputa-systems" / "ui-dashboard" / "dist"
UI_SHEETS = ("styles.css", "tokens.css", "styles/layout.css", "styles/data.css")
_LIST_ROWS = 25
_PREVIEW_ROWS = 8
_SHOWN_REPOSITORIES = 4
_STATUS_CARDS = 12
_STATUS_TONES = {
    StatusLevel.OK: Tone.OK,
    StatusLevel.ATTENTION: Tone.WARN,
    StatusLevel.FAILING: Tone.BAD,
    StatusLevel.MUTED: Tone.NEUTRAL,
}
_CLEAN_TIER = "Clean"
_TIERS = (
    ("Vulnerable", "Security issues first"),
    ("Breaking upgrades", "No known vulnerabilities, but breaking upgrades are waiting"),
    ("Maintenance", "Routine upgrades, version drift, or role conflicts"),
)
_BLOAT_ROWS = 150
_CONSUMER_ROWS = 20
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
    now: float
    outdated: tuple[insights.Outdated, ...]
    vulnerable: tuple[insights.Vulnerable, ...]
    weights: tuple[insights.Weight, ...]
    drift: tuple[insights.Split, ...]
    roles: tuple[insights.RoleUse, ...]
    scorecards: tuple[insights.Scorecard, ...]
    forks: tuple[str, ...]
    actions: tuple[triage.Action, ...]
    status: Mapping[str, Sequence[StatusItem]] | None
    hidden: frozenset[str]
    world: world.World


def render(
    inventory: Inventory,
    graph: Graph,
    output: Path,
    *,
    root: Path | None = None,
    diff_value: diff_module.Diff | None = None,
    status: Mapping[str, Sequence[StatusItem]] | None = None,
    hidden: frozenset[str] = frozenset(),
    unmanaged: frozenset[str] = frozenset(),
) -> Dashboard:
    missing = [sheet for sheet in UI_SHEETS if not (UI_DASHBOARD / sheet).is_file()]
    if missing:
        raise FileNotFoundError(
            f"@laputa-systems/ui-dashboard is not installed at {UI_DASHBOARD}; "
            "run `pnpm install --frozen-lockfile` in the repomod checkout"
        )
    site = _site(inventory, graph, root, diff_value, status, hidden, unmanaged)
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
            ("world", "World", _world),
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
    status: Mapping[str, Sequence[StatusItem]] | None,
    hidden: frozenset[str],
    unmanaged: frozenset[str],
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
    if diff_value is None and root is not None:
        try:
            diff_value = diff_module.diff_inventory(
                root, inventory.path, from_rev="HEAD~1"
            )
        except ValueError:
            diff_value = None
    if diff_value is not None:
        visible = tuple(
            change for change in diff_value.repositories if change.repository in graph.repositories
        )
        diff_value = replace(diff_value, repositories=visible)
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
    drift = insights.drift(splits, outdated)
    roles = insights.role_uses(dependency_roles.load_roles(), mine)
    return _Site(
        graph,
        inventory,
        inventory.entries,
        cache,
        cast(Mapping[str, Any], report_value),
        diff_value,
        tuple(index),
        time.time(),
        outdated,
        vulnerable,
        weights,
        drift,
        roles,
        insights.scorecards(
            [name for name in graph.repositories if name not in forks],
            outdated,
            vulnerable,
            drift,
            roles,
            weights,
        ),
        forks,
        _triage(graph, vulnerable, outdated, forks),
        status,
        hidden,
        world.build(graph, [name for name in graph.repositories if name not in forks], cache.sources),
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
            ),
            heading=False,
        ),
        NavGroup("Overview", (NavLink("world", "World"),)),
        NavGroup(
            "Review",
            (
                NavLink("security", "Security", len(_of_kind(site, ActionKind.SECURITY))),
                NavLink("outdated", "Upgrades", len(site.outdated)),
                NavLink("alignment", "Alignment", len(site.drift)),
                NavLink("choices", "Choices", len(_contested_roles(site))),
                NavLink("bloat", "Bloat"),
            ),
        ),
        NavGroup(
            "Inventory",
            (NavLink("coverage", "Coverage"), NavLink("changes", "Changes"), NavLink("packages", "Packages")),
        ),
    )


def _write_page(path: Path, content: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


def _write_assets(output: Path, site: _Site) -> None:
    (output / "assets" / "ui-dashboard").mkdir(parents=True)
    for name in ("app.css", "app.js"):
        shutil.copyfile(ASSETS / name, output / "assets" / name)
    for sheet in UI_SHEETS:
        shutil.copyfile(UI_DASHBOARD / sheet, output / "assets" / "ui-dashboard" / Path(sheet).name)
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
        Tile(str(len(site.drift)), "Version drift", "packages upgrades will not align", "alignment.html"),
        Tile(str(len(contested)), "Role conflicts", f"{off_preferred} repositories off preferred", "choices.html"),
    )


def _fork_exposure(site: _Site) -> dict[str, int]:
    exposure = dict.fromkeys(site.forks, 0)
    for row in site.vulnerable:
        for repository in {*row.direct_in, *row.transitive_in}:
            if repository in exposure:
                exposure[repository] += 1
    return exposure


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
        label="Versions",
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
            label="Used by",
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


def _change_group(title: str, items: Sequence[str]) -> str:
    if not items:
        return ""
    rendered = "".join(f"<li>{item}</li>" for item in items)
    return f'<div class="change-group">{title}</div><ul class="plain-list">{rendered}</ul>'


_KNOWN_LIMITS = {
    CoverageKind.UNSUPPORTED_FORMAT: "files in formats with no parser",
    CoverageKind.DYNAMIC_VALUE: "computed versions",
    CoverageKind.VENDORED_UNKNOWN: "vendored trees with no manifest",
    CoverageKind.SUBMODULE: "submodules not inspected",
}
_CHECKOUT_KINDS = frozenset(
    {
        CoverageKind.CHECKOUT_MISSING,
        CoverageKind.ORIGIN_MISMATCH,
        CoverageKind.DEFAULT_REF_MISSING,
        CoverageKind.GIT_FAILURE,
    }
)
_COVERAGE_ACTIONS = {
    CoverageKind.CHECKOUT_MISSING: ("No checkout", "Run `repomod sync`, or fix the path in the catalog."),
    CoverageKind.ORIGIN_MISMATCH: ("Wrong remote", "Fix the remote or the catalog entry, then re-sync."),
    CoverageKind.DEFAULT_REF_MISSING: ("Default branch missing", "Fetch the default branch and set `origin/HEAD`."),
    CoverageKind.GIT_FAILURE: ("Git read failed", "Run the git command in that checkout to see the raw error."),
    CoverageKind.PARSE_ERROR: ("File does not parse", "Fix the syntax. The repository page links the file."),
    CoverageKind.LOCK_MISMATCH: ("Manifest and lock disagree", "Regenerate the lockfile and commit it."),
    CoverageKind.MISSING_LOCK: ("No lockfile", "Generate the lockfile for the ecosystem in use and commit it."),
    CoverageKind.SYMLINK_SKIPPED: ("Symlinked file skipped", "Replace the symlink with the real file."),
    CoverageKind.NON_UTF8: ("Not UTF-8 text", "Convert the file to UTF-8."),
}


def _state_badge(data: RepositoryData) -> str:
    value = data.state.value
    tone = {"complete": Tone.OK, "partial": Tone.NEUTRAL}.get(value, Tone.BAD)
    return ui.badge(value, tone, dot=True)


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
    if exposed:
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
    rest = ", ".join(
        f'<a href="{prefix}repos/{_e(name)}/index.html">{_e(_short_repository(name))}</a>' for name in hidden
    )
    return f"{shown} <details><summary>+{len(hidden)} more</summary>{rest}</details>"


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
        '<div class="ui-panel"><div class="ui-panel-body">'
        + ui.scope(ui.bar_list(bars, scale, "Breaking upgrades"), limit=_PREVIEW_ROWS, total=len(bars))
        + "</div></div>",
        count=len(actions),
        note="Breaking upgrades by repositories affected. These usually need a manifest edit.",
    )


def _tier(card: insights.Scorecard) -> str:
    if card.vulnerable:
        return "Vulnerable"
    if card.breaking:
        return "Breaking upgrades"
    if any(card.rank):
        return "Maintenance"
    return _CLEAN_TIER


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
                f'<div class="ui-panel"><div class="ui-panel-body">{listing}</div></div>',
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
    header = ui.page_header("Alignment", "Version drift that upgrading will not fix. Upgrades covers the rest.")
    if not site.drift:
        return header + ui.panel(
            "Version drift",
            '<p class="zero">no drift is left once pending upgrades are applied</p>',
            flush=True,
        )
    groups: dict[Ecosystem, list[insights.Split]] = defaultdict(list)
    for row in site.drift:
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


def _lane_label(version: str) -> str:
    if "@sha256:" in version:
        tag, digest = version.split("@sha256:", 1)
        return f"{tag}@{digest[:7]}"
    if len(version) == 40 and all(character in "0123456789abcdef" for character in version):
        return version[:7]
    return version


def _bloat(site: _Site) -> str:
    rows = [row for row in site.weights if row.sole > 0][:_BLOAT_ROWS]
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
        tail=f'<details class="ui-disclosure"><summary class="ui-disclosure-summary">Packages</summary>'
        f'<div class="ui-disclosure-body"><ul class="plain-list">{items}</ul></div></details>',
    )


def _risk_bars(counts: Sequence[tuple[str, int]]) -> str:
    bars = [
        Bar(_e(label), (Segment("Count", value, Mark.GHOST),))
        for label, value in sorted(counts, key=lambda item: -item[1])
    ]
    return ui.bar_list(bars, max((value for _label, value in counts), default=0), "Source risk")


_RISK_CATEGORIES = (
    (
        "git_sources",
        "Git sources",
        "A git ref can move or disappear; a registry is stable.",
        "git",
        Tone.NEUTRAL,
    ),
    (
        "unpinned_actions",
        "Unpinned Actions",
        "A moving tag can change what CI runs.",
        "unpinned",
        Tone.WARN,
    ),
    (
        "floating_images",
        "Floating image tags",
        "A tag can point at a new image.",
        "floating",
        Tone.WARN,
    ),
    (
        "missing_locks",
        "Missing locks",
        "Dependencies can resolve differently over time.",
        "no lockfile",
        Tone.WARN,
    ),
    (
        "unpinned_artifacts",
        "Unpinned artifacts",
        "A download can change without a version or checksum.",
        "unpinned",
        Tone.WARN,
    ),
    (
        "vendored",
        "Vendored code",
        "A vendored tree has no manifest to track its versions.",
        "vendored",
        Tone.NEUTRAL,
    ),
)


def _risk_section(site: _Site) -> str:
    details = _table(_table(site.report["source_risk"]).get("details"))
    cards: list[str] = []
    counts: list[tuple[str, int]] = []
    for key, title, note, badge, tone in _RISK_CATEGORIES:
        items = details.get(key)
        if not isinstance(items, (tuple, list)) or not items:
            continue
        kept = _without_forks(site, key, items)
        if not kept:
            continue
        lines = _risk_lines(site, key, kept, badge, tone)
        if not lines:
            continue
        counts.append((title, len(lines)))
        cards.append(_risk_card(title, note, lines))
    if not cards:
        return ui.panel("Source risk", ui.empty_state("No source risk"), flush=True)
    return ui.group(
        "Source risk",
        _risk_bars(counts) + ui.card_grid(cards, wide=True),
        count=sum(value for _label, value in counts),
        note="Every source that can change without a new commit.",
    )


def _without_forks(site: _Site, key: str, items: Sequence[object]) -> list[object]:
    if not site.forks:
        return list(items)
    if key == "missing_locks":
        return [
            item
            for item in items
            if not (isinstance(item, Mapping) and item.get("repository") in site.forks)
        ]
    return [
        item
        for item in items
        if isinstance(item, str)
        and any(
            repository not in site.forks
            for repository in site.graph.instance_repositories.get(item, ())
        )
    ]


def _risk_card(title: str, note: str, lines: Sequence[Line]) -> str:
    body = ui.lines(lines)
    if len(lines) > _LIST_ROWS:
        body = ui.scope(body, limit=_LIST_ROWS, total=len(lines))
    return ui.card(
        f"<span>{_e(title)}</span>",
        f'<div class="card-change">{_e(note)}</div>{body}',
        meta=ui.badge(_plural(len(lines), "item"), Tone.WARN),
    )


def _risk_lines(site: _Site, key: str, items: Sequence[object], badge: str, tone: Tone) -> list[Line]:
    if key == "missing_locks":
        return _missing_lock_lines(items, badge, tone)
    return _instance_risk_lines(site, items, badge, tone)


def _instance_risk_lines(
    site: _Site, items: Sequence[object], badge: str, tone: Tone
) -> list[Line]:
    instances = [
        instance
        for item in items
        if isinstance(item, str) and (instance := site.graph.instances.get(item)) is not None
    ]
    instances.sort(key=lambda instance: (instance.purl.casefold(), instance.version or ""))
    return [_instance_risk_line(site, instance, badge, tone) for instance in instances]


def _instance_risk_line(site: _Site, instance: Instance, badge: str, tone: Tone) -> Line:
    component = site.graph.components.get(instance.component)
    name = component.display_name if component is not None else instance.component
    version = f"@{instance.version}" if instance.version is not None else ""
    repositories = sorted(
        (
            name
            for name in site.graph.instance_repositories.get(instance.id, ())
            if name not in site.forks
        ),
        key=str.casefold,
    )
    source = _risk_source(instance)
    detail = source + (" · " + _repository_names(repositories) if repositories else "")
    return Line(
        _package_link(instance.purl, "", f"{name}{version}"),
        detail,
        ui.badge(badge, tone, dot=True),
        source,
    )


def _missing_lock_lines(items: Sequence[object], badge: str, tone: Tone) -> list[Line]:
    locks: list[tuple[str, Mapping[str, object]]] = []
    for item in items:
        if isinstance(item, Mapping):
            name = item.get("repository")
            if isinstance(name, str):
                locks.append((name, cast(Mapping[str, object], item)))
    locks.sort(key=lambda entry: (entry[0].casefold(), str(entry[1].get("path") or "")))
    lines: list[Line] = []
    for name, item in locks:
        path = item.get("path")
        message = item.get("message")
        detail = _e(path) if isinstance(path, str) and path else _e(str(message or ""))
        lines.append(
            Line(
                f'<a href="repos/{_e(name)}/index.html">{_e(_short_repository(name))}</a>',
                detail,
                ui.badge(badge, tone, dot=True),
                name,
            )
        )
    return lines


def _risk_source(instance: Instance) -> str:
    if instance.source_kind is SourceKind.GIT:
        if instance.commit is not None:
            return f"commit {_lane_label(instance.commit)}"
        if instance.vcs_url is not None:
            return instance.vcs_url
        return "git source"
    if instance.source_kind is SourceKind.VENDORED:
        return "vendored tree"
    if instance.source_kind is SourceKind.URL:
        url = _download_url(instance.purl)
        return url if url is not None else "download"
    if instance.source_kind is SourceKind.IMAGE:
        return f"tag {instance.version or 'latest'}"
    if instance.version is not None:
        return f"ref {instance.version}"
    return instance.source_kind.value


def _download_url(purl: str) -> str | None:
    values = parse_qs(urlsplit(purl).query).get("download_url")
    return values[0] if values is not None else None


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
    data = site.graph.repositories[name]
    entry = site.entries.get(name)
    commit = entry.commit if entry is not None else None
    coverage = [
        Line(
            _evidence_path(name, commit, issue.path) or _e(_COVERAGE_ACTIONS[issue.kind][0]),
            f"{_e(_COVERAGE_ACTIONS[issue.kind][0])} · {_e(issue.message)}",
        )
        for issue in sorted(_actionable_issues(site, name, data), key=lambda item: (item.path or "", item.kind.value))
    ]
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
        for split in site.drift
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
            ("Coverage", Tone.WARN, coverage),
        )
        if lines
    ]


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
    exposure = _fork_exposure(site)
    exposed = sorted(
        (name for name in site.forks if exposure[name]), key=lambda item: (-exposure[item], item.casefold())
    )
    if exposed:
        groups.append(
            ui.group(
                "Forks with vulnerable versions",
                ui.chip_links(_repository_chip(name, exposure[name]) for name in exposed),
                count=len(exposed),
            )
        )
    return "\n".join(
        (
            ui.page_header(
                "Repositories",
                f"{_owned_count(site)} repositories · {len(tiers[_CLEAN_TIER])} clean · "
                f"{len(site.forks)} forks · {_enrichment_stamp(site)}"
                + (f" · {len(site.hidden)} private hidden" if site.hidden else ""),
            ),
            _status_section(site),
            ui.legend(_WORK_LEGEND),
            ui.scope("".join(groups), search="Filter repositories"),
        )
    )


def _status_section(site: _Site) -> str:
    if site.status is None:
        return ui.notice(
            'CI, releases, upstreams, and checkouts are not collected. Run '
            '<span class="mono">make deps</span> to include them.'
        )
    flagged = sorted(((name, items) for name, items in site.status.items() if items), key=_status_rank)
    if not flagged:
        return ui.panel("Status", ui.empty_state("Nothing flagged"), flush=True)
    cards = [
        ui.card(
            f'<a href="repos/{_e(name)}/index.html">{_e(_short_repository(name))}</a>',
            _status_lines(items),
        )
        for name, items in flagged
    ]
    return ui.scope(
        ui.group(
            "Status",
            ui.card_grid(cards),
            count=len(cards),
            note="CI, releases, upstreams, pull request conflicts, and local checkouts",
            group_id="status",
        ),
        limit=_STATUS_CARDS,
        total=len(cards),
    )


def _status_rank(item: tuple[str, Sequence[StatusItem]]) -> tuple[int, int, str]:
    name, items = item
    levels = Counter(entry.level for entry in items)
    return (-levels[StatusLevel.FAILING], -levels[StatusLevel.ATTENTION], name.casefold())


def _status_lines(items: Sequence[StatusItem]) -> str:
    return ui.lines(
        [
            Line(
                ui.badge(item.area.value, _STATUS_TONES[item.level], dot=True, running=item.running),
                _status_label(item),
            )
            for item in items
        ]
    )


def _status_label(item: StatusItem) -> str:
    if item.url is None or not item.url.startswith("https://"):
        return _e(item.label)
    return f'<a href="{_e(item.url)}">{_e(item.label)}</a>'


def _repository_status_panel(site: _Site, name: str) -> str:
    if site.status is None:
        return ""
    items = site.status.get(name, ())
    return ui.panel(
        "Status",
        _status_lines(items) if items else '<p class="zero">nothing flagged</p>',
        panel_id="status",
    )


def _repository_card(card: insights.Scorecard, scale: int) -> str:
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
        (ui.segments(_work_parts(card), scale) if _work_total(card) else "")
        + (f'<div class="card-foot">{_e(summary)}</div>' if summary else "")
        + (f'<div class="card-foot quiet">{_e(drift)}</div>' if drift else ""),
        href=f"repos/{card.repository}/index.html",
    )


def _forks_panel(site: _Site) -> str:
    exposure: dict[str, list[str]] = {name: [] for name in site.forks}
    for row in site.vulnerable:
        for repository in {*row.direct_in, *row.transitive_in}:
            if repository in exposure:
                exposure[repository].append(f"{row.component.display_name}@{row.version}")
    exposed = sorted(
        (name for name in site.forks if exposure[name]), key=lambda item: (-len(exposure[item]), item.casefold())
    )
    if not exposed:
        return ""
    chips = ui.chip_links(
        ui.chip_link(
            _short_repository(name),
            f"repos/{name}/index.html",
            len(exposure[name]),
            tip=", ".join(sorted(exposure[name])),
        )
        for name in exposed
    )
    return ui.collapsed_panel(
        "Forks",
        chips,
        meta=f"{len(exposed)} of {len(site.forks)} carry vulnerable versions",
        panel_id="forks",
    )


def _lane_card(row: insights.Split) -> str:
    in_use = {use.version for use in row.uses}
    ordered = tuple(reversed(row.uses))
    if row.target not in in_use:
        ordered += (insights.VersionUse(row.target, ()),)
    stops = [
        Stop(
            _lane_label(use.version),
            len(use.repositories),
            use.version == row.target,
            f"{_short_ref(use.version)}: target"
            if use.version == row.target
            else f"{_short_ref(use.version)}: {len(use.repositories)} off target",
        )
        for use in ordered
    ]
    off_target = sorted(set(row.off_target), key=str.casefold)
    basis = {
        insights.TargetBasis.APPROVED_PIN: "Approved pin",
        insights.TargetBasis.LATEST: "Latest release",
        insights.TargetBasis.NEWEST_IN_USE: "Newest in use · latest release not confirmed",
        insights.TargetBasis.MOST_COMMON: "Most common ref · latest release not confirmed",
    }[row.basis]
    return ui.card(
        _package_link(row.component.purl, "", row.component.display_name),
        ui.lane(stops) + f'<div class="card-foot">{basis}<br>Move {_repository_names(off_target)}</div>',
        meta=ui.note(f"{len(off_target)} off target"),
    )


def _choices(site: _Site) -> str:
    contested = _contested_roles(site)
    in_use = sum(1 for use in site.roles if use.in_use)
    body = [
        ui.page_header(
            "Choices",
            f"{len(contested)} roles need a change · {in_use - len(contested)} settled · forks left out",
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
    else:
        body.append(ui.panel("Choices", ui.empty_state("Nothing to change"), flush=True))
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
    preferred = use.role.preferred
    heading = f"Move to {preferred}" if preferred is not None else "Pick one option"
    movers = "".join(
        f'<div class="version-use">{_repo_link(repository, "")} '
        f"{ui.note(_e('uses ' + ', '.join(_other_options(use, repository))))}</div>"
        for repository in sorted({*use.off_preferred, *use.overlapping}, key=str.casefold)
    )
    return ui.card(
        f"<span>{_e(use.role.name)}</span>",
        ui.bar_list(bars, scale, f"{use.role.name} adoption")
        + f'<div class="card-label">{_e(heading)}</div>{movers}',
        meta=ui.badge(" · ".join(pressure), Tone.WARN),
        card_id=f"role-{use.role.name}",
    )


def _other_options(use: insights.RoleUse, repository: str) -> list[str]:
    return [
        option
        for option, repositories in use.options.items()
        if repository in repositories and option != use.role.preferred
    ] or ["none of the options"]


def _role_bar(use: insights.RoleUse, option: str) -> Bar:
    repositories = use.options[option]
    packages = (
        []
        if option == use.role.preferred
        else sorted(
            {
                package
                for repository in repositories
                for package in use.packages.get((repository, option), ())
                if package != option
            }
        )
    )
    more = f" +{len(packages) - 2}" if len(packages) > 2 else ""
    note = _e(f"{', '.join(packages[:2])}{more}") if packages else ""
    mark = Mark.OK if option == use.role.preferred else Mark.BAD if option in use.role.bans else Mark.WARN
    return Bar(_e(option), (Segment("Repositories", len(repositories), mark),), note=note)


def _packages(site: _Site) -> str:
    ecosystems = Counter(component.ecosystem for component in site.graph.components.values())
    bars = [
        Bar(_e(ecosystem.value), (Segment("Packages", count, Mark.ACCENT),))
        for ecosystem, count in ecosystems.most_common()
    ]
    mix = ui.panel(
        "Packages by ecosystem",
        ui.bar_list(bars, max(ecosystems.values(), default=0), "Packages by ecosystem"),
    )
    header = ui.page_header(
        "Packages",
        f"{len(site.graph.components)} packages in {len(ecosystems)} ecosystems. "
        'Press <span class="mono">/</span> to find one.',
    )
    sections = "".join(
        _package_section(site, purl) for purl in sorted(site.graph.components, key=str.casefold)
    )
    return f'<div id="package-index" class="ui-page-section">{header}{mix}</div>{sections}'


def _changes(site: _Site) -> str:
    value = site.diff_value
    if value is None or not value.baseline:
        return ui.page_header("Changes") + (
            '<p class="unknown">no baseline: the previous revision has no dependency inventory</p>'
        )
    body = [ui.page_header("Changes", _summary_metrics(value))]
    flagged = False
    for change in value.repositories:
        groups = "".join(
            (
                _change_group(
                    "Downgrades and other version changes",
                    [
                        f'<span class="mono">{_e(item.name)}</span>: {_e(item.old or "—")} → {_e(item.new or "—")}'
                        for item in change.versions
                        if item.direction != "upgrade"
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
                    "New issues",
                    [
                        f'<span class="finding">{_e(item.kind.value)} {_e(item.path or "")}</span>'
                        for item in change.new_issues
                    ],
                ),
            )
        )
        if groups:
            flagged = True
            body.append(ui.panel(change.repository, groups))
    if not flagged:
        body.append(ui.panel("Review", ui.empty_state("Nothing to review"), flush=True))
    return "\n".join(body)


def _actionable_issues(site: _Site, name: str, data: RepositoryData) -> list[Issue]:
    return [
        issue
        for issue in data.issues
        if issue.kind not in _KNOWN_LIMITS and (name not in site.forks or issue.kind in _CHECKOUT_KINDS)
    ]


def _coverage(site: _Site) -> str:
    states: Counter[str] = Counter(data.state.value for data in site.graph.repositories.values())
    kinds: Counter[CoverageKind] = Counter(
        issue.kind for data in site.graph.repositories.values() for issue in data.issues
    )
    limits = " · ".join(f"{kinds[kind]} {label}" for kind, label in _KNOWN_LIMITS.items() if kinds[kind])
    subtitle = (
        f"{len(site.graph.repositories)} repositories · {states['complete']} complete · "
        f"{states['partial']} partial · {states['failed']} failed · {states['unavailable']} unavailable"
    )
    body = [ui.page_header("Coverage", f"{subtitle}<br>Known limits: {limits}" if limits else subtitle)]
    affected: dict[CoverageKind, set[str]] = defaultdict(set)
    for name, data in site.graph.repositories.items():
        for issue in _actionable_issues(site, name, data):
            affected[issue.kind].add(name)
    if affected:
        body.append(
            ui.group(
                "What to fix",
                ui.card_grid(
                    (
                        _coverage_card(kind, sorted(names, key=str.casefold))
                        for kind, names in sorted(affected.items(), key=lambda item: (item[0].severity, -len(item[1])))
                    ),
                    wide=True,
                ),
                count=len(affected),
            )
        )
    else:
        body.append(ui.panel("What to fix", ui.empty_state("Nothing to fix"), flush=True))
    body.append(_risk_section(site))
    return "\n".join(body)


def _coverage_card(kind: CoverageKind, repositories: Sequence[str]) -> str:
    label, action = _COVERAGE_ACTIONS[kind]
    tone = Tone.BAD if kind.severity == "error" else Tone.WARN
    return ui.card(
        f"<span>{_e(label)}</span>",
        f'<div class="card-change">{_e(action)}</div>'
        f'<div class="card-foot">{_repository_names(repositories)}</div>',
        meta=ui.badge(_plural(len(repositories), "repository", "repositories"), tone),
    )


def _repository(site: _Site, name: str) -> str:
    data = site.graph.repositories[name]
    card = next((item for item in site.scorecards if item.repository == name), None)
    fork = "fork, tracked only" if name in site.forks else ""
    body = [
        ui.breadcrumbs(
            (("Repositories", "../../../repositories.html"), ("To do", "../../../index.html")),
            name,
        ),
        ui.page_header(name, fork),
    ]
    if data.state.value in ("failed", "unavailable"):
        body.append(f'<div class="badges">{_state_badge(data)}</div>')
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
    body.append(_repository_status_panel(site, name))
    body.append(_todo_panel(site, name))
    if not data.edges:
        body.append(
            '<p class="zero">complete: zero dependencies</p>'
            if data.state.value == "complete"
            else '<p class="unknown">no dependency edges recorded</p>'
        )
    return "\n".join(body)


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
    if entry is None or not isinstance(entry.get("vulns"), list):
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


_ORIGIN_MARKS = {
    world.Origin.YOURS: Mark.ACCENT,
    world.Origin.THIRD_PARTY: Mark.GHOST,
    world.Origin.UNKNOWN: Mark.UNKNOWN,
}
_ORIGIN_LABELS = {
    world.Origin.YOURS: "Yours",
    world.Origin.THIRD_PARTY: "Third-party",
    world.Origin.UNKNOWN: "Owner unknown",
}
_REACH_BUCKETS = ((1, 1), (2, 2), (3, 5), (6, 10), (11, 20), (21, None))
_TREEMAP_SIZE = (1200, 760)


def _world(site: _Site) -> str:
    value = site.world
    packages = value.packages
    forks = len(site.forks)
    header = ui.page_header(
        "World",
        f"Every package your {len(value.repositories)} repositories reach, by who maintains it. "
        + (f"{forks} forks are left out. " if forks else "")
        + f"Yours means {', '.join(sorted(world.display_owner(owner) for owner in value.owners))}.",
    )
    if not packages:
        return header + ui.empty_state("No packages", "No repository in this view declares a dependency.")
    origins = Counter(package.origin for package in packages)
    direct = sum(1 for package in packages if package.direct)
    in_repo = sum(1 for package in packages if package.in_repo)
    third_owners = {package.owner for package in packages if package.origin is world.Origin.THIRD_PARTY}
    tiles = (
        Tile(
            str(len(packages)),
            "Packages",
            f"{direct} declared directly · {len(packages) - direct} transitive only",
            "#map",
        ),
        Tile(
            str(origins[world.Origin.YOURS]),
            "Yours",
            f"{_percent(origins[world.Origin.YOURS], len(packages))} · {in_repo} built in your repositories",
            "#yours",
        ),
        Tile(
            str(origins[world.Origin.THIRD_PARTY]),
            "Third-party",
            f"{_percent(origins[world.Origin.THIRD_PARTY], len(packages))} · {len(third_owners)} owners",
            "#owners",
        ),
        Tile(
            str(origins[world.Origin.UNKNOWN]),
            "Owner unknown",
            f"{_percent(origins[world.Origin.UNKNOWN], len(packages))} · no source URL"
            if site.cache.sources
            else "run deps enrich to find owners",
            "#owners",
        ),
    )
    body = [header, ui.status_strip(tiles)]
    if not site.cache.sources:
        body.append(
            ui.notice(
                'Owners of registry packages are not looked up yet. Run '
                '<span class="mono">uv run repomod deps enrich</span>, then render again.'
            )
        )
    body.extend(
        (
            _ownership_panel(packages),
            _world_map(site),
            _yours_panel(packages),
            _reach_panel(value),
            _owners_panel(value),
        )
    )
    return "\n".join(body)


def _percent(part: int, total: int) -> str:
    return f"{round(100 * part / total)}%" if total else "0%"


def _origin_parts(packages: Sequence[world.Package], weight: Callable[[world.Package], int]) -> tuple[Segment, ...]:
    totals = Counter[world.Origin]()
    for package in packages:
        totals[package.origin] += weight(package)
    return tuple(Segment(_ORIGIN_LABELS[origin], totals[origin], _ORIGIN_MARKS[origin]) for origin in world.Origin)


def _origin_legend(caption: str = "") -> str:
    return ui.legend([(_ORIGIN_LABELS[origin], _ORIGIN_MARKS[origin]) for origin in world.Origin], caption)


def _ownership_panel(packages: Sequence[world.Package]) -> str:
    def uses(package: world.Package) -> int:
        return len(package.repositories)

    def one(_: world.Package) -> int:
        return 1

    totals = [
        Bar("Packages", _origin_parts(packages, one), note="each package counted once"),
        Bar(
            "Package uses",
            _origin_parts(packages, uses),
            note="each package counted once per repository that reaches it",
        ),
    ]
    by_ecosystem: dict[Ecosystem, list[world.Package]] = defaultdict(list)
    for package in packages:
        by_ecosystem[package.component.ecosystem].append(package)
    ecosystems = [
        Bar(_e(ecosystem.value), _origin_parts(rows, one))
        for ecosystem, rows in sorted(by_ecosystem.items(), key=lambda item: (-len(item[1]), item[0].value))
    ]
    body = (
        _origin_legend()
        + ui.bar_list(totals, max(sum(part.value for part in bar.parts) for bar in totals), "Ownership")
        + '<p class="change-group">Packages by ecosystem</p>'
        + ui.bar_list(ecosystems, max(len(rows) for rows in by_ecosystem.values()), "Ownership by ecosystem")
    )
    return ui.panel("How much is yours", body)


def _world_map(site: _Site) -> str:
    vulnerable = {row.component.purl for row in site.vulnerable}
    by_ecosystem: dict[Ecosystem, dict[str | None, list[world.Package]]] = defaultdict(lambda: defaultdict(list))
    for package in site.world.packages:
        by_ecosystem[package.component.ecosystem][package.owner].append(package)
    blocks = []
    for ecosystem, owners in by_ecosystem.items():
        count = sum(len(rows) for rows in owners.values())
        children = []
        for owner, rows in owners.items():
            name = world.display_owner(owner)
            cells = [
                Cell(
                    package.component.display_name,
                    len(package.repositories),
                    f"packages.html#pkg={package.component.purl}",
                    f"{package.component.display_name} · {name} · {len(package.repositories)} "
                    f"{'repository' if len(package.repositories) == 1 else 'repositories'} · "
                    f"{'direct' if package.direct else 'transitive'}"
                    + (" · vulnerable" if package.component.purl in vulnerable else ""),
                    " ".join(
                        (
                            f"tm-{package.origin.name.casefold().replace('_', '-')}",
                            "tm-direct" if package.direct else "tm-transitive",
                            *(("tm-vulnerable",) if package.component.purl in vulnerable else ()),
                        )
                    ),
                )
                for package in rows
            ]
            children.append(Block(name, f"{name} · {len(rows)} packages", cells))
        blocks.append(Block(f"{ecosystem.value} · {count}", f"{ecosystem.value} · {count} packages", children))
    width, height = _TREEMAP_SIZE
    legend = ui.legend(
        [
            ("Yours, direct", Mark.ACCENT),
            ("Third-party, direct", Mark.GHOST),
            ("Owner unknown", Mark.UNKNOWN),
            ("Vulnerable", Mark.BAD),
        ],
        "Area is repositories reached. Lighter cells are transitive only. Select a cell to open the package.",
    )
    chart = ui.treemap(blocks, width, height, f"{len(site.world.packages)} packages by ecosystem and owner")
    return ui.panel("Map", legend + f'<div class="treemap-frame">{chart}</div>', panel_id="map")


def _yours_panel(packages: Sequence[world.Package]) -> str:
    reused = [package for package in packages if package.origin is world.Origin.YOURS and not package.in_repo]
    built = sum(1 for package in packages if package.in_repo)
    meta = f"{built} more are built inside your repositories"
    if not reused:
        empty = '<p class="zero">no repository consumes a package you publish</p>'
        return ui.panel("Your packages", empty, meta=meta, panel_id="yours")
    bars = [
        Bar(
            f'<a href="packages.html#pkg={_e(package.component.purl)}">{_e(package.component.display_name)}</a>',
            (Segment("Repositories", len(package.repositories), Mark.ACCENT),),
            note=_e(f"{package.component.ecosystem.value} · {world.display_owner(package.owner)}"),
            tip=", ".join(package.repositories),
        )
        for package in reused
    ]
    listing = ui.scope(
        ui.bar_list(bars, max(len(package.repositories) for package in reused), "Your packages"),
        search="Filter your packages",
        limit=_LIST_ROWS,
        total=len(bars),
    )
    return ui.panel(f"Your packages your repositories use · {len(reused)}", listing, meta=meta, panel_id="yours")


def _reach_panel(value: world.World) -> str:
    bars = []
    for low, high in _REACH_BUCKETS:
        if low > len(value.repositories):
            break
        top = len(value.repositories) if high is None else min(high, len(value.repositories))
        rows = [package for package in value.packages if low <= len(package.repositories) <= top]
        label = str(low) if low == top else f"{low} to {top}"
        bars.append(Bar(f"{label} {'repository' if top == 1 else 'repositories'}", _origin_parts(rows, lambda _: 1)))
    scale = max((sum(part.value for part in bar.parts) for bar in bars), default=0)
    return ui.panel(
        "Reach",
        _origin_legend("Packages by how many of your repositories reach them")
        + ui.bar_list(bars, scale, "Packages by reach"),
    )


def _owners_panel(value: world.World) -> str:
    packages = value.packages
    grouped: dict[str, list[world.Package]] = defaultdict(list)
    for package in packages:
        if package.owner is not None:
            grouped[package.owner].append(package)
    rows = sorted(grouped.items(), key=lambda item: (-len(item[1]), item[0]))
    bars = []
    for owner, owned in rows:
        reached = {repository for package in owned for repository in package.repositories}
        direct = sum(1 for package in owned if package.direct)
        ecosystems = sorted({package.component.ecosystem.value for package in owned})
        yours = owner in value.owners
        bars.append(
            Bar(
                _e(world.display_owner(owner)) + (" " + ui.badge("yours") if yours else ""),
                (
                    Segment("Direct", direct, Mark.ACCENT),
                    Segment("Transitive only", len(owned) - direct, Mark.GHOST),
                ),
                note=_e(f"{len(reached)} repositories · {', '.join(ecosystems)}"),
            )
        )
    unknown = sum(1 for package in packages if package.owner is None)
    listing = ui.scope(
        ui.bar_list(bars, max((len(owned) for _, owned in rows), default=0), "Owners"),
        search="Filter owners",
        limit=_LIST_ROWS,
        total=len(bars),
    )
    return ui.panel(
        f"Owners · {len(rows)}",
        ui.legend([("Direct", Mark.ACCENT), ("Transitive only", Mark.GHOST)], "Packages per owner") + listing,
        meta=f"{unknown} packages have no known owner",
        panel_id="owners",
    )
