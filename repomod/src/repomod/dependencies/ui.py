from __future__ import annotations

import html
from collections.abc import Iterable, Sequence
from dataclasses import dataclass
from enum import Enum
from math import sqrt

_SEARCH_ICON = (
    '<svg class="ui-icon" width="13" height="13" viewBox="0 0 24 24" fill="none" '
    'stroke="currentColor" stroke-width="1.6" stroke-linecap="round" aria-hidden="true">'
    '<path d="M10.5 4a6.5 6.5 0 1 0 0 13 6.5 6.5 0 0 0 0-13Z"/><path d="m15.2 15.2 4.3 4.3"/></svg>'
)


class Tone(Enum):
    OK = "ok"
    WARN = "warn"
    BAD = "bad"
    NEUTRAL = "neutral"


class Mark(Enum):
    ACCENT = "accent"
    GHOST = "ghost"
    OK = "ok"
    WARN = "warn"
    BAD = "bad"


@dataclass(frozen=True, slots=True)
class Segment:
    label: str
    value: int
    mark: Mark


@dataclass(frozen=True, slots=True)
class Bar:
    label: str
    parts: Sequence[Segment]
    note: str = ""
    tip: str = ""
    tail: str = ""


@dataclass(frozen=True, slots=True)
class Stop:
    label: str
    repositories: int
    target: bool
    tip: str


@dataclass(frozen=True, slots=True)
class Line:
    primary: str
    detail: str
    badge: str = ""
    tip: str = ""


@dataclass(frozen=True, slots=True)
class Tile:
    value: str
    label: str
    note: str
    href: str
    tone: Tone | None = None


@dataclass(frozen=True, slots=True)
class Column:
    label: str
    numeric: bool = False
    wrap: bool = False
    mono: bool = False


@dataclass(frozen=True, slots=True)
class NavLink:
    page: str
    label: str
    count: int | None = None


@dataclass(frozen=True, slots=True)
class NavGroup:
    label: str
    links: Sequence[NavLink]
    heading: bool = True


def escape(value: object) -> str:
    return html.escape(str(value), quote=True)


def badge(label: str, tone: Tone = Tone.NEUTRAL, *, dot: bool = False, running: bool = False) -> str:
    dot_class = "ui-badge-dot dot-running" if running else "ui-badge-dot"
    mark = f'<span class="{dot_class}" aria-hidden="true"></span>' if dot else ""
    return f'<span class="ui-badge ui-badge-{tone.value}">{mark}{escape(label)}</span>'


def note(content: str) -> str:
    return f'<span class="cell-note">{content}</span>'


def page_header(title: str, subtitle: str = "") -> str:
    below = f'<p class="page-sub">{subtitle}</p>' if subtitle else ""
    return f'<header class="ui-page-header"><h1>{escape(title)}</h1></header>{below}'


def status_strip(tiles: Sequence[Tile]) -> str:
    cells = "".join(
        f'<a class="tile{f" tile-{tile.tone.value}" if tile.tone is not None else ""}" '
        f'href="{escape(tile.href)}">'
        f'<span class="tile-label">{escape(tile.label)}</span>'
        f'<span class="tile-value">{escape(tile.value)}</span>'
        f'<span class="tile-note">{escape(tile.note)}</span></a>'
        for tile in tiles
    )
    return f'<div class="status-strip" style="--cells:{len(tiles)}">{cells}</div>'


def panel(
    title: str, body: str, *, meta: str = "", panel_id: str | None = None, flush: bool = False
) -> str:
    identity = f' id="{escape(panel_id)}"' if panel_id is not None else ""
    return (
        f'<section class="ui-panel"{identity}>{_panel_head(title, meta)}'
        f'<div class="{_body_class(flush)}">{body}</div></section>'
    )


def collapsed_panel(
    title: str, body: str, *, meta: str = "", panel_id: str | None = None, flush: bool = False
) -> str:
    identity = f' id="{escape(panel_id)}"' if panel_id is not None else ""
    return (
        f'<details class="ui-panel collapsed-panel"{identity}>'
        f"<summary>{_panel_head(title, meta)}</summary>"
        f'<div class="{_body_class(flush)}">{body}</div></details>'
    )


def group(
    title: str, body: str, *, count: int | None = None, note: str = "", group_id: str | None = None
) -> str:
    identity = f' id="{escape(group_id)}"' if group_id is not None else ""
    number = f'<span class="group-count">{count}</span>' if count is not None else ""
    below = f'<span class="group-note">{escape(note)}</span>' if note else ""
    return (
        f'<section class="group" data-group{identity}><header class="group-head">'
        f"<h2>{escape(title)}</h2>{number}{below}</header>{body}</section>"
    )


def breadcrumbs(ancestors: Sequence[tuple[str, str]], current: str) -> str:
    items = "".join(
        f'<li><a href="{escape(href)}">{escape(label)}</a>'
        '<span class="ui-breadcrumb-separator" aria-hidden="true">/</span></li>'
        for label, href in ancestors
    )
    return (
        '<nav class="ui-breadcrumbs" aria-label="Breadcrumb"><ol>'
        f'{items}<li><span aria-current="page">{escape(current)}</span></li></ol></nav>'
    )


def card_grid(cards: Iterable[str], *, wide: bool = False) -> str:
    variant = " card-grid-wide" if wide else ""
    return f'<div class="card-grid{variant}">{"".join(cards)}</div>'


def card(
    title: str, body: str, *, meta: str = "", card_id: str | None = None, href: str | None = None
) -> str:
    identity = f' id="{escape(card_id)}"' if card_id is not None else ""
    target = f' data-row data-href="{escape(href)}"' if href is not None else ""
    below = f'<div class="ui-panel-meta">{meta}</div>' if meta else ""
    return (
        f'<section class="ui-panel card" data-item{identity}{target}>'
        '<div class="ui-panel-head"><div class="ui-panel-title">'
        f"<h3>{title}</h3></div>{below}</div>"
        f'<div class="ui-panel-body">{body}</div></section>'
    )


def legend(items: Sequence[tuple[str, Mark]], caption: str = "") -> str:
    keys = "".join(
        f'<li><span class="ui-breakdown-key mark-{mark.value}"></span>{escape(label)}</li>'
        for label, mark in items
    )
    note_text = f'<span class="legend-note">{escape(caption)}</span>' if caption else ""
    return f'<div class="legend"><ul class="ui-breakdown-legend">{keys}</ul>{note_text}</div>'


def segments(parts: Sequence[Segment], scale: int) -> str:
    total = sum(part.value for part in parts)
    width = 0.0 if scale <= 0 else min(100.0, total / scale * 100)
    return f'<div class="track">{_fill(parts, f"{width:.1f}%", "thin")}</div>'


def bar_list(bars: Sequence[Bar], scale: int, label: str) -> str:
    return f'<ul class="bar-list" aria-label="{escape(label)}">{"".join(_bar(bar, scale) for bar in bars)}</ul>'


def lane(stops: Sequence[Stop]) -> str:
    cells = "".join(
        f'<div class="stop{" stop-target" if stop.target else ""}" data-tip="{escape(stop.tip)}">'
        f'<span class="stop-dot" style="--dot:{_dot_size(stop.repositories)}px"></span>'
        f'<span class="stop-label">{escape(stop.label)}</span></div>'
        for stop in stops
    )
    return f'<div class="lane">{cells}</div>'


def lines(items: Sequence[Line]) -> str:
    rendered = "".join(
        f"<li data-item{f' data-tip="{escape(item.tip)}"' if item.tip else ''}><span>{item.primary}</span>{item.badge}"
        f'<span class="line-detail">{item.detail}</span></li>'
        for item in items
    )
    return f'<ul class="lines">{rendered}</ul>'


def chip_links(chips: Iterable[str]) -> str:
    return f'<div class="ui-filter-chips">{"".join(chips)}</div>'


def chip_link(label: str, href: str, count: int, *, tip: str = "") -> str:
    number = f'<span class="chip-count">{count}</span>' if count else ""
    hint = f' data-tip="{escape(tip)}"' if tip else ""
    return f'<a class="ui-filter-chip chip" data-item href="{escape(href)}"{hint}>{escape(label)}{number}</a>'


def scope(
    content: str,
    *,
    search: str | None = None,
    limit: int | None = None,
    total: int = 0,
) -> str:
    limited = limit is not None and total > limit
    attributes = f' data-limit="{limit}"' if limited else ""
    more = '<button type="button" class="list-more" data-more hidden></button>' if limited else ""
    return (
        f'<div class="scope" data-table-scope{attributes}>{_toolbar(search)}{content}'
        '<div class="ui-empty-state" data-empty hidden><p class="ui-empty-detail">'
        f"Nothing matches this view.</p></div>{more}</div>"
    )


def static_table(columns: Sequence[Column], rows: Sequence[Sequence[str]], *, label: str) -> str:
    head = "".join(f'<th scope="col"{_cell_class(column)}>{escape(column.label)}</th>' for column in columns)
    classes = [_cell_class(column) for column in columns]
    body = "".join(
        f"<tr>{''.join(f'<td{name}>{cell}</td>' for name, cell in zip(classes, row, strict=True))}</tr>"
        for row in rows
    )
    return (
        f'<div class="ui-data-table-scroll" role="region" aria-label="{escape(label)}" tabindex="0">'
        '<table class="ui-data-table">'
        f"<thead><tr>{head}</tr></thead><tbody>{body}</tbody></table></div>"
    )


def empty_state(title: str, detail: str = "") -> str:
    below = f'<p class="ui-empty-detail">{detail}</p>' if detail else ""
    return (
        '<div class="ui-empty-state"><div class="ui-empty-body"><div class="ui-empty-text">'
        f'<p class="ui-empty-title">{escape(title)}</p>{below}</div></div></div>'
    )


def notice(content: str, tone: Tone | None = None) -> str:
    variant = f" ui-notice-{tone.value}" if tone is not None and tone is not Tone.NEUTRAL else ""
    return f'<div class="ui-notice{variant}"><div>{content}</div></div>'


def copy_button(value: str) -> str:
    return (
        f'<button type="button" class="ui-btn ui-btn-small copy" data-copy="{escape(value)}">'
        "copy</button>"
    )


def shell(
    title: str,
    page_id: str,
    active: str,
    prefix: str,
    navigation: Sequence[NavGroup],
    body: str,
) -> str:
    groups = "".join(
        _nav_group(group, index, prefix, active) for index, group in enumerate(navigation)
    )
    return (
        "<!doctype html>\n"
        '<html lang="en">\n<head>\n<meta charset="utf-8">\n'
        '<meta name="viewport" content="width=device-width, initial-scale=1">\n'
        f"<title>{escape(title)} · dependencies</title>\n"
        f'<link rel="stylesheet" href="{prefix}assets/ui-dashboard/styles.css">\n'
        f'<link rel="stylesheet" href="{prefix}assets/ui-dashboard/layout.css">\n'
        f'<link rel="stylesheet" href="{prefix}assets/ui-dashboard/data.css">\n'
        f'<link rel="stylesheet" href="{prefix}assets/app.css">\n'
        "</head>\n"
        f'<body data-page="{escape(page_id)}" data-prefix="{prefix}">\n'
        '<div class="ui-shell">\n<aside class="ui-side">'
        f'<div class="ui-side-top"><a class="ui-side-brand" href="{prefix}index.html">Dependencies</a></div>'
        f'<div class="ui-side-body">{groups}</div></aside>\n'
        '<main class="ui-main" id="main" tabindex="-1"><div class="page ui-page-section">'
        '<div class="page-search"><label class="ui-search">'
        '<span class="ui-sr-only">Search packages</span>'
        f"{_SEARCH_ICON}"
        '<input id="search" type="search" placeholder="Search packages  /" autocomplete="off">'
        '</label><div id="search-results"></div></div>'
        f"{body}</div></main>\n</div>\n"
        f'<script src="{prefix}assets/search.js"></script>\n'
        f'<script src="{prefix}assets/app.js"></script>\n'
        "</body>\n</html>\n"
    )


def _fill(parts: Sequence[Segment], width: str, variant: str) -> str:
    drawn = [part for part in parts if part.value > 0]
    summary = ", ".join(f"{part.label} {part.value}" for part in drawn)
    spans = "".join(
        f'<span class="seg mark-{part.mark.value}" style="flex-grow:{part.value}" '
        f'data-tip="{escape(part.label)}: {part.value}"></span>'
        for part in drawn
    )
    return (
        f'<div class="fill fill-{variant}" style="width:{width}" role="img" aria-label="{escape(summary)}">'
        f"{spans}</div>"
    )


def _bar(bar: Bar, scale: int) -> str:
    total = sum(part.value for part in bar.parts)
    width = 0.0 if scale <= 0 else min(100.0, total / scale * 100)
    note_text = f'<span class="bar-note">{bar.note}</span>' if bar.note else ""
    hint = f' data-tip="{escape(bar.tip)}"' if bar.tip else ""
    tail = f'<div class="bar-tail">{bar.tail}</div>' if bar.tail else ""
    return (
        f'<li class="bar-row" data-item{hint}><div class="bar-label"><span>{bar.label}</span>{note_text}</div>'
        f'<div class="bar-cell">{_fill(bar.parts, f"calc((100% - 3em) * {width / 100:.3f})", "bar")}'
        f'<span class="bar-value">{total}</span></div>{tail}</li>'
    )


def _dot_size(repositories: int) -> int:
    return min(24, 10 + round(4 * sqrt(max(0, repositories - 1))))


def _panel_head(title: str, meta: str) -> str:
    below = f'<div class="ui-panel-meta">{meta}</div>' if meta else ""
    return (
        '<div class="ui-panel-head"><div class="ui-panel-title">'
        f"<h2>{escape(title)}</h2></div>{below}</div>"
    )


def _body_class(flush: bool) -> str:
    return "ui-panel-body ui-panel-flush" if flush else "ui-panel-body"


def _cell_class(column: Column) -> str:
    flags = (("numeric", column.numeric), ("wrap", column.wrap), ("mono", column.mono))
    names = [name for name, active in flags if active]
    return f' class="{" ".join(names)}"' if names else ""


def _toolbar(search: str | None) -> str:
    if search is None:
        return ""
    return (
        '<div class="ui-table-toolbar"><div></div><label class="ui-search">'
        f'<span class="ui-sr-only">Filter</span>{_SEARCH_ICON}'
        f'<input type="search" data-table-search placeholder="{escape(search)}"></label></div>'
    )


def _nav_group(group: NavGroup, index: int, prefix: str, active: str) -> str:
    links = "".join(_nav_link(link, prefix, active) for link in group.links)
    if not group.heading:
        return f'<nav class="ui-side-nav" aria-label="{escape(group.label)}">{links}</nav>'
    identity = f"ui-side-group-{index}"
    return (
        f'<nav class="ui-side-nav" aria-labelledby="{identity}">'
        f'<span class="ui-side-group" id="{identity}">{escape(group.label)}</span>{links}</nav>'
    )


def _nav_link(link: NavLink, prefix: str, active: str) -> str:
    current = ' aria-current="page"' if link.page == active else ""
    count = f'<span class="nav-count">{link.count}</span>' if link.count is not None and link.count > 0 else ""
    return f'<a href="{prefix}{link.page}.html"{current}>{escape(link.label)}{count}</a>'
