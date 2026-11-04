import hashlib
from dataclasses import dataclass, field
from pathlib import Path

from .catalog_style import CSS
from .errors import OperationError
from .semver import Version, compare_key

SCHEMA_VERSION = 4
FINGERPRINT_GENERATION = "fingerprint"
KINDS = ("runtime", "peer", "build", "dev")
KIND_ORDER = {kind: index for index, kind in enumerate(KINDS)}
SEARCH_JS_PATH = Path(__file__).resolve().parents[2] / "stuff-catalog" / "assets" / "search.js"

_ESCAPES = {'"': '\\"', "\\": "\\\\", "\n": "\\n", "\r": "\\r", "\t": "\\t"}
_HTML_ESCAPES = {"&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#x27;"}


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


def _versions_markup(package):
    parts = []
    for version in package.versions:
        text = escape_html(version.version)
        if version.yanked:
            parts.append(f'<span class="yanked" title="Yanked">{text}</span>')
        elif version.version == package.latest:
            parts.append(f"<b>{text}</b>")
        else:
            parts.append(text)
    return " ".join(parts)


def _document(title, body):
    return (
        '<!DOCTYPE html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n'
        '<meta name="viewport" content="width=device-width, initial-scale=1">\n'
        f"<title>{escape_html(title)}</title>\n<style>{CSS}</style>\n</head>\n<body>\n"
        f"{body}<script>{_search_js()}</script>\n</body>\n</html>\n"
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


def _name_list(catalog, names):
    hosted = []
    elsewhere = []
    for name in names:
        target = catalog.resolve_dependency(name)
        if target is not None:
            hosted.append(f'<li class="pkg">{_package_link(target)}</li>\n')
        else:
            elsewhere.append(f'<span class="pkg">{escape_html(name)}</span>')
    out = ""
    if hosted:
        out += '<ul class="refs">\n' + "".join(hosted) + "</ul>\n"
    if elsewhere:
        out += f'<p class="mute">Not hosted here: {", ".join(elsewhere)}</p>\n'
    return out


KIND_TITLES = {
    "peer": "Peer dependencies",
    "build": "Build dependencies",
    "dev": "Dev dependencies",
}


def _dependency_sections(catalog, package, users):
    out = ["<h2>Depends on</h2>\n"]
    if not package.dependencies.runtime:
        out.append(f'<p class="mute">No dependencies in {escape_html(package.latest)}.</p>\n')
    else:
        out.append(_name_list(catalog, package.dependencies.runtime))
    for kind in ("peer", "build", "dev"):
        names = package.dependencies.of(kind)
        if names:
            out.append(
                f"<details>\n<summary>{KIND_TITLES[kind]} ({len(names)})</summary>\n"
                f"{_name_list(catalog, names)}</details>\n"
            )
    out.append("<h2>Used by</h2>\n")
    if users is None:
        out.append('<p class="mute">No package here depends on it.</p>\n')
    else:
        out.append('<ul class="refs">\n')
        for user, kind in users:
            note = "" if kind == "runtime" else f'<span class="mute">{kind}</span>'
            out.append(f'<li class="pkg">{_package_link(user)}{note}</li>\n')
        out.append("</ul>\n")
    return "".join(out)


def _detail_sections(catalog):
    out = []
    dependents = _dependents_of(catalog)
    for package in catalog.packages:
        anchor = _anchor(package)
        out.append(f'<section class="detail" id="{escape_html(anchor)}">\n')
        out.append('<p class="back"><a href="#">All packages</a></p>\n')
        out.append(
            f'<h1 class="pkg">{escape_html(package.name)}</h1>\n'
            f'<p class="lede">Cargo{escape_html(", " + package.source)}. Latest {escape_html(package.latest)}.</p>\n'
        )
        out.append(_dependency_sections(catalog, package, dependents.get(anchor)))
        out.append(f'<h2>Versions</h2>\n<p class="vers">{_versions_markup(package)}</p>\n</section>\n')
    return "".join(out)


def render_html(catalog):
    out = [
        "<header>Cargo</header>\n<main>\n",
        '<div class="index">\n<h1>Cargo registry</h1>\n'
        f'<p class="lede">{catalog.package_count()} packages, {catalog.version_count()} versions</p>\n',
        '<input type="search" data-package-search hidden placeholder="Find a package" '
        'aria-label="Find a package" autocomplete="off">\n',
        '<table data-packages>\n<thead><tr><th>Package</th><th class="hide">Source</th><th>Versions</th></tr></thead>\n<tbody>\n',
    ]
    for package in catalog.packages:
        out.append(
            f'<tr data-name="{escape_html(package.name)}" data-registry="cargo" '
            f'data-source="{escape_html(package.source)}">'
        )
        out.append(f'<td class="pkg">{_package_link(package)}</td>')
        out.append(f'<td class="mute hide">{escape_html(package.source)}</td>')
        out.append(f'<td class="vers">{_versions_markup(package)}</td></tr>\n')
    out.append("</tbody>\n</table>\n")
    out.append('<p class="mute" data-no-match hidden>No packages match.</p>\n')
    out.append("</div>\n")
    out.append(_detail_sections(catalog))
    out.append("</main>\n")
    generation = escape_html(catalog.generation)
    out.append(
        f'<footer><span>Generation <code>{generation}</code></span>'
        f'<a href="/generations/{generation}/catalog.json">catalog.json</a>'
        f"<span>Schema version {catalog.schema_version}</span></footer>\n"
    )
    return _document("Cargo registry browser", "".join(out))


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
