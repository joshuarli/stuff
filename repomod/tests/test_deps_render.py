from __future__ import annotations

import socket
from dataclasses import replace
from pathlib import Path

import pytest
from deps_fixtures import instance, issue, project, repository, write_inventory

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

HOSTILE_PURL = "pkg:cargo/evil</script><script>alert(1)</script>"


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
                    CoverageKind.DYNAMIC_VALUE,
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
    for name in ("app.js", "search.js", "app.css"):
        content = (result.output / "assets" / name).read_text(encoding="utf-8")
        assert "fetch(" not in content


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
    assert "packages.html#pkg=" in packages
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
    for page in ("index.html", "alignment.html"):
        content = (result.output / page).read_text(encoding="utf-8")
        assert "<img src=x" not in content
        assert "evil&lt;img src=x onerror=alert(6)&gt;" in content


def test_fork_repositories_are_listed_apart_from_owned_ones(tmp_path: Path) -> None:
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
    index = (result.output / "index.html").read_text(encoding="utf-8")
    assert "acme/mine" in choices
    assert "acme/upstream" not in choices
    forks = index[index.index("<h2>Forks</h2>"):]
    assert "acme/upstream" in forks
    assert "acme/mine" not in forks
