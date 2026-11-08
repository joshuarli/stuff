from __future__ import annotations

import html
from collections.abc import Sequence
from dataclasses import dataclass
from enum import Enum

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
class Row:
    cells: Sequence[str]
    tags: Sequence[str] = ()
    href: str | None = None
    row_id: str | None = None


@dataclass(frozen=True, slots=True)
class Filter:
    tag: str
    label: str
    count: int
    pressed: bool = False


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


def badge(label: str, tone: Tone = Tone.NEUTRAL, *, dot: bool = False) -> str:
    mark = '<span class="ui-badge-dot" aria-hidden="true"></span>' if dot else ""
    return f'<span class="ui-badge ui-badge-{tone.value}">{mark}{escape(label)}</span>'


def note(content: str) -> str:
    return f'<span class="cell-note">{content}</span>'


def stack(primary: str, secondary: str = "") -> str:
    below = note(secondary) if secondary else ""
    return f'<span class="cell-stack"><span>{primary}</span>{below}</span>'


def meter(value: int, maximum: int, tone: Tone = Tone.NEUTRAL) -> str:
    width = 0.0 if maximum <= 0 else max(0.0, min(100.0, value / maximum * 100))
    return (
        '<div class="ui-meter" aria-hidden="true">'
        f'<span class="ui-meter-bar ui-meter-{tone.value}" style="width:{width:.1f}%"></span></div>'
    )


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


def table(
    label: str,
    columns: Sequence[Column],
    rows: Sequence[Row],
    *,
    filters: Sequence[Filter] = (),
    search: str | None = None,
    limit: int | None = None,
) -> str:
    limited = limit is not None and len(rows) > limit
    attributes = f' data-limit="{limit}"' if limited else ""
    more = '<button type="button" class="list-more" data-more hidden></button>' if limited else ""
    head = "".join(f'<th scope="col"{_cell_class(column)}>{escape(column.label)}</th>' for column in columns)
    body = "".join(_row(columns, row) for row in rows)
    return (
        f'<div data-table-scope{attributes}>{_toolbar(filters, search)}'
        f'<div class="ui-table-scroll" role="region" aria-label="{escape(label)} table">'
        f'<table class="ui-data-table" aria-label="{escape(label)}">'
        f"<thead><tr>{head}</tr></thead><tbody>{body}</tbody></table></div>"
        '<div class="ui-empty-state" data-empty hidden><p class="ui-empty-detail">'
        "Nothing matches this view.</p></div>"
        f"{more}</div>"
    )


def static_table(columns: Sequence[Column], rows: Sequence[Sequence[str]]) -> str:
    head = "".join(f'<th scope="col"{_cell_class(column)}>{escape(column.label)}</th>' for column in columns)
    classes = [_cell_class(column) for column in columns]
    body = "".join(
        f"<tr>{''.join(f'<td{name}>{cell}</td>' for name, cell in zip(classes, row, strict=True))}</tr>"
        for row in rows
    )
    return (
        '<div class="ui-table-scroll"><table class="ui-data-table">'
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
        f'<link rel="stylesheet" href="{prefix}assets/app.css">\n'
        "</head>\n"
        f'<body data-page="{escape(page_id)}" data-prefix="{prefix}">\n'
        '<div class="ui-shell">\n<aside class="ui-side">'
        f'<div class="ui-side-top"><a class="ui-side-brand" href="{prefix}index.html">Dependencies</a></div>'
        f'<div class="ui-side-body">{groups}</div></aside>\n'
        '<main class="ui-main" id="main" tabindex="-1"><div class="page">'
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


def _row(columns: Sequence[Column], row: Row) -> str:
    attributes = " data-row" if row.href is not None else ""
    if row.tags:
        attributes += f' data-tags="{escape(" ".join(row.tags))}"'
    if row.href is not None:
        attributes += f' data-href="{escape(row.href)}"'
    if row.row_id is not None:
        attributes += f' id="{escape(row.row_id)}"'
    cells = "".join(
        f"<td{_cell_class(column)}>{cell}</td>"
        for column, cell in zip(columns, row.cells, strict=True)
    )
    return f"<tr{attributes}>{cells}</tr>"


def _toolbar(filters: Sequence[Filter], search: str | None) -> str:
    if not filters and search is None:
        return ""
    chips = "".join(
        f'<button class="chip" type="button" data-filter="{escape(item.tag)}" '
        f'aria-pressed="{"true" if item.pressed else "false"}">{escape(item.label)}'
        f'<span class="chip-count">{item.count}</span></button>'
        for item in filters
    )
    group = f'<div class="chips" role="group" aria-label="Filter rows">{chips}</div>' if chips else "<div></div>"
    box = (
        '<label class="ui-search"><span class="ui-sr-only">Filter rows</span>'
        f'{_SEARCH_ICON}<input type="search" data-table-search placeholder="{escape(search)}"></label>'
        if search is not None
        else ""
    )
    return f'<div class="ui-table-toolbar">{group}{box}</div>'


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
