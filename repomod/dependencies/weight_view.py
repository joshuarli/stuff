from __future__ import annotations

from collections.abc import Callable, Sequence
from urllib.parse import quote

from repomod.dependencies import ui, weight
from repomod.dependencies.ui import escape as _e
from repomod.dependencies.weight import (
    DEFAULT_PLATFORM,
    DEFAULT_VARIANT,
    LANGUAGES,
    PLATFORMS,
    TOP_PER_LANGUAGE,
    VARIANTS,
    Package,
    Repository,
    Total,
)

type PackageLink = Callable[[str, str], str]

EMPTY_NAMES = 12


def hash_for(repository: str) -> str:
    return f"repo={quote(repository, safe='/')}"


def value_text(total: Total) -> str:
    if total.bytes > 0:
        return weight.format_bytes(total.bytes)
    return "size unknown" if total.packages > 0 else "none"


def note_text(total: Total) -> str:
    if total.packages == 0:
        return "no packages"
    count = f"{total.packages} package{'' if total.packages == 1 else 's'}"
    return f"{count} · {total.unknown} size unknown" if total.unknown else count


def variant_key(variant: weight.Variant) -> str:
    return f"{variant[0]}-{'dev' if variant[1] else 'run'}"


def tip_text(row: Repository, keys: Sequence[str], variant: weight.Variant = DEFAULT_VARIANT) -> str:
    total = row.total(keys, variant)
    split = [
        f"{language.short} {weight.format_bytes(row.totals[variant][language.key].bytes)}"
        for language in LANGUAGES
        if language.key in keys and row.totals[variant][language.key].bytes > 0
    ]
    detail = f" ({', '.join(split)})" if len(split) > 1 else ""
    return f"{row.repository}: {value_text(total)}{detail}, {note_text(total)}"


def empty_text(
    rows: Sequence[Repository],
    keys: Sequence[str],
    variant: weight.Variant = DEFAULT_VARIANT,
    names: int = EMPTY_NAMES,
) -> str:
    empty = sorted(
        (row.repository for row in rows if row.total(keys, variant).packages == 0), key=str.lower
    )
    if not empty:
        return ""
    what = (
        "third-party"
        if len(keys) == len(LANGUAGES)
        else " or ".join(language.short for language in LANGUAGES if language.key in keys)
    )
    count = f"{len(empty)} {'repository has' if len(empty) == 1 else 'repositories have'} no {what} dependencies"
    shown = ", ".join(empty[:names])
    more = f" and {len(empty) - names} more" if len(empty) > names else ""
    return f"{count}: {shown}{more}."


def cell_text(sized: bool, total: Total) -> tuple[str, str]:
    if total.packages == 0:
        return "—", ""
    if sized and total.bytes > 0:
        return weight.format_bytes(total.bytes), str(total.packages)
    return str(total.packages), "no size"


def page(rows: Sequence[Repository], link: PackageLink) -> str:
    header = ui.page_header(
        "Weight",
        "How much third-party code each repository carries, and which packages to drop first. "
        "A direct dependency can be removed; a transitive package leaves only with whatever brings it in "
        '(<a href="bloat.html">Bloat</a> shows that). Your own packages are left out.',
    )
    if not rows:
        return header + ui.empty_state("No repositories", "No owned repository in this view has an inventory.")
    everything = [language.key for language in LANGUAGES]
    ordered = sorted(
        rows,
        key=lambda row: (-row.total(everything).bytes, -row.total(everything).packages, row.repository.lower()),
    )
    scale = max((row.total(everything).bytes for row in ordered), default=0)
    sized = any(package.bytes is not None for row in ordered for package in row.heaviest)
    body = [header]
    if not sized:
        body.append(
            ui.notice(
                "Package sizes are not looked up yet. Run "
                '<span class="mono">uv run repomod deps enrich</span>, then render again.'
            )
        )
    body.append(_filters())
    body.append(
        ui.panel(
            "Third-party weight by repository",
            f'<ul class="weight-list" data-weight-list aria-label="Third-party weight by repository">'
            f"{''.join(_row(row, scale, link) for row in ordered)}</ul>"
            f'<p class="weight-empty" data-empty-line{"" if empty_text(ordered, everything) else " hidden"}>'
            f"{_e(empty_text(ordered, everything))}</p>",
        )
    )
    body.append(ui.collapsed_panel("Table view", _table(ordered), meta="The same numbers", flush=True))
    return (
        f'<div class="weight" data-weight data-limit="{TOP_PER_LANGUAGE}" data-names="{EMPTY_NAMES}">'
        + "\n".join(body)
        + "</div>"
    )


def _filters() -> str:
    hint = ' <span class="weight-hint">count only</span>'
    languages = "".join(
        f'<button type="button" class="ui-segment" aria-pressed="true" data-lang-toggle="{language.key}">'
        f"{_e(language.label)}{'' if language.sized else hint}</button>"
        for language in LANGUAGES
    )
    platforms = "".join(
        f'<button type="button" class="ui-segment" aria-pressed="{str(platform.key == DEFAULT_PLATFORM).lower()}"'
        f' data-platform-toggle="{platform.key}">{_e(platform.label)}</button>'
        for platform in PLATFORMS
    )
    measures = "".join(
        f'<li data-lang-show="{language.key}"><span class="weight-lang">{_e(language.label)}</span> '
        f"{_e(language.measure)}</li>"
        for language in LANGUAGES
    )
    scope = (
        '<button type="button" class="ui-segment" aria-pressed="false" data-dev-toggle>'
        "Include dev dependencies</button>"
    )
    return (
        '<div class="weight-filters">'
        '<div class="weight-controls" data-weight-controls hidden>'
        f'{_control("Languages", languages)}{_control("npm platform", platforms)}{_control("Scope", scope)}'
        "</div>"
        f'<ul class="weight-measures">{measures}</ul>'
        '<p class="weight-caption">These are different measures, so a total that spans languages adds '
        "unlike sizes: compare repositories on one language, or read a mixed total as a rough scale. "
        "Each package counts once per repository, however many lockfiles list it. The platform applies to npm "
        "packages that only an optional dependency brings in and whose os, cpu, or libc fields exclude it; "
        "everything else always counts. Dev-only means reached only through dev, test, build, or CI "
        "dependencies. Lockfiles list every platform's packages, so All platforms is an upper bound.</p>"
        "</div>"
    )


def _control(label: str, buttons: str) -> str:
    return (
        f'<div class="weight-control"><span class="weight-control-label">{_e(label)}</span>'
        f'<div class="ui-segmented" role="group" aria-label="{_e(label)}">{buttons}</div></div>'
    )


def _row(row: Repository, scale: int, link: PackageLink) -> str:
    keys = [language.key for language in LANGUAGES]
    total = row.total(keys)
    attributes = "".join(
        f' data-t-{variant_key(variant)}-{key}="{t.bytes},{t.packages},{t.unknown}"'
        for variant in VARIANTS
        for key, t in row.totals[variant].items()
    )
    ratio = total.bytes / scale if scale > 0 else 0.0
    tip = tip_text(row, keys)
    return (
        f'<li class="weight-row" id="repo-{_e(row.repository)}" data-weight-row data-repo="{_e(row.repository)}"'
        f'{attributes}{"" if total.packages else " hidden"}>'
        '<details class="weight-details">'
        f'<summary class="weight-summary" data-summary data-tip="{_e(tip)}">'
        f'<span class="weight-name"><span class="weight-repo">{_e(row.repository)}</span>'
        f'<span class="weight-note" data-note>{_e(note_text(total))}</span></span>'
        f'<span class="weight-bar"><span class="fill fill-bar" data-fill style="--r:{ratio:.4f}"'
        f'{"" if total.bytes else " hidden"} aria-hidden="true">'
        '<span class="seg mark-accent"></span></span>'
        f'<span class="weight-value{"" if total.bytes else " weight-none"}" data-value>{_e(value_text(total))}</span>'
        "</span></summary>"
        f'<div class="weight-body">{_packages(row, total, link)}'
        f'<p class="weight-open"><a href="repos/{_e(row.repository)}/index.html">Open {_e(row.repository)}</a></p>'
        "</div></details></li>"
    )


def _packages(row: Repository, total: Total, link: PackageLink) -> str:
    counted = [package for package in row.heaviest if package.counts(DEFAULT_VARIANT)]
    shown = {id(package) for package in counted[:TOP_PER_LANGUAGE]}
    if not row.heaviest:
        return '<p class="weight-count" data-count>The selected languages report no package sizes.</p>'
    lines = "".join(_package_row(package, id(package) in shown, link) for package in row.heaviest)
    count = (
        f"Heaviest {min(len(counted), TOP_PER_LANGUAGE)} of {total.packages} packages"
        + (f", {total.unknown} without a size." if total.unknown else ".")
        if counted
        else "The selected languages report no package sizes."
    )
    return (
        f'<p class="weight-count" data-count>{_e(count)}</p>'
        '<div class="ui-data-table-scroll" tabindex="0" role="region" '
        f'aria-label="Heaviest packages in {_e(row.repository)}"><table class="ui-data-table weight-packages">'
        '<thead><tr><th scope="col">Package</th><th scope="col">Version</th><th scope="col">Registry</th>'
        '<th scope="col" class="numeric">Size</th><th scope="col">Measure</th><th scope="col">Role</th></tr></thead>'
        f"<tbody data-packages>{lines}</tbody></table></div>"
    )


def _package_row(package: Package, visible: bool, link: PackageLink) -> str:
    hidden = "" if visible else " hidden"
    role = "direct" if package.direct else '<span class="cell-note">transitive</span>'
    platforms = " ".join(sorted(package.platforms))
    dev = ' data-dev="1"' if package.dev_only else ""
    return (
        f'<tr data-lang="{package.language}" data-bytes="{package.bytes or 0}" data-platforms="{platforms}"'
        f"{dev}{hidden}>"
        f"<td>{link(package.purl, package.name)}</td>"
        f'<td class="mono">{_e(package.version or "")}</td>'
        f"<td>{_e(package.ecosystem.value)}</td>"
        f'<td class="numeric">{_e(weight.format_bytes(package.bytes or 0))}</td>'
        f"<td>{_e(weight.kind_label(package.kind))}</td>"
        f"<td>{role}</td></tr>"
    )


def _table(rows: Sequence[Repository]) -> str:
    head = "".join(
        f'<th scope="col" class="numeric" data-lang-show="{language.key}">{_e(language.label)}</th>'
        for language in LANGUAGES
    )
    body = "".join(_table_row(row) for row in rows)
    return (
        '<div class="ui-data-table-scroll" role="region" aria-label="Third-party weight table" tabindex="0">'
        '<table class="ui-data-table weight-table"><thead><tr><th scope="col">Repository</th>'
        f'{head}<th scope="col" class="numeric">Total</th><th scope="col" class="numeric">Packages</th>'
        '<th scope="col" class="numeric">Size unknown</th></tr></thead>'
        f"<tbody data-weight-table>{body}</tbody></table></div>"
    )


def _table_row(row: Repository) -> str:
    keys = [language.key for language in LANGUAGES]
    total = row.total(keys)
    cells = "".join(
        _table_cell(language.key, language.sized, row.totals[DEFAULT_VARIANT][language.key])
        for language in LANGUAGES
    )
    return (
        f'<tr data-weight-table-row data-repo="{_e(row.repository)}">'
        f'<td><a href="repos/{_e(row.repository)}/index.html">{_e(row.repository)}</a></td>{cells}'
        f'<td class="numeric" data-total-bytes>{_e(value_text(total))}</td>'
        f'<td class="numeric" data-total-packages>{total.packages}</td>'
        f'<td class="numeric" data-total-unknown>{total.unknown}</td></tr>'
    )


def _table_cell(key: str, sized: bool, total: Total) -> str:
    text, note = cell_text(sized, total)
    suffix = f' <span class="cell-note">{note}</span>' if note else ""
    return f'<td class="numeric" data-lang-show="{key}" data-cell="{key}">{_e(text)}{suffix}</td>'


def repository_panel(row: Repository, prefix: str) -> str:
    items = []
    for language in LANGUAGES:
        total = row.totals[DEFAULT_VARIANT][language.key]
        if total.packages == 0:
            continue
        count = f"{total.packages} package{'' if total.packages == 1 else 's'}"
        if language.sized and total.bytes > 0:
            unknown = f" · {total.unknown} size unknown" if total.unknown else ""
            items.append(
                f"<li><span>{_e(language.label)}</span> <strong>{_e(weight.format_bytes(total.bytes))}</strong> "
                f'<span class="cell-note">{_e(language.unit)} · {count}{unknown}</span></li>'
            )
        else:
            items.append(
                f"<li><span>{_e(language.label)}</span> "
                f'<span class="cell-note">{count} · size unknown</span></li>'
            )
    items.append('<li class="cell-note">macOS arm64, dev dependencies left out</li>')
    link = f'<a href="{prefix}weight.html#{hash_for(row.repository)}">Compare on Weight</a>'
    return ui.panel("Dependency weight", f'<ul class="plain-list">{"".join(items)}</ul>', meta=link, panel_id="weight")
