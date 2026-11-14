import hashlib
from dataclasses import dataclass, field
from pathlib import Path

from .errors import OperationError
from .semver import Version, compare_key

SCHEMA_VERSION = 4
FINGERPRINT_GENERATION = "fingerprint"
KINDS = ("runtime", "peer", "build", "dev")
KIND_ORDER = {kind: index for index, kind in enumerate(KINDS)}
ASSETS = Path(__file__).resolve().parents[2] / "stuff-catalog" / "assets"
SEARCH_JS_PATH = ASSETS / "search.js"
STYLESHEET_PATHS = (
    ASSETS / "ui-dashboard" / "tokens.css",
    ASSETS / "ui-dashboard" / "styles.css",
    ASSETS / "ui-dashboard" / "layout.css",
    ASSETS / "catalog.css",
)
UI_STYLES_IMPORT = '@import "./tokens.css";\n'

_ESCAPES = {'"': '\\"', "\\": "\\\\", "\n": "\\n", "\r": "\\r", "\t": "\\t"}
_HTML_ESCAPES = {"&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#x27;"}


def _read_asset(path):
    try:
        return path.read_bytes().decode("utf-8")
    except OSError as error:
        raise OperationError(f"could not read the catalog asset {path}: {error}") from error


def _stylesheet():
    tokens, styles, layout, app = (_read_asset(path) for path in STYLESHEET_PATHS)
    if not styles.startswith(UI_STYLES_IMPORT):
        raise OperationError("ui-dashboard styles.css no longer starts by importing tokens.css")
    return "\n".join([tokens, styles[len(UI_STYLES_IMPORT) :], layout, app])


def _search_js():
    try:
        return SEARCH_JS_PATH.read_bytes().decode("utf-8")
    except OSError as error:
        raise OperationError(f"could not read the catalog search script {SEARCH_JS_PATH}: {error}") from error


@dataclass
class Dependencies:
    runtime: list = field(default_factory=list)
    peer: list = field(default_factory=list)
    build: list = field(default_factory=list)
    dev: list = field(default_factory=list)

    def of(self, kind):
        return getattr(self, kind)

    def iter(self):
        for kind in KINDS:
            for name in self.of(kind):
                yield kind, name

    def normalized(self, owner):
        out = Dependencies()
        for kind in KINDS:
            names = self.of(kind)
            if any(not name or "\0" in name for name in names):
                raise OperationError(f"could not build package catalog: package {owner} has an empty dependency name")
            setattr(out, kind, sorted(set(names)))
        return out


@dataclass
class VersionEntry:
    version: str
    yanked: bool


@dataclass
class Package:
    name: str
    source: str
    latest: str
    versions: list
    dependencies: Dependencies


@dataclass
class Catalog:
    generation: str
    packages: list
    schema_version: int = SCHEMA_VERSION
    registry: str = "cargo"

    def package_count(self):
        return len(self.packages)

    def version_count(self):
        return sum(len(p.versions) for p in self.packages)

    def resolve_dependency(self, name):
        for package in self.packages:
            if package.name == name:
                return package
        return None


@dataclass
class CargoInput:
    name: str
    version: str
    yanked: bool
    source: str
    dependencies: Dependencies


def _latest(versions):
    pool = [v for v in versions if not v.yanked] or list(versions)
    stable = [v for v in pool if Version.parse(v.version).is_stable()]
    ranked = stable or pool
    best = ranked[0]
    for candidate in ranked[1:]:
        if compare_key(candidate.version) >= compare_key(best.version):
            best = candidate
    return best.version


def cargo_catalog(generation, inputs):
    if not generation:
        raise OperationError("could not build package catalog: generation must not be empty")
    grouped = {}
    for item in inputs:
        if not item.name or not item.version:
            raise OperationError("could not build package catalog: cargo package name and version must not be empty")
        try:
            Version.parse(item.version)
        except Exception:
            raise OperationError(
                f"could not build package catalog: invalid cargo version {item.version} for {item.name}"
            ) from None
        grouped.setdefault((item.name, item.source), []).append(item)
    packages = []
    for (name, source), items in sorted(grouped.items()):
        seen = set()
        for item in items:
            if item.version in seen:
                raise OperationError(
                    f"could not build package catalog: cargo package {name} ({source}) "
                    f"has a duplicate version {item.version}"
                )
            seen.add(item.version)
        versions = sorted((VersionEntry(i.version, i.yanked) for i in items), key=lambda v: compare_key(v.version))
        latest = _latest(versions)
        latest_input = next(i for i in items if i.version == latest)
        packages.append(Package(name, source, latest, versions, latest_input.dependencies.normalized(name)))
    return Catalog(generation=generation, packages=packages)


def escape_json(text):
    out = ['"']
    for char in text:
        escaped = _ESCAPES.get(char)
        if escaped is not None:
            out.append(escaped)
        elif ord(char) < 0x20:
            out.append(f"\\u{ord(char):04x}")
        else:
            out.append(char)
    out.append('"')
    return "".join(out)


def _json_names(names):
    return "[" + ", ".join(escape_json(n) for n in names) + "]"


def render_catalog_json(catalog):
    out = [
        "{\n",
        f'  "schema_version": {catalog.schema_version},\n',
        f'  "registry": {escape_json(catalog.registry)},\n',
        f'  "generation": {escape_json(catalog.generation)},\n',
        '  "packages": [',
    ]
    if not catalog.packages:
        out.append("]\n}\n")
        return "".join(out)
    out.append("\n")
    last_package = len(catalog.packages) - 1
    for index, package in enumerate(catalog.packages):
        out.append("    {\n")
        out.append(f'      "name": {escape_json(package.name)},\n')
        out.append(f'      "source": {escape_json(package.source)},\n')
        out.append(f'      "latest": {escape_json(package.latest)},\n')
        out.append('      "versions": [')
        if not package.versions:
            out.append("],\n")
        else:
            out.append("\n")
            last_version = len(package.versions) - 1
            for position, version in enumerate(package.versions):
                yanked = "true" if version.yanked else "false"
                out.append(f'        {{"version": {escape_json(version.version)}, "yanked": {yanked}}}')
                if position < last_version:
                    out.append(",")
                out.append("\n")
            out.append("      ],\n")
        kinds = ", ".join(f'"{kind}": {_json_names(package.dependencies.of(kind))}' for kind in KINDS)
        out.append(f'      "dependencies": {{{kinds}}}\n')
        out.append("    }")
        if index < last_package:
            out.append(",")
        out.append("\n")
    out.append("  ]\n}\n")
    return "".join(out)


def escape_html(text):
    return "".join(_HTML_ESCAPES.get(c, c) for c in text)


def _anchor(package):
    return f"cargo/{package.source}/{package.name}"


def _fragment_href(anchor):
    out = ["#"]
    for byte in anchor.encode("utf-8"):
        char = chr(byte)
        if char.isascii() and (char.isalnum() or char in "-._~@/:"):
            out.append(char)
        else:
            out.append(f"%{byte:02X}")
    return "".join(out)


def _package_link(package):
    return f'<a href="{escape_html(_fragment_href(_anchor(package)))}">{escape_html(package.name)}</a>'


def _document(title, brand, nav, page):
    links = "".join(f'<a href="{escape_html(href)}">{escape_html(label)}</a>' for href, label in nav)
    return (
        '<!DOCTYPE html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n'
        '<meta name="viewport" content="width=device-width, initial-scale=1">\n'
        f"<title>{escape_html(title)}</title>\n<style>{_stylesheet()}</style>\n</head>\n<body>\n"
        '<div class="ui-shell">\n<aside class="ui-side">\n'
        f'<div class="ui-side-top"><a class="ui-side-brand" href="#">{escape_html(brand)}</a></div>\n'
        f'<div class="ui-side-body"><nav class="ui-side-nav" aria-label="Main">{links}</nav></div>\n'
        '</aside>\n<main class="ui-main" id="main" tabindex="-1">\n<div class="page">\n'
        f"{page}</div>\n</main>\n</div>\n<script>{_search_js()}</script>\n</body>\n</html>\n"
    )


def _dependents_of(catalog):
    dependents = {}
    for package in catalog.packages:
        strongest = {}
        for kind, name in package.dependencies.iter():
            target = catalog.resolve_dependency(name)
            if target is not None:
                anchor = _anchor(target)
                current = strongest.get(anchor)
                if current is None or KIND_ORDER[kind] < KIND_ORDER[current]:
                    strongest[anchor] = kind
        for anchor in sorted(strongest):
            dependents.setdefault(anchor, []).append((package, strongest[anchor]))
    return dependents


_SHORT_LIST = 12
_KIND_TITLES = {"peer": "Peer", "build": "Build", "dev": "Dev"}
_SEARCH_ICON = (
    '<svg class="ui-icon" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" '
    'stroke-width="1.6" stroke-linecap="round" aria-hidden="true">'
    '<path d="M10.5 4a6.5 6.5 0 1 0 0 13 6.5 6.5 0 0 0 0-13Z"/><path d="m15.2 15.2 4.3 4.3"/></svg>'
)


def _count(n):
    return f"{n:,}"


def _plural(n, one, many):
    return f"{_count(n)} {one if n == 1 else many}"


def _panel(panel_id, title, meta, body):
    identity = f' id="{escape_html(panel_id)}"' if panel_id is not None else ""
    meta_html = f'<span class="ui-panel-meta">{escape_html(meta)}</span>' if meta else ""
    return (
        f'<section class="ui-panel"{identity}><div class="ui-panel-head"><div class="ui-panel-title">'
        f"<h2>{escape_html(title)}</h2></div>{meta_html}</div>"
        f'<div class="ui-panel-body">{body}</div></section>\n'
    )


def _group(title, body):
    return f'<div class="group"><div class="group-head"><h3>{escape_html(title)}</h3></div>{body}</div>'


def _short_list(rows, shown):
    out = '<ol class="rows">' + "".join(rows[:shown]) + "</ol>"
    if len(rows) > shown:
        out += (
            f'<details class="more"><summary>Show {_count(len(rows) - shown)} more</summary>'
            '<ol class="rows">' + "".join(rows[shown:]) + "</ol></details>"
        )
    return out


def _page_header(title, sub_html):
    return (
        f'<header class="ui-page-header"><h1>{escape_html(title)}</h1></header>\n'
        f'<p class="page-sub">{sub_html}</p>\n'
    )


def _status_strip(tiles):
    cells = "".join(
        f'<a class="tile" href="{escape_html(href)}"><span class="tile-label">{escape_html(label)}</span>'
        f'<span class="tile-value">{escape_html(value)}</span><span class="tile-note">{escape_html(note)}</span></a>'
        for label, value, note, href in tiles
    )
    return f'<div class="status-strip" style="--cells:{max(len(tiles), 1)}">{cells}</div>\n'


def _find_panel(catalog):
    total = _plural(catalog.package_count(), "package", "packages")
    rows = "".join(
        f'<li data-name="{escape_html(package.name)}" data-registry="cargo" '
        f'data-source="{escape_html(package.source)}">'
        f'<span class="row-name">{_package_link(package)}'
        f'<span class="row-note">{escape_html("Cargo · " + package.source)}</span></span>'
        f'<span class="row-value">{escape_html(package.latest)}</span></li>'
        for package in catalog.packages
    )
    body = (
        '<div class="find">'
        f'<label class="ui-search" data-package-search-box hidden>{_SEARCH_ICON}'
        '<input type="search" data-package-search placeholder="Find a package by name" '
        'aria-label="Find a package" autocomplete="off"></label>'
        f'<details class="browse" data-browse><summary>Browse all {total}</summary>'
        f'<ol class="rows" data-packages>{rows}</ol></details>'
        '<p class="note" data-no-match hidden>No package matches.</p>'
        '<button class="ui-btn" type="button" data-more hidden></button>'
        "</div>"
    )
    return _panel("find", "Find a package", total, body)


def _published_here(catalog):
    return [package for package in catalog.packages if package.source == "native"]


def _published_here_panel(catalog):
    rows = [
        f'<li><span class="row-name">{_package_link(package)}</span>'
        f'<span class="row-value">{escape_html(package.latest)}</span></li>'
        for package in _published_here(catalog)
    ]
    if not rows:
        return ""
    return _panel(
        "published",
        f"Published here · {_count(len(rows))}",
        f"{_count(catalog.package_count() - len(rows))} mirrored",
        f'<div class="columns">{_short_list(rows, 24)}</div>',
    )


def _setup_panel(catalog):
    generation = escape_html(catalog.generation)
    body = (
        '<p class="lead">Setup for this registry, with the client configuration, is in '
        f'<a href="/generations/{generation}/llms.txt">llms.txt</a>.</p>'
        f'<p class="panel-foot"><a href="/generations/{generation}/catalog.json">catalog.json</a></p>'
    )
    return _panel("r-cargo", "Setup", "", body)


def _name_rows(catalog, names):
    hosted = []
    elsewhere = []
    for name in names:
        target = catalog.resolve_dependency(name)
        if target is not None:
            hosted.append(f"<li>{_package_link(target)}</li>")
        else:
            elsewhere.append(f"<code>{escape_html(name)}</code>")
    out = ""
    if hosted:
        out += _short_list(hosted, _SHORT_LIST)
    if elsewhere:
        out += f'<p class="note">Not hosted here: {", ".join(elsewhere)}</p>'
    return out


def _depends_on_panel(catalog, package):
    if not package.dependencies.runtime:
        body = f'<p class="note">No runtime dependencies in {escape_html(package.latest)}.</p>'
    else:
        body = _name_rows(catalog, package.dependencies.runtime)
    if catalog.schema_version < 3:
        body += '<p class="note">Peer, build and dev dependencies were not recorded for this generation.</p>'
    else:
        for kind in ("peer", "build", "dev"):
            names = package.dependencies.of(kind)
            if names:
                body += _group(f"{_KIND_TITLES[kind]} · {len(names)}", _name_rows(catalog, names))
    total = sum(len(package.dependencies.of(kind)) for kind in KINDS)
    return _panel(None, "Depends on", _plural(total, "package", "packages"), body)


def _used_by_panel(users):
    users = users or []
    if not users:
        body = '<p class="note">No package here depends on it.</p>'
    else:
        rows = []
        for user, kind in users:
            note = "" if kind == "runtime" else f'<span class="row-note">{kind}</span>'
            rows.append(f"<li>{_package_link(user)}{note}</li>")
        body = _short_list(rows, _SHORT_LIST)
    return _panel(None, "Used by", _plural(len(users), "package", "packages"), body)


def _versions_panel(package):
    rows = []
    for version in reversed(package.versions):
        if version.yanked:
            state = '<span class="state state-bad">Yanked</span>'
        elif version.version == package.latest:
            state = '<span class="row-note">latest</span>'
        else:
            state = ""
        rows.append(f'<li><span class="mono">{escape_html(version.version)}</span>{state}</li>')
    meta = _plural(len(package.versions), "version", "versions")
    yanked = sum(1 for version in package.versions if version.yanked)
    if yanked:
        meta += f", {yanked} yanked"
    return _panel(None, "Versions", meta, _short_list(rows, 8))


def _detail_sections(catalog):
    out = []
    dependents = _dependents_of(catalog)
    separator = '<span class="ui-breadcrumb-separator" aria-hidden="true">/</span>'
    for package in catalog.packages:
        anchor = _anchor(package)
        command = escape_html(f"cargo add {package.name}@{package.latest}")
        out.append(f'<section class="detail page" id="{escape_html(anchor)}">\n')
        out.append(
            f'<nav class="ui-breadcrumbs" aria-label="Breadcrumb"><ol><li><a href="#">Cargo</a>{separator}</li>'
            f'<li><span aria-current="page">{escape_html(package.name)}</span></li></ol></nav>\n'
        )
        out.append(
            _page_header(
                package.name,
                f"Cargo · {escape_html(package.source)} · latest <b>{escape_html(package.latest)}</b>",
            )
        )
        out.append(f'<div class="install"><pre class="snippet"><code>{command}</code></pre></div>\n')
        out.append('<div class="ui-dashboard-grid">\n')
        out.append(_versions_panel(package))
        out.append(_depends_on_panel(catalog, package))
        out.append(_used_by_panel(dependents.get(anchor)))
        out.append("</div>\n</section>\n")
    return "".join(out)


def render_html(catalog):
    packages = catalog.package_count()
    native = len(_published_here(catalog))
    tiles = [("Packages", _count(packages), _plural(catalog.version_count(), "version", "versions"), "#find")]
    if native:
        tiles.append(("Published here", _count(native), f"{_count(packages - native)} mirrored", "#published"))
    generation = escape_html(catalog.generation)
    page = [
        '<div class="index">\n',
        _page_header(
            "Cargo registry",
            f"{_count(packages)} packages served as static files from generation <code>{generation}</code>.",
        ),
        _status_strip(tiles),
        _find_panel(catalog),
        _published_here_panel(catalog),
        _setup_panel(catalog),
        "</div>\n",
        _detail_sections(catalog),
    ]
    nav = [("#find", "Find")]
    if native:
        nav.append(("#published", "Published here"))
    nav.append(("#r-cargo", "Setup"))
    return _document("Cargo registry browser", "Cargo", nav, "".join(page))


def _llms_use_section(origin, generation):
    catalog = f"/generations/{generation}/catalog.json"
    index = f"/generations/{generation}/index.html"
    return (
        "Configure Cargo against the sparse index under `current/registry/`:\n"
        "\n"
        "```toml\n"
        "[source.crates-io]\n"
        'replace-with = "stuff"\n'
        "[source.stuff]\n"
        f'registry = "sparse+{origin}/current/registry/"\n'
        "```\n"
        "\n"
        "Index files live at `current/registry/<shard>` (sharded like the crates.io sparse index; "
        "mirrored upstream entries under `current/crates-io/`). "
        f"Config: [config.json](/generations/{generation}/registry/config.json). "
        "Blobs: `blobs/<sha256>.crate` via the `dl` template. "
        f"Inventory: [catalog.json]({catalog}). Browser: [index.html]({index}). "
        "Pin a generation with `current` (`current -> generations/<id>`); "
        "immutable snapshots live under `generations/<id>/`."
    )


def render_llms_txt(catalog, origin):
    origin = origin.rstrip("/")
    generation = catalog.generation
    packages = catalog.package_count()
    versions = catalog.version_count()
    out = [
        "# Cargo registry\n",
        f"> Static snapshot for LLMs. Generation `{generation}`: "
        f"{packages} {'package' if packages == 1 else 'packages'}, "
        f"{versions} {'version' if versions == 1 else 'versions'}. "
        f"Machine-readable inventory: [catalog.json](/generations/{generation}/catalog.json). "
        f"Human browser: [index.html](/generations/{generation}/index.html).\n",
        "\n## Registry\n\n",
        f"- Origin: {origin}\n",
        f"- Pinned generation: `generations/{generation}/`\n",
        f"- Inventory: [catalog.json](/generations/{generation}/catalog.json)\n",
        f"- Browser: [index.html](/generations/{generation}/index.html)\n",
        "\n## Use\n\n",
        _llms_use_section(origin, generation),
        "\n\n## Packages\n\n",
    ]
    if not catalog.packages:
        out.append("No packages published in this generation.\n")
    for package in catalog.packages:
        count = len(package.versions)
        listed = ", ".join(f"`{v.version}`" + (" (yanked)" if v.yanked else "") for v in package.versions)
        out.append(
            f"- `{package.name}` — latest `{package.latest}` "
            f"({count} {'version' if count == 1 else 'versions'}): {listed} [{package.source}]\n"
        )
    out.append("\n")
    return "".join(out)


def catalog_fingerprint(catalog, origin):
    sealed = Catalog(FINGERPRINT_GENERATION, catalog.packages, catalog.schema_version, catalog.registry)
    digest = hashlib.sha256()
    digest.update(render_catalog_json(sealed).encode("utf-8"))
    digest.update(b"\0")
    digest.update(render_html(sealed).encode("utf-8"))
    digest.update(b"\0")
    digest.update(render_llms_txt(sealed, origin).encode("utf-8"))
    return digest.hexdigest()
