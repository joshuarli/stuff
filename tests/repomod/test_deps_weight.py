from __future__ import annotations

import json
import re
import shutil
import subprocess
from dataclasses import replace
from pathlib import Path
from typing import Any

import pytest
from deps_fixtures import build_graph, instance, project, repository, write_inventory

from repomod.audit import PackageSize
from repomod.dependencies import enrich, graph, render, weight, weight_view, world
from repomod.dependencies.model import Component, Ecosystem, Edge, Relation, Resolution, Scope, SourceKind

ASSET = Path(render.ASSETS) / "weight.js"
OWNED = ["joshuarli/tool", "joshuarli/other"]


def _direct(source: str, target: str) -> Edge:
    return Edge(source, target, Relation.DIRECT, Scope.RUNTIME, False, None, None, None,
                SourceKind.REGISTRY, Resolution.RESOLVED)


def _edge(
    source: str, target: str, *, scope: Scope = Scope.RUNTIME, optional: bool = False, direct: bool = False
) -> Edge:
    return Edge(source, target, Relation.DIRECT if direct else Relation.TRANSITIVE, scope, optional, None, None, None,
                SourceKind.REGISTRY, Resolution.RESOLVED)


def _size(bytes_: int | None, kind: str) -> dict[str, Any]:
    return {"size": {"bytes": bytes_, "kind": kind}}


def _fixture(tmp_path: Path):
    react = instance(Ecosystem.NPM, "react", "18.0.0", context="locks/a")
    react_again = instance(Ecosystem.NPM, "react", "18.0.0", context="locks/b")
    old_react = instance(Ecosystem.NPM, "react", "17.0.0", context="locks/b")
    loose = instance(Ecosystem.NPM, "loose-envify", "1.4.0", context="locks/a")
    bare = instance(Ecosystem.NPM, "bare", "1.0.0", context="locks/a")
    mine = instance(Ecosystem.NPM, "@joshuarli/ui", "1.0.0", context="locks/a")
    serde = instance(Ecosystem.CARGO, "serde", "1.0.0")
    core = instance(Ecosystem.CARGO, "tool-core", "0.1.0", source_kind=SourceKind.WORKSPACE, first_party=True)
    requests = instance(Ecosystem.PYPI, "requests", "2.0.0")
    gomod = instance(Ecosystem.GOLANG, "example.com/mod", "v1.0.0")
    action = instance(Ecosystem.GITHUB, "actions/checkout", "v4", source_kind=SourceKind.GIT)
    tool = repository(
        "joshuarli/tool",
        (react, react_again, old_react, loose, bare, mine, serde, core, requests, gomod, action),
        projects=(project("locks/a", Ecosystem.NPM), project("locks/b", Ecosystem.NPM), project(".", Ecosystem.CARGO)),
        edges=(
            _direct("locks/b:npm", react_again.id),
            _direct(".:cargo", serde.id),
        ),
    )
    tool = replace(
        tool,
        components=tuple(
            Component(item.purl, item.ecosystem, "@joshuarli/ui", "@joshuarli/ui")
            if item.purl == mine.component
            else item
            for item in tool.components
        ),
    )
    other = repository(
        "joshuarli/other",
        (instance(Ecosystem.CARGO, "serde", "1.0.0"), instance(Ecosystem.CARGO, "serde", "1.0.1")),
        projects=(project(".", Ecosystem.CARGO),),
    )
    pretty = instance(Ecosystem.CARGO, "pretty", "1.0.0")
    devonly = repository(
        "joshuarli/devonly",
        (pretty,),
        projects=(project(".", Ecosystem.CARGO),),
        edges=(_edge(".:cargo", pretty.id, scope=Scope.DEV, direct=True),),
    )
    nodeps = repository("joshuarli/nodeps", ())
    fork = repository("acme/fork", (instance(Ecosystem.NPM, "only-in-fork", "1.0.0"),), tags=("fork",))
    built = build_graph(tmp_path, [tool, other, devonly, nodeps, fork])
    results = {
        "pkg:npm/react@18.0.0": _size(3000, "unpacked"),
        "pkg:npm/react@17.0.0": _size(2000, "unpacked"),
        "pkg:npm/loose-envify@1.4.0": _size(100, "unpacked"),
        "pkg:npm/bare@1.0.0": _size(None, "unpacked"),
        "pkg:npm/%40joshuarli/ui@1.0.0": _size(9999, "unpacked"),
        "pkg:cargo/serde@1.0.0": _size(800, "crate"),
        "pkg:cargo/serde@1.0.1": _size(850, "crate"),
        "pkg:cargo/pretty@1.0.0": _size(500, "crate"),
        "pkg:pypi/requests@2.0.0": _size(70, "wheel"),
    }
    return built, results


def _run(row: weight.Repository) -> Any:
    return row.totals[weight.DEFAULT_VARIANT]


def _weights(tmp_path: Path) -> dict[str, weight.Repository]:
    built, results = _fixture(tmp_path)
    value = world.build(built, OWNED, {}, {})
    return {row.repository: row for row in weight.build(built, OWNED, results, value)}


def _platform_fixture(tmp_path: Path):
    def npm(name: str) -> Any:
        return instance(Ecosystem.NPM, name, "1.0.0", context="locks/a")

    names = (
        "app", "tsc", "esb-darwin", "esb-x64-musl", "esb-x64-gnu", "esb-arm64-musl", "not-windows", "not-darwin",
        "native", "mixed", "dev-helper", "shared", "linux-or-mac",
    )
    found = {name: npm(name) for name in names}
    root = "locks/a:npm"
    edges = [
        _edge(root, found["app"].id, direct=True),
        _edge(root, found["tsc"].id, scope=Scope.DEV, direct=True),
        *(
            _edge(found["app"].id, found[name].id, scope=Scope.OPTIONAL, optional=True)
            for name in ("esb-darwin", "esb-x64-musl", "esb-x64-gnu", "esb-arm64-musl", "not-windows", "not-darwin",
                         "mixed", "linux-or-mac")
        ),
        _edge(found["app"].id, found["native"].id),
        _edge(found["app"].id, found["shared"].id),
        _edge(found["tsc"].id, found["mixed"].id),
        _edge(found["tsc"].id, found["dev-helper"].id),
        _edge(found["tsc"].id, found["shared"].id),
    ]
    plat = repository(
        "joshuarli/plat", tuple(found.values()), projects=(project("locks/a", Ecosystem.NPM),), edges=edges
    )
    built = build_graph(tmp_path, [plat])
    allowed = {
        "esb-darwin": {"os": ("darwin",), "cpu": ("arm64",)},
        "esb-x64-musl": {"os": ("linux",), "cpu": ("x64",), "libc": ("musl",)},
        "esb-x64-gnu": {"os": ("linux",), "cpu": ("x64",), "libc": ("glibc",)},
        "esb-arm64-musl": {"os": ("linux",), "cpu": ("arm64",), "libc": ("musl",)},
        "not-windows": {"os": ("!win32",)},
        "not-darwin": {"os": ("!darwin",)},
        "native": {"os": ("win32",)},
        "mixed": {"os": ("win32",)},
        "linux-or-mac": {"os": ("linux", "darwin", "!win32")},
    }
    results = {}
    for index, (name, item) in enumerate(found.items()):
        platform = {"os": (), "cpu": (), "libc": (), **allowed.get(name, {})}
        results[item.purl] = {"size": _size_record(PackageSize(100 * (index + 1), "unpacked", None, None, **platform))}
    value = world.build(built, ["joshuarli/plat"], {}, {})
    return weight.build(built, ["joshuarli/plat"], results, value)[0]


def _size_record(size: PackageSize) -> dict[str, Any]:
    return {"bytes": size.bytes, "kind": size.kind, "os": list(size.os or ()), "cpu": list(size.cpu or ()),
            "libc": list(size.libc or ())}


def _counted(row: weight.Repository, platform: str, dev: bool = False) -> set[str]:
    return {package.name for package in row.heaviest if package.counts((platform, dev))}


_RUNTIME = {"app", "esb-darwin", "not-windows", "native", "mixed", "shared", "linux-or-mac"}


def test_optional_packages_that_exclude_the_platform_are_not_counted(tmp_path: Path) -> None:
    row = _platform_fixture(tmp_path)

    assert _counted(row, "darwin-arm64") == _RUNTIME
    assert _counted(row, "linux-x64-musl") == {
        "app", "esb-x64-musl", "not-windows", "not-darwin", "native", "mixed", "shared", "linux-or-mac",
    }
    assert _counted(row, "linux-arm64-musl") == {
        "app", "esb-arm64-musl", "not-windows", "not-darwin", "native", "mixed", "shared", "linux-or-mac",
    }
    assert row.total(["npm"], ("darwin-arm64", False)).packages == len(_RUNTIME)
    assert row.total(["npm"], ("darwin-arm64", False)).bytes == sum(
        package.bytes or 0 for package in row.heaviest if package.name in _RUNTIME
    )


def test_all_platforms_counts_every_optional_package(tmp_path: Path) -> None:
    row = _platform_fixture(tmp_path)

    assert _counted(row, "all") == _counted(row, "all", True) - {"tsc", "dev-helper"}
    assert {"esb-x64-gnu", "esb-darwin", "not-darwin"} <= _counted(row, "all")
    assert row.total(["npm"], ("all", True)).packages == 13


def test_non_optional_packages_count_on_every_platform_whatever_their_os_field(tmp_path: Path) -> None:
    row = _platform_fixture(tmp_path)

    for platform in ("darwin-arm64", "linux-x64-musl", "linux-arm64-musl", "all"):
        assert {"native", "mixed"} <= _counted(row, platform)


def test_dev_only_packages_follow_transitive_edges_and_need_the_toggle(tmp_path: Path) -> None:
    row = _platform_fixture(tmp_path)
    by_name = {package.name: package for package in row.heaviest}

    assert by_name["tsc"].dev_only
    assert by_name["dev-helper"].dev_only
    assert not by_name["shared"].dev_only
    assert not by_name["mixed"].dev_only
    assert not by_name["app"].dev_only
    assert _counted(row, "darwin-arm64", True) == _RUNTIME | {"tsc", "dev-helper"}
    assert "tsc" not in _counted(row, "all")
    assert row.total(["npm"], ("darwin-arm64", True)).packages == len(_RUNTIME) + 2


@pytest.mark.parametrize(
    ("platform", "lists", "expected"),
    [
        ("darwin-arm64", {"os": ("darwin",), "cpu": ("arm64",)}, True),
        ("darwin-arm64", {"os": ("linux",)}, False),
        ("darwin-arm64", {"os": ("darwin",), "cpu": ("x64",)}, False),
        ("darwin-arm64", {"os": ("!darwin",)}, False),
        ("darwin-arm64", {"os": ("!win32",)}, True),
        ("darwin-arm64", {"os": ("!win32", "!linux")}, True),
        ("linux-x64-musl", {"os": ("linux", "!win32")}, True),
        ("linux-x64-musl", {"os": ("!linux",)}, False),
        ("linux-x64-musl", {"os": ("linux",), "cpu": ("x64",), "libc": ("musl",)}, True),
        ("linux-x64-musl", {"os": ("linux",), "libc": ("glibc",)}, False),
        ("linux-x64-musl", {"libc": ("!musl",)}, False),
        ("linux-x64-musl", {"libc": ("!glibc",)}, True),
        ("darwin-arm64", {"libc": ("glibc",)}, True),
        ("linux-arm64-musl", {"cpu": ("x64", "arm64")}, True),
        ("linux-arm64-musl", {"cpu": ("x64",)}, False),
        ("darwin-arm64", {}, True),
    ],
)
def test_npm_platform_lists_follow_npm_semantics(
    platform: str, lists: dict[str, tuple[str, ...]], expected: bool
) -> None:
    target = next(item for item in weight.PLATFORMS if item.key == platform)
    size = PackageSize(1, "unpacked", None, None, **{"os": (), "cpu": (), "libc": (), **lists})

    assert weight.npm_allows(target, size) is expected
    assert weight.npm_allows(target, None)


def test_weight_counts_each_third_party_version_once_per_repository(tmp_path: Path) -> None:
    tool = _weights(tmp_path)["joshuarli/tool"]

    assert _run(tool)["npm"] == weight.Total(bytes=5100, packages=4, unknown=1)
    assert _run(tool)["cargo"] == weight.Total(bytes=800, packages=1, unknown=0)
    assert _run(tool)["pypi"] == weight.Total(bytes=70, packages=1, unknown=0)


def test_weight_counts_unsized_ecosystems_as_unknown(tmp_path: Path) -> None:
    tool = _weights(tmp_path)["joshuarli/tool"]

    assert _run(tool)["go"] == weight.Total(bytes=0, packages=1, unknown=1)
    assert _run(tool)["other"] == weight.Total(bytes=0, packages=1, unknown=1)
    assert tool.total(["go", "other"]) == weight.Total(bytes=0, packages=2, unknown=2)


def test_weight_leaves_out_your_packages_and_forks(tmp_path: Path) -> None:
    rows = _weights(tmp_path)
    names = {package.name for package in rows["joshuarli/tool"].heaviest}

    assert "@joshuarli/ui" not in names
    assert "tool-core" not in names
    assert "acme/fork" not in rows
    assert rows["joshuarli/tool"].total(["npm"]).packages == 4


def test_weight_keeps_versions_apart_and_marks_direct_anywhere_in_the_repository(tmp_path: Path) -> None:
    rows = _weights(tmp_path)
    tool = {(package.name, package.version): package for package in rows["joshuarli/tool"].heaviest}
    other = rows["joshuarli/other"]

    assert tool[("react", "18.0.0")].direct
    assert not tool[("react", "17.0.0")].direct
    assert not tool[("loose-envify", "1.4.0")].direct
    assert tool[("serde", "1.0.0")].direct
    assert _run(other)["cargo"] == weight.Total(bytes=1650, packages=2, unknown=0)
    assert not any(package.direct for package in other.heaviest)


def test_weight_lists_the_heaviest_sized_packages_first_within_a_language(tmp_path: Path) -> None:
    npm = [package for package in _weights(tmp_path)["joshuarli/tool"].heaviest if package.language == "npm"]

    assert [(package.name, package.bytes, package.kind) for package in npm] == [
        ("react", 3000, "unpacked"),
        ("react", 2000, "unpacked"),
        ("loose-envify", 100, "unpacked"),
    ]


def test_weight_without_cached_sizes_is_all_unknown(tmp_path: Path) -> None:
    built, _results = _fixture(tmp_path)
    value = world.build(built, OWNED, {}, {})
    tool = next(row for row in weight.build(built, OWNED, {}, value) if row.repository == "joshuarli/tool")

    assert _run(tool)["npm"].bytes == 0
    assert _run(tool)["npm"].unknown == _run(tool)["npm"].packages == 4
    assert tool.heaviest == ()


@pytest.mark.parametrize(
    ("value", "text"),
    [
        (0, "0 B"),
        (999, "999 B"),
        (1000, "1.0 kB"),
        (1234, "1.2 kB"),
        (9949, "9.9 kB"),
        (9950, "10 kB"),
        (123_456, "123 kB"),
        (999_499, "999 kB"),
        (999_500, "1.0 MB"),
        (31_200_000, "31 MB"),
        (2_500_000_000, "2.5 GB"),
    ],
)
def test_format_bytes_uses_decimal_units_with_three_digits(value: int, text: str) -> None:
    assert weight.format_bytes(value) == text


@pytest.fixture
def cache_path(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    _built, results = _fixture(tmp_path / "fixture")
    path = tmp_path / "cache.json"
    path.write_text(json.dumps({"fetched_at": 1.0, "results": results, "sources": {}, "mirror": {}}), encoding="utf-8")
    monkeypatch.setattr(enrich, "cache_file", lambda _name: path)
    return path


@pytest.fixture(autouse=True)
def ui_dashboard(tmp_path_factory: pytest.TempPathFactory, monkeypatch: pytest.MonkeyPatch) -> Path:
    dist = tmp_path_factory.mktemp("ui-dashboard")
    for sheet in render.UI_SHEETS:
        (dist / sheet).parent.mkdir(parents=True, exist_ok=True)
        (dist / sheet).write_text(f".ui-{Path(sheet).stem} {{}}\n", encoding="utf-8")
    monkeypatch.setattr(render, "UI_DASHBOARD", dist)
    return dist


def _render(tmp_path: Path) -> render.Dashboard:
    root = tmp_path / "inventory"
    _fixture(root)
    inventory = graph.load_inventory(root / "data" / "dependencies" / "v1")
    return render.render(inventory, graph.build_graph(inventory), tmp_path / "dashboard")


def test_weight_page_embeds_per_language_data_and_toggles(tmp_path: Path, cache_path: Path) -> None:
    page = (_render(tmp_path).output / "weight.html").read_text(encoding="utf-8")

    assert 'data-page="weight"' in page
    assert '<a href="weight.html" aria-current="page">Weight</a>' in page
    assert '<a href="weight.html">Weight</a>' in (tmp_path / "dashboard" / "bloat.html").read_text(encoding="utf-8")
    for key in ("npm", "cargo", "pypi", "go", "other"):
        assert f'data-lang-toggle="{key}"' in page
        assert f'data-lang-show="{key}"' in page
    for label in ("TypeScript/JavaScript", "Rust", "Python", "Go", "Other"):
        assert label in page
    assert page.count("data-lang-toggle=") == 5
    assert page.count('aria-pressed="true"') == 6
    assert page.count("data-platform-toggle=") == 4
    for platform in ("darwin-arm64", "linux-x64-musl", "linux-arm64-musl", "all"):
        assert f'data-platform-toggle="{platform}"' in page
    assert 'aria-pressed="true" data-platform-toggle="darwin-arm64"' in page
    assert 'aria-pressed="false" data-dev-toggle' in page
    assert "Include dev dependencies" in page
    assert "All platforms (upper bound)" in page
    row = re.search(r'<li class="weight-row"[^>]*data-repo="joshuarli/tool"[^>]*>', page)
    assert row is not None
    for attribute in (
        'data-t-darwin-arm64-run-npm="5100,4,1"', 'data-t-all-dev-npm="5100,4,1"',
        'data-t-linux-x64-musl-run-cargo="800,1,0"', 'data-t-darwin-arm64-run-pypi="70,1,0"',
        'data-t-darwin-arm64-run-go="0,1,1"', 'data-t-darwin-arm64-run-other="0,1,1"',
    ):
        assert attribute in row.group()
    assert 'src="assets/weight.js"' in page
    assert "fetch(" not in (tmp_path / "dashboard" / "assets" / "weight.js").read_text(encoding="utf-8")


def test_weight_page_collapses_repositories_without_packages_into_one_line(tmp_path: Path, cache_path: Path) -> None:
    page = (_render(tmp_path).output / "weight.html").read_text(encoding="utf-8")
    chart = page.split("data-weight-list")[1].split("Table view")[0]

    assert re.search(r'<li class="weight-row"[^>]*data-repo="joshuarli/devonly"[^>]* hidden>', chart)
    assert re.search(r'<li class="weight-row"[^>]*data-repo="joshuarli/nodeps"[^>]* hidden>', chart)
    assert not re.search(r'<li class="weight-row"[^>]*data-repo="joshuarli/tool"[^>]* hidden>', chart)
    assert (
        '<p class="weight-empty" data-empty-line>'
        "2 repositories have no third-party dependencies: joshuarli/devonly, joshuarli/nodeps.</p>"
    ) in chart
    devonly = re.search(r'<li class="weight-row"[^>]*data-repo="joshuarli/devonly"[^>]*>', chart)
    assert devonly is not None
    assert 'data-t-darwin-arm64-run-cargo="0,0,0"' in devonly.group()
    assert 'data-t-darwin-arm64-dev-cargo="500,1,0"' in devonly.group()
    table = page.split("Table view")[1]
    assert 'data-weight-table-row data-repo="joshuarli/devonly"' in table
    assert 'data-weight-table-row data-repo="joshuarli/nodeps"' in table


def test_weight_page_orders_bars_by_total_and_labels_them_in_text(tmp_path: Path, cache_path: Path) -> None:
    page = (_render(tmp_path).output / "weight.html").read_text(encoding="utf-8")
    bars = page.split('data-weight-list')[1].split("</ul>")[0]

    assert bars.index('data-repo="joshuarli/tool"') < bars.index('data-repo="joshuarli/other"')
    assert re.search(r'data-repo="joshuarli/tool".*?data-value[^>]*>6\.0 kB<', bars, re.DOTALL)
    assert re.search(r'data-repo="joshuarli/other".*?data-value[^>]*>1\.7 kB<', bars, re.DOTALL)
    assert "--r:1.0000" in bars
    assert 'data-tip="joshuarli/tool: 6.0 kB (TS/JS 5.1 kB, Rust 800 B, Python 70 B), ' in page


def test_weight_page_states_the_measure_of_each_language(tmp_path: Path, cache_path: Path) -> None:
    page = (_render(tmp_path).output / "weight.html").read_text(encoding="utf-8")

    assert "npm unpacked size, the installed size" in page
    assert "crates.io .crate archive, compressed" in page
    assert "wheel download (macOS arm64, else Linux, else pure Python) or the sdist" in page
    assert "different measures" in page
    assert "count only" in page


def test_weight_page_lists_heaviest_packages_with_role_and_a_table_view(tmp_path: Path, cache_path: Path) -> None:
    page = (_render(tmp_path).output / "weight.html").read_text(encoding="utf-8")
    tool = page.split('data-repo="joshuarli/tool"')[1].split("</li>")[0]

    assert tool.index(">react<") < tool.index(">loose-envify<")
    assert 'data-lang="npm" data-bytes="3000"' in tool
    assert "<td>direct</td>" in tool
    assert "transitive" in tool
    assert "unpacked" in tool
    assert "crate archive" in tool
    assert "@joshuarli/ui" not in page.split("Table view")[0]
    assert "Table view" in page
    assert 'data-weight-table-row data-repo="joshuarli/tool"' in page
    assert "data-total-bytes>6.0 kB<" in page


def test_weight_page_without_sizes_says_to_enrich(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(enrich, "cache_file", lambda _name: tmp_path / "none.json")
    page = (_render(tmp_path).output / "weight.html").read_text(encoding="utf-8")

    assert "Package sizes are not looked up yet" in page
    assert "deps enrich" in page


def test_weight_page_escapes_repository_and_package_names(tmp_path: Path, cache_path: Path) -> None:
    hostile = instance(Ecosystem.NPM, "evil<img src=x onerror=alert(1)>", "1.0.0")
    inventory = graph.load_inventory(write_inventory(tmp_path, [repository("acme/<b>x</b>", (hostile,))]))
    page = (render.render(inventory, graph.build_graph(inventory), tmp_path / "out").output / "weight.html").read_text(
        encoding="utf-8"
    )

    assert "<b>x</b>" not in page
    assert "<img src=x" not in page
    assert "acme/&lt;b&gt;x&lt;/b&gt;" in page


def test_repository_page_links_its_totals_to_the_weight_page(tmp_path: Path, cache_path: Path) -> None:
    result = _render(tmp_path)
    page = (result.output / "repos/joshuarli/tool/index.html").read_text(encoding="utf-8")

    assert "Dependency weight" in page
    assert 'href="../../../weight.html#repo=joshuarli/tool"' in page
    assert "<strong>5.1 kB</strong>" in page
    assert "<strong>800 B</strong>" in page
    assert "1 package · size unknown" in page
    fork = (result.output / "repos/acme/fork/index.html").read_text(encoding="utf-8")
    assert "Dependency weight" not in fork


def _node(cases: dict[str, Any]) -> dict[str, Any]:
    script = """
const vm = require('node:vm'), fs = require('node:fs');
const context = { window: {}, document: { querySelector: () => null }, URLSearchParams };
vm.runInNewContext(fs.readFileSync(process.argv[1], 'utf8'), context);
const w = context.window.depsWeight, c = JSON.parse(fs.readFileSync(0, 'utf8')), out = {};
out.bytes = c.bytes.map((n) => w.formatBytes(n));
out.parse = c.hashes.map((h) => w.parseHash(h));
out.format = c.views.map((v) => w.formatHash(v));
out.order = c.orders.map((o) => w.order(o.rows, o.view));
out.sums = c.orders.map((o) => o.rows.map((r) => w.summarize(r, o.view)));
out.tips = c.tips.map((t) => w.tipText(t.row, t.view));
out.text = c.sums.map((s) => [w.valueText(s), w.noteText(s)]);
out.empty = c.empties.map((e) => w.emptyText(e.rows, e.view, e.names));
out.cells = c.cells.map((e) => w.cellText(e.lang, e.sum));
process.stdout.write(JSON.stringify(out));
"""
    done = subprocess.run(
        ["node", "-e", script, str(ASSET)], input=json.dumps(cases), capture_output=True, text=True, check=True
    )
    return json.loads(done.stdout)


LANGS = ["npm", "cargo", "pypi", "go", "other"]


def _totals(**langs: tuple[int, int, int]) -> dict[str, dict[str, int]]:
    zero = (0, 0, 0)
    return {
        key: dict(zip(("bytes", "packages", "unknown"), langs.get(key, zero), strict=True))
        for key in LANGS
    }


def _repo(name: str, **variants: dict[str, dict[str, int]]) -> dict[str, Any]:
    return {"repo": name, "totals": {key.replace("_", "-"): value for key, value in variants.items()}}


def _view(
    langs: list[str], platform: str = "darwin-arm64", dev: bool = False, repo: str | None = None
) -> dict[str, Any]:
    return {"langs": langs, "platform": platform, "dev": dev, "repo": repo}


def _model(row: dict[str, Any]) -> weight.Repository:
    zero = {lang: weight.Total(0, 0, 0) for lang in LANGS}
    totals = dict.fromkeys(weight.VARIANTS, zero)
    for key, value in row["totals"].items():
        platform, _, scope = key.rpartition("-")
        totals[(platform, scope == "dev")] = {lang: weight.Total(**t) for lang, t in value.items()}
    return weight.Repository(row["repo"], totals, ())


@pytest.mark.skipif(shutil.which("node") is None, reason="node is not installed")
def test_browser_filter_logic_matches_the_python_model() -> None:
    run = "darwin_arm64_run"
    rows = [
        _repo("b/small", **{run: _totals(npm=(10, 1, 0), cargo=(500, 2, 0))}),
        _repo("a/big", **{run: _totals(npm=(9000, 90, 4), go=(0, 30, 30)), "all_dev": _totals(npm=(12000, 99, 4))}),
        _repo("c/gonly", **{run: _totals(go=(0, 5, 5))}),
        _repo("d/empty", **{run: _totals()}),
        _repo("A/tie", **{run: _totals(cargo=(500, 2, 0))}),
        _repo("e/devonly", **{run: _totals(), "darwin_arm64_dev": _totals(cargo=(70, 3, 0))}),
    ]
    tip_row = _repo(
        "joshuarli/tool",
        **{run: _totals(npm=(5100, 4, 1), cargo=(800, 1, 0), pypi=(70, 1, 0), go=(0, 1, 1))},
    )
    everything = _view(LANGS)
    views = {
        "default": everything,
        "npm": _view(["npm"]),
        "go": _view(["go"]),
        "cargo_go": _view(["cargo", "go"]),
        "all_dev": _view(LANGS, "all", True),
        "dev": _view(LANGS, dev=True),
        "other_platform": _view(LANGS, "linux-x64-musl"),
    }
    sums = [(0, 0, 0), (0, 3, 3), (999_500, 1, 0), (5, 1, 0), (10, 2, 1)]
    empties: list[dict[str, Any]] = [
        {"rows": rows, "view": everything, "names": 12},
        {"rows": rows, "view": _view(["npm"]), "names": 12},
        {"rows": rows, "view": _view(["npm"]), "names": 2},
        {"rows": rows, "view": _view(["cargo", "go"]), "names": 12},
        {"rows": rows, "view": views["dev"], "names": 12},
        {"rows": rows[:2], "view": views["all_dev"], "names": 12},
        {"rows": [rows[3]], "view": _view(["npm"]), "names": 12},
    ]
    cases: dict[str, Any] = {
        "bytes": [0, 7, 999, 1000, 9949, 9950, 123_456, 999_499, 999_500, 31_200_000, 2_500_000_000],
        "hashes": ["", "#", "#lang=npm", "#lang=cargo,npm", "#lang=nope", "#lang=", "#lang=go&repo=joshuarli/tool",
                   "#repo=a%2Fb", "#lang=npm,npm,pypi", "#platform=linux-x64-musl", "#platform=all&dev=1",
                   "#platform=nope", "#dev=0", "#lang=npm&platform=darwin-arm64&dev=1&repo=a/b"],
        "views": [
            everything,
            _view(["npm"]),
            _view(["npm", "pypi"], repo="joshuarli/pi-build"),
            _view(LANGS, repo="a b/c"),
            _view(["npm"], "darwin-arm64"),
            _view(LANGS, "linux-arm64-musl"),
            _view(LANGS, "all", True),
            _view(["npm"], "linux-x64-musl", True, "a/b"),
            _view(LANGS, dev=True),
        ],
        "orders": [{"rows": rows, "view": view} for view in views.values()],
        "tips": [
            {"row": tip_row, "view": everything},
            {"row": tip_row, "view": _view(["npm"])},
            {"row": tip_row, "view": _view(["go"])},
            {"row": tip_row, "view": _view(LANGS, "all")},
            {"row": _repo("d/empty", **{run: _totals()}), "view": _view(["npm"])},
        ],
        "sums": [{"bytes": b, "packages": p, "unknown": u} for b, p, u in sums],
        "empties": empties,
        "cells": [{"lang": lang, "sum": {"bytes": b, "packages": p, "unknown": u}}
                  for lang in ("npm", "go") for b, p, u in sums],
    }

    out = _node(cases)

    assert out["bytes"] == [weight.format_bytes(n) for n in cases["bytes"]]
    assert [view["langs"] for view in out["parse"]] == [
        LANGS, LANGS, ["npm"], ["npm", "cargo"], LANGS, LANGS, ["go"], LANGS, ["npm", "pypi"],
        LANGS, LANGS, LANGS, LANGS, ["npm"],
    ]
    assert [view["platform"] for view in out["parse"]] == (
        ["darwin-arm64"] * 9 + ["linux-x64-musl", "all"] + ["darwin-arm64"] * 3
    )
    assert [view["dev"] for view in out["parse"]] == [False] * 10 + [True, False, False, True]
    repos = [None, None, None, None, None, None, "joshuarli/tool", "a/b", None, None, None, None, None, "a/b"]
    assert [view["repo"] for view in out["parse"]] == repos
    assert out["format"] == [
        "", "lang=npm", "lang=npm,pypi&repo=joshuarli/pi-build", "repo=a%20b/c", "lang=npm",
        "platform=linux-arm64-musl", "platform=all&dev=1", "lang=npm&platform=linux-x64-musl&dev=1&repo=a/b",
        "dev=1",
    ]
    order = dict(zip(views, out["order"], strict=True))
    assert order["default"] == ["a/big", "b/small", "A/tie", "c/gonly", "d/empty", "e/devonly"]
    assert order["npm"] == ["a/big", "b/small", "A/tie", "c/gonly", "d/empty", "e/devonly"]
    assert order["go"] == ["a/big", "c/gonly", "A/tie", "b/small", "d/empty", "e/devonly"]
    assert order["cargo_go"] == ["A/tie", "b/small", "a/big", "c/gonly", "d/empty", "e/devonly"]
    assert order["all_dev"] == ["a/big", "A/tie", "b/small", "c/gonly", "d/empty", "e/devonly"]
    assert order["dev"] == ["e/devonly", "a/big", "A/tie", "b/small", "c/gonly", "d/empty"]
    assert order["other_platform"] == ["a/big", "A/tie", "b/small", "c/gonly", "d/empty", "e/devonly"]
    sums_by_view = dict(zip(views, out["sums"], strict=True))
    assert sums_by_view["npm"][0] == {"bytes": 10, "packages": 1, "unknown": 0}
    assert sums_by_view["go"][1] == {"bytes": 0, "packages": 30, "unknown": 30}
    assert sums_by_view["cargo_go"][0] == {"bytes": 500, "packages": 2, "unknown": 0}
    assert sums_by_view["all_dev"][1] == {"bytes": 12000, "packages": 99, "unknown": 4}
    assert sums_by_view["dev"][5] == {"bytes": 70, "packages": 3, "unknown": 0}
    assert sums_by_view["default"][5] == {"bytes": 0, "packages": 0, "unknown": 0}
    assert sums_by_view["other_platform"][0] == {"bytes": 0, "packages": 0, "unknown": 0}
    text = [(weight_view.value_text(weight.Total(*s)), weight_view.note_text(weight.Total(*s))) for s in sums]
    assert [tuple(pair) for pair in out["text"]] == text
    assert out["tips"] == [
        weight_view.tip_text(
            _model(case["row"]), case["view"]["langs"], (case["view"]["platform"], case["view"]["dev"])
        )
        for case in cases["tips"]
    ]
    expected = [
        "2 repositories have no third-party dependencies: d/empty, e/devonly.",
        "4 repositories have no TS/JS dependencies: A/tie, c/gonly, d/empty, e/devonly.",
        "4 repositories have no TS/JS dependencies: A/tie, c/gonly and 2 more.",
        "2 repositories have no Rust or Go dependencies: d/empty, e/devonly.",
        "5 repositories have no third-party dependencies: a/big, A/tie, b/small, c/gonly, d/empty.",
        "1 repository has no third-party dependencies: b/small.",
        "1 repository has no TS/JS dependencies: d/empty.",
    ]
    assert out["empty"] == expected
    assert out["empty"] == [
        weight_view.empty_text(
            [_model(row) for row in case["rows"]],
            case["view"]["langs"],
            (case["view"]["platform"], case["view"]["dev"]),
            case["names"],
        )
        for case in empties
    ]
    assert out["cells"] == [
        list(weight_view.cell_text(lang != "go", weight.Total(*s))) for lang in ("npm", "go") for s in sums
    ]
