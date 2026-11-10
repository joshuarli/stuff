from __future__ import annotations

import socket
from dataclasses import replace
from pathlib import Path

import pytest
from deps_fixtures import instance, issue, project, repository, write_inventory

from repomod.dependencies import diff as diff_module
from repomod.dependencies import graph, render
from repomod.dependencies.model import (
    Component,
    CoverageKind,
    Ecosystem,
    Edge,
    Evidence,
    Instance,
    Relation,
    Resolution,
    Scope,
    SourceKind,
)
from repomod.types import StatusArea, StatusItem, StatusLevel

HOSTILE_PURL = "pkg:cargo/evil</script><script>alert(1)</script>"


@pytest.fixture(autouse=True)
def ui_dashboard(tmp_path_factory: pytest.TempPathFactory, monkeypatch: pytest.MonkeyPatch) -> Path:
    dist = tmp_path_factory.mktemp("ui-dashboard")
    for sheet in render.UI_SHEETS:
        (dist / sheet).parent.mkdir(parents=True, exist_ok=True)
        (dist / sheet).write_text(f".ui-{Path(sheet).stem} {{}}\n", encoding="utf-8")
    monkeypatch.setattr(render, "UI_DASHBOARD", dist)
    return dist


def test_render_copies_the_installed_library_sheets(tmp_path: Path, ui_dashboard: Path) -> None:
    result = _render(tmp_path)
    for sheet in render.UI_SHEETS:
        copied = result.output / "assets" / "ui-dashboard" / Path(sheet).name
        assert copied.read_text(encoding="utf-8") == (ui_dashboard / sheet).read_text(encoding="utf-8")


def test_render_without_the_installed_library_fails_before_writing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(render, "UI_DASHBOARD", tmp_path / "missing")
    inventory, built = _fixture(tmp_path)
    with pytest.raises(FileNotFoundError, match="pnpm install --frozen-lockfile"):
        render.render(inventory, built, tmp_path / "dashboard")
    assert not (tmp_path / "dashboard").exists()


def _fixture(tmp_path: Path) -> tuple[graph.Inventory, graph.Graph]:
    hostile = Instance(
        f"{HOSTILE_PURL}#",
        HOSTILE_PURL,
        HOSTILE_PURL,
        None,
        SourceKind.REGISTRY,
    )
    hostile_edge = Edge(
        ".:cargo",
        hostile.id,
        Relation.DIRECT,
        Scope.RUNTIME,
        False,
        "<script>constraint</script>",
        None,
        "<img src=x onerror=alert(2)>",
        SourceKind.REGISTRY,
        Resolution.RESOLVED,
        (Evidence("<script>path</script>", "<img src=x onerror=alert(3)>"),),
    )
    repositories = [
        repository(
            "acme/hostile",
            (hostile,),
            tags=("tool", "<script>tag</script>"),
            projects=(project(".", Ecosystem.CARGO),),
            issues=(
                issue(
                    CoverageKind.PARSE_ERROR,
                    path="<script>alert(2)</script>",
                    locator="<img src=x onerror=alert(4)>",
                    message="<img src=x onerror=alert(5)>",
                ),
            ),
            edges=(hostile_edge,),
        ),
        repository(
            "acme/empty",
            (),
            projects=(project(".", Ecosystem.GENERIC),),
        ),
        repository(
            "acme/broken",
            (),
            issues=(issue(CoverageKind.CHECKOUT_MISSING, path="Cargo.lock"),),
        ),
    ]
    inventory_path = write_inventory(tmp_path, repositories)
    inventory = graph.load_inventory(inventory_path)
    return inventory, graph.build_graph(inventory)


def _render(tmp_path: Path, name: str = "dashboard") -> render.Dashboard:
    inventory, built = _fixture(tmp_path)
    return render.render(inventory, built, tmp_path / name)


def test_render_escapes_hostile_names_and_paths(tmp_path: Path) -> None:
    result = _render(tmp_path)
    index = (result.output / "index.html").read_text(encoding="utf-8")
    packages = (result.output / "packages.html").read_text(encoding="utf-8")
    coverage = (result.output / "coverage.html").read_text(encoding="utf-8")
    hostile = (result.output / "repos/acme/hostile/index.html").read_text(encoding="utf-8")
    assert "<script>alert(1)</script>" not in index
    assert "<script>alert(1)</script>" not in packages
    assert "&lt;script&gt;alert(1)&lt;/script&gt;" in packages
    assert "&lt;script&gt;alert(2)&lt;/script&gt;" in hostile
    assert "<script>alert(2)</script>" not in coverage
    assert "&lt;script&gt;" in hostile
    search = (result.output / "assets/search.js").read_text(encoding="utf-8")
    assert "</script>" not in search
    assert "\\u003c/script\\u003e" in search


def test_assets_do_not_fetch(tmp_path: Path) -> None:
    result = _render(tmp_path)
    for name in (
        "app.js",
        "search.js",
        "app.css",
        "ui-dashboard/styles.css",
        "ui-dashboard/tokens.css",
        "ui-dashboard/layout.css",
        "ui-dashboard/data.css",
    ):
        content = (result.output / "assets" / name).read_text(encoding="utf-8")
        assert "fetch(" not in content


def test_stylesheets_load_the_base_before_the_family_sheets(tmp_path: Path) -> None:
    result = _render(tmp_path)
    index = (result.output / "index.html").read_text(encoding="utf-8")
    order = [
        index.index("ui-dashboard/styles.css"),
        index.index("ui-dashboard/layout.css"),
        index.index("ui-dashboard/data.css"),
        index.index("assets/app.css"),
    ]
    assert order == sorted(order)


def test_render_does_not_open_sockets(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    inventory, built = _fixture(tmp_path)

    def blocked(*arguments: object, **keywords: object) -> None:
        raise AssertionError("network access is not allowed")

    monkeypatch.setattr(socket.socket, "connect", blocked)
    monkeypatch.setattr(socket, "create_connection", blocked)

    result = render.render(inventory, built, tmp_path / "dashboard")
    assert (result.output / "index.html").is_file()


def test_zero_dependencies_render_differently_from_unknown(tmp_path: Path) -> None:
    result = _render(tmp_path)
    empty = (result.output / "repos/acme/empty/index.html").read_text(encoding="utf-8")
    broken = (result.output / "repos/acme/broken/index.html").read_text(encoding="utf-8")
    packages = (result.output / "packages.html").read_text(encoding="utf-8")
    assert "complete: zero dependencies" in empty
    assert 'class="zero"' in empty
    assert "unavailable" in broken
    assert "no dependency edges recorded" in broken
    assert 'class="unknown"' in broken
    assert "not checked" in packages
    assert "no known vulnerabilities" not in packages


def test_missing_registry_data_is_not_reported_as_zero_findings(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(render.enrich, "load_cache", lambda: render.enrich.Enrichment(tmp_path / "cache", None, {}))
    result = _render(tmp_path)
    index = (result.output / "index.html").read_text(encoding="utf-8")
    assert "not checked yet" in index
    assert '<span class="tile-value">—</span>' in index
    assert '<span class="tile-value">0</span><span class="tile-note">not checked' not in index


def test_every_repository_page_links_back(tmp_path: Path) -> None:
    result = _render(tmp_path)
    pages = sorted((result.output / "repos").glob("*/*/index.html"))
    assert len(pages) == 3
    for page in pages:
        content = page.read_text(encoding="utf-8")
        assert "../../../index.html" in content
        assert 'data-page="repository"' in content


def test_package_index_and_routing_markup(tmp_path: Path) -> None:
    result = _render(tmp_path)
    packages = (result.output / "packages.html").read_text(encoding="utf-8")
    assert 'data-page="packages"' in packages
    assert 'id="package-index"' in packages
    assert 'data-purl="pkg:cargo/' in packages
    assert "evil" in packages


def test_changes_page_reports_no_baseline(tmp_path: Path) -> None:
    result = _render(tmp_path)
    changes = (result.output / "changes.html").read_text(encoding="utf-8")
    assert "no baseline" in changes


def test_refuses_to_replace_a_non_dashboard_directory(tmp_path: Path) -> None:
    inventory, built = _fixture(tmp_path)
    other = tmp_path / "other"
    other.mkdir()
    (other / "keep.txt").write_text("keep", encoding="utf-8")
    with pytest.raises(ValueError, match="not a dashboard"):
        render.render(inventory, built, other)
    assert (other / "keep.txt").read_text(encoding="utf-8") == "keep"


def test_output_stays_under_the_budget(tmp_path: Path) -> None:
    result = _render(tmp_path)
    assert result.bytes < 10_000_000


def test_insight_pages_escape_package_and_role_names(tmp_path: Path) -> None:
    name = "evil<img src=x onerror=alert(6)>"
    repositories = []
    for owner, version in (("acme/one", "1.0.0"), ("acme/two", "2.0.0")):
        hostile = instance(Ecosystem.CARGO, name, version)
        built = repository(
                owner,
                (hostile,),
                projects=(project(".", Ecosystem.CARGO),),
                edges=(
                    Edge(
                        ".:cargo",
                        hostile.id,
                        Relation.DIRECT,
                        Scope.RUNTIME,
                        False,
                        None,
                        version,
                        None,
                        SourceKind.REGISTRY,
                        Resolution.RESOLVED,
                    ),
                ),
            )
        repositories.append(
            replace(
                built,
                components=(Component(hostile.component, Ecosystem.CARGO, name, name),),
            )
        )
    inventory = graph.load_inventory(write_inventory(tmp_path, repositories))
    result = render.render(inventory, graph.build_graph(inventory), tmp_path / "dashboard")
    for page in ("alignment.html",):
        content = (result.output / page).read_text(encoding="utf-8")
        assert "<img src=x" not in content
        assert "evil&lt;img src=x onerror=alert(6)&gt;" in content


def test_fork_repositories_are_listed_apart_from_owned_ones(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    vulnerable = {"pkg:cargo/clap@4.0.0": {"vulns": [{"id": "RUSTSEC-1"}]}}
    cache = render.enrich.Enrichment(tmp_path / "cache", 1.0, vulnerable)
    monkeypatch.setattr(render.enrich, "load_cache", lambda: cache)
    repositories = []
    for name, tags in (("acme/mine", ("tool",)), ("acme/upstream", ("fork", "tool"))):
        clap = instance(Ecosystem.CARGO, "clap", "4.0.0")
        repositories.append(
            repository(
                name,
                (clap,),
                tags=tags,
                projects=(project(".", Ecosystem.CARGO),),
                edges=(
                    Edge(
                        ".:cargo",
                        clap.id,
                        Relation.DIRECT,
                        Scope.RUNTIME,
                        False,
                        None,
                        "4.0.0",
                        None,
                        SourceKind.REGISTRY,
                        Resolution.RESOLVED,
                    ),
                ),
            )
        )
    inventory = graph.load_inventory(write_inventory(tmp_path, repositories))
    result = render.render(inventory, graph.build_graph(inventory), tmp_path / "dashboard")
    choices = (result.output / "choices.html").read_text(encoding="utf-8")
    security = (result.output / "security.html").read_text(encoding="utf-8")
    assert "acme/mine" in choices
    assert "acme/upstream" not in choices
    forks = security[security.index("<h2>Forks</h2>"):]
    assert "acme/upstream" in forks
    assert "acme/mine" not in forks


def test_heaviest_dependencies_mark_dev_only_with_a_pill(tmp_path: Path) -> None:
    syn = instance(Ecosystem.CARGO, "syn", "2.0.0")
    proc_macro2 = instance(Ecosystem.CARGO, "proc-macro2", "1.0.0")
    clap = instance(Ecosystem.CARGO, "clap", "4.0.0")
    clap_derive = instance(Ecosystem.CARGO, "clap_derive", "4.0.0")
    repo = repository(
        "acme/app",
        (syn, proc_macro2, clap, clap_derive),
        projects=(project(".", Ecosystem.CARGO),),
        edges=(
            Edge(
                ".:cargo",
                syn.id,
                Relation.DIRECT,
                Scope.DEV,
                False,
                None,
                "2.0.0",
                None,
                SourceKind.REGISTRY,
                Resolution.RESOLVED,
            ),
            Edge(
                syn.id,
                proc_macro2.id,
                Relation.TRANSITIVE,
                Scope.DEV,
                False,
                None,
                "1.0.0",
                None,
                SourceKind.REGISTRY,
                Resolution.RESOLVED,
            ),
            Edge(
                ".:cargo",
                clap.id,
                Relation.DIRECT,
                Scope.RUNTIME,
                False,
                None,
                "4.0.0",
                None,
                SourceKind.REGISTRY,
                Resolution.RESOLVED,
            ),
            Edge(
                clap.id,
                clap_derive.id,
                Relation.TRANSITIVE,
                Scope.RUNTIME,
                False,
                None,
                "4.0.0",
                None,
                SourceKind.REGISTRY,
                Resolution.RESOLVED,
            ),
        ),
    )
    inventory = graph.load_inventory(write_inventory(tmp_path, [repo]))
    result = render.render(inventory, graph.build_graph(inventory), tmp_path / "dashboard")
    bloat = (result.output / "bloat.html").read_text(encoding="utf-8")
    assert "syn@2.0.0" in bloat
    assert 'ui-badge-neutral">dev</span>' in bloat
    assert "clap@4.0.0" in bloat
    syn_row = bloat.split("syn@2.0.0", 1)[1].split("</li>", 1)[0]
    clap_row = bloat.split("clap@4.0.0", 1)[1].split("</li>", 1)[0]
    assert 'ui-badge-neutral">dev</span>' in syn_row
    assert 'ui-badge-neutral">dev</span>' not in clap_row
    assert "Only via it" in bloat


def test_choices_show_which_repositories_deviate_from_preferred(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    lexopt = instance(Ecosystem.CARGO, "lexopt", "0.3.0")
    clap = instance(Ecosystem.CARGO, "clap", "4.0.0")
    clap_complete = instance(Ecosystem.CARGO, "clap_complete", "4.0.0")
    repos = [
        repository(
            "acme/clean",
            (lexopt,),
            projects=(project(".", Ecosystem.CARGO),),
            edges=(
                Edge(
                    ".:cargo",
                    lexopt.id,
                    Relation.DIRECT,
                    Scope.RUNTIME,
                    False,
                    None,
                    "0.3.0",
                    None,
                    SourceKind.REGISTRY,
                    Resolution.RESOLVED,
                ),
            ),
        ),
        repository(
            "acme/legacy",
            (clap,),
            projects=(project(".", Ecosystem.CARGO),),
            edges=(
                Edge(
                    ".:cargo",
                    clap.id,
                    Relation.DIRECT,
                    Scope.RUNTIME,
                    False,
                    None,
                    "4.0.0",
                    None,
                    SourceKind.REGISTRY,
                    Resolution.RESOLVED,
                ),
            ),
        ),
        repository(
            "acme/mixed",
            (lexopt, clap_complete),
            projects=(project(".", Ecosystem.CARGO),),
            edges=(
                Edge(
                    ".:cargo",
                    lexopt.id,
                    Relation.DIRECT,
                    Scope.RUNTIME,
                    False,
                    None,
                    "0.3.0",
                    None,
                    SourceKind.REGISTRY,
                    Resolution.RESOLVED,
                ),
                Edge(
                    ".:cargo",
                    clap_complete.id,
                    Relation.DIRECT,
                    Scope.RUNTIME,
                    False,
                    None,
                    "4.0.0",
                    None,
                    SourceKind.REGISTRY,
                    Resolution.RESOLVED,
                ),
            ),
        ),
    ]
    inventory = graph.load_inventory(write_inventory(tmp_path, repos))
    built = graph.build_graph(inventory)
    roles_path = tmp_path / "dependency-roles.toml"
    roles_path.write_text(
        '[cli-arguments]\n'
        'ecosystem = "cargo"\n'
        'preferred = "lexopt"\n'
        '[cli-arguments.options]\n'
        'lexopt = ["lexopt"]\n'
        'clap = ["clap", "clap_complete"]\n',
        encoding="utf-8",
    )
    (tmp_path / "action-pins.toml").write_text("", encoding="utf-8")
    monkeypatch.chdir(tmp_path)
    result = render.render(inventory, built, tmp_path / "dashboard")
    choices = (result.output / "choices.html").read_text(encoding="utf-8")
    section = choices.split('id="role-cli-arguments"', 1)[1].split("<h2", 1)[0]
    assert "deviate from preferred" not in section
    assert section.count('class="mono">acme/legacy<') == 1
    assert section.count('class="mono">acme/mixed<') == 1
    assert "acme/clean" not in section
    assert "clap_complete" in section


def test_alignment_orders_versions_oldest_to_newest_and_marks_the_target(tmp_path: Path) -> None:
    repositories = []
    for owner, version in (("acme/new", "1.10.0"), ("acme/old", "1.9.0")):
        package = instance(Ecosystem.CARGO, "serde", version)
        repositories.append(
            repository(
                owner,
                (package,),
                projects=(project(".", Ecosystem.CARGO),),
                edges=(
                    Edge(
                        ".:cargo",
                        package.id,
                        Relation.DIRECT,
                        Scope.RUNTIME,
                        False,
                        None,
                        version,
                        None,
                        SourceKind.REGISTRY,
                        Resolution.RESOLVED,
                    ),
                ),
            )
        )
    inventory = graph.load_inventory(write_inventory(tmp_path, repositories))
    result = render.render(inventory, graph.build_graph(inventory), tmp_path / "dashboard")
    alignment = (result.output / "alignment.html").read_text(encoding="utf-8")
    lane = alignment.split('class="lane"', 1)[1].split("</section>", 1)[0]
    assert lane.index(">1.9.0<") < lane.index(">1.10.0<")
    assert lane.count("stop-target") == 1
    assert lane.index("stop-target") > lane.index(">1.9.0<")


def test_coverage_shows_fixable_issues_and_only_counts_known_limits(tmp_path: Path) -> None:
    repositories = [
        repository(
            "acme/mine",
            (),
            issues=(
                issue(CoverageKind.MISSING_LOCK, path="Cargo.toml"),
                issue(CoverageKind.UNSUPPORTED_FORMAT, path="weird.lock"),
            ),
        ),
        repository(
            "acme/upstream",
            (),
            tags=("fork",),
            issues=(issue(CoverageKind.MISSING_LOCK, path="Cargo.toml"),),
        ),
    ]
    inventory = graph.load_inventory(write_inventory(tmp_path, repositories))
    result = render.render(inventory, graph.build_graph(inventory), tmp_path / "dashboard")
    coverage = (result.output / "coverage.html").read_text(encoding="utf-8")
    main = coverage[coverage.index('id="main"'):]
    assert "No lockfile" in main
    assert "mine" in main
    assert "upstream" not in main
    assert "weird.lock" not in main
    assert "1 files in formats with no parser" in main


def test_coverage_lists_source_risk_items_and_leaves_out_forks(tmp_path: Path) -> None:
    artifact = Instance(
        "pkg:generic/tool.tar.gz?download_url=https%3A%2F%2Fexample.com%2Ftool.tar.gz#",
        "pkg:generic/tool.tar.gz?download_url=https%3A%2F%2Fexample.com%2Ftool.tar.gz",
        "pkg:generic/tool.tar.gz?download_url=https%3A%2F%2Fexample.com%2Ftool.tar.gz",
        None,
        SourceKind.URL,
    )
    checkout = instance(
        Ecosystem.GITHUB,
        "actions/checkout",
        "v4",
        source_kind=SourceKind.GIT,
        vcs_url="https://github.com/actions/checkout",
    )
    image = instance(Ecosystem.DOCKER, "python", "3.13-slim", source_kind=SourceKind.IMAGE)
    vendored = instance(Ecosystem.CARGO, "bitflags", None, source_kind=SourceKind.VENDORED)
    repositories = [
        repository(
            "acme/mine",
            (checkout, image, vendored, artifact),
            issues=(issue(CoverageKind.MISSING_LOCK, path="Cargo.toml"),),
        ),
        repository(
            "acme/upstream",
            (checkout,),
            tags=("fork",),
            issues=(issue(CoverageKind.MISSING_LOCK, path="Cargo.toml"),),
        ),
    ]
    inventory = graph.load_inventory(write_inventory(tmp_path, repositories))
    result = render.render(inventory, graph.build_graph(inventory), tmp_path / "dashboard")
    coverage = (result.output / "coverage.html").read_text(encoding="utf-8")
    main = coverage[coverage.index('id="main"'):]
    assert "actions/checkout" in main
    assert "3.13-slim" in main
    assert "bitflags" in main
    assert "Cargo.toml" in main
    assert "https://example.com/tool.tar.gz" in main
    assert "upstream" not in main


def _page_text(result: render.Dashboard, *parts: str) -> str:
    return result.output.joinpath(*parts).read_text(encoding="utf-8")


def test_status_appears_on_repositories_and_the_repository_page(tmp_path: Path) -> None:
    inventory, built = _fixture(tmp_path)
    result = render.render(
        inventory,
        built,
        tmp_path / "dashboard",
        status={
            "acme/empty": (
                StatusItem(StatusArea.RELEASE, "v1.2.3 · 7 since", StatusLevel.ATTENTION),
            ),
            "acme/broken": (
                StatusItem(
                    StatusArea.WORKFLOWS,
                    "FAIL build",
                    StatusLevel.FAILING,
                    "https://github.com/acme/broken/actions/runs/1",
                ),
            ),
            "acme/hostile": (),
        },
    )
    repositories = _page_text(result, "repositories.html")
    assert repositories.index("FAIL build") < repositories.index("v1.2.3 · 7 since")
    assert 'href="https://github.com/acme/broken/actions/runs/1"' in repositories
    assert "not collected" not in repositories
    assert "FAIL build" in _page_text(result, "repos", "acme", "broken", "index.html")
    assert "nothing flagged" in _page_text(result, "repos", "acme", "hostile", "index.html")


def test_status_labels_and_urls_cannot_inject_markup(tmp_path: Path) -> None:
    inventory, built = _fixture(tmp_path)
    result = render.render(
        inventory,
        built,
        tmp_path / "dashboard",
        status={
            "acme/empty": (
                StatusItem(
                    StatusArea.LOCAL,
                    "dirty (1) on <script>alert(1)</script>",
                    StatusLevel.ATTENTION,
                    "javascript:alert(2)",
                ),
            ),
        },
    )
    repositories = _page_text(result, "repositories.html")
    assert "<script>alert(1)" not in repositories
    assert "javascript:alert(2)" not in repositories


def test_uncollected_status_is_reported_rather_than_shown_as_clean(tmp_path: Path) -> None:
    result = _render(tmp_path)
    assert "not collected" in _page_text(result, "repositories.html")
    assert 'id="status"' not in _page_text(result, "repos", "acme", "empty", "index.html")


def test_only_running_status_items_animate(tmp_path: Path) -> None:
    inventory, built = _fixture(tmp_path)
    running = StatusItem(StatusArea.WORKFLOWS, "RUN release", StatusLevel.ATTENTION, running=True)
    failed = StatusItem(StatusArea.WORKFLOWS, "FAIL build", StatusLevel.FAILING)
    result = render.render(
        inventory, built, tmp_path / "dashboard", status={"acme/empty": (running,), "acme/broken": (failed,)}
    )
    assert "dot-running" in _page_text(result, "repos", "acme", "empty", "index.html")
    assert "dot-running" not in _page_text(result, "repos", "acme", "broken", "index.html")


def test_hidden_repositories_are_counted_and_left_out_of_changes(tmp_path: Path) -> None:
    inventory, built = _fixture(tmp_path)
    change = diff_module.RepositoryDiff(
        "acme/secret",
        "changed",
        "complete",
        "complete",
        "a" * 40,
        "b" * 40,
        (),
        (),
        (diff_module.VersionChange("pkg:cargo/hidden", "hidden-pkg", Ecosystem.CARGO, "2", "1", "downgrade"),),
        (),
        (),
        (),
    )
    result = render.render(
        inventory,
        built,
        tmp_path / "dashboard",
        diff_value=diff_module.Diff(True, "a", "b", (change,)),
        hidden=frozenset({"acme/secret"}),
    )
    assert "1 private hidden" in _page_text(result, "repositories.html")
    assert "hidden-pkg" not in _page_text(result, "changes.html")


def test_excluding_drops_a_repository_from_the_graph(tmp_path: Path) -> None:
    inventory, _ = _fixture(tmp_path)
    remaining = graph.build_graph(graph.excluding(inventory, frozenset({"acme/hostile"})))
    assert "acme/hostile" not in remaining.repositories
    assert HOSTILE_PURL not in remaining.components
