from __future__ import annotations

import itertools
from dataclasses import replace
from pathlib import Path
from typing import Any

import pytest
from deps_fixtures import build_graph, instance, project, repository

from repomod import audit
from repomod.dependencies import enrich, world
from repomod.dependencies.model import Component, Ecosystem, Edge, Relation, Resolution, Scope, SourceKind
from repomod.dependencies.ui import Rect, squarify
from repomod.dependencies.world import Origin


@pytest.mark.parametrize(
    ("url", "owner"),
    [
        ("https://github.com/Serde-RS/serde", "github.com/serde-rs"),
        ("git+https://github.com/babel/babel.git", "github.com/babel"),
        ("git@github.com:dtolnay/syn.git", "github.com/dtolnay"),
        ("github:joshuarli/kache", "github.com/joshuarli"),
        ("evanw/esbuild", "github.com/evanw"),
        ("https://www.gitlab.com/group/project/-/tree/main", "gitlab.com/group"),
        ("https://ghcr.io/laputa-systems/image", "github.com/laputa-systems"),
        ("https://www.sqlite.org/index.html", "sqlite.org"),
        ("https://github.com/", None),
        ("not a url", None),
    ],
)
def test_source_owner_reads_forge_owners_and_other_hosts(url: str, owner: str | None) -> None:
    assert world.source_owner(url) == owner


def _direct(target: str) -> Edge:
    return Edge(".:cargo", target, Relation.DIRECT, Scope.RUNTIME, False, None, None, None,
                SourceKind.REGISTRY, Resolution.RESOLVED)


def _world(tmp_path: Path, sources: dict[str, tuple[str, ...]]) -> world.World:
    serde = instance(Ecosystem.CARGO, "serde", "1.0.0")
    mine = instance(Ecosystem.CARGO, "laputa-async", "0.1.0")
    member = instance(Ecosystem.CARGO, "tool-core", "0.1.0", source_kind=SourceKind.WORKSPACE, first_party=True)
    mystery = instance(Ecosystem.CARGO, "mystery", "1.0.0")
    scoped = instance(Ecosystem.NPM, "@laputa-systems/ui-dashboard", "1.0.0")
    action = instance(Ecosystem.GITHUB, "actions/checkout", "v4", source_kind=SourceKind.GIT)
    forked = instance(Ecosystem.CARGO, "only-in-fork", "1.0.0")
    tool = repository(
        "joshuarli/tool",
        (serde, mine, member, mystery, scoped, action),
        projects=(project(".", Ecosystem.CARGO),),
        edges=(_direct(serde.id),),
    )
    tool = replace(
        tool,
        components=tuple(
            Component(item.purl, item.ecosystem, "@laputa-systems/ui-dashboard", "@laputa-systems/ui-dashboard")
            if item.purl == scoped.component
            else item
            for item in tool.components
        ),
    )
    graph = build_graph(
        tmp_path,
        [
            tool,
            repository("laputa-systems/lib", (serde,)),
            repository("joshuarli/upstream", (serde, forked), tags=("fork",)),
        ],
    )
    return world.build(graph, ["joshuarli/tool", "laputa-systems/lib"], sources)


def test_world_separates_your_packages_from_third_party_ones(tmp_path: Path) -> None:
    value = _world(
        tmp_path,
        {
            "pkg:cargo/serde": ("https://serde.rs", "https://github.com/serde-rs/serde"),
            "pkg:cargo/laputa-async": ("https://github.com/laputa-systems/async",),
        },
    )
    found = {package.component.name: package for package in value.packages}

    assert value.owners == {"github.com/joshuarli", "github.com/laputa-systems"}
    assert "only-in-fork" not in found
    assert found["serde"].origin is Origin.THIRD_PARTY
    assert found["serde"].owner == "github.com/serde-rs"
    assert found["serde"].repositories == ("joshuarli/tool", "laputa-systems/lib")
    assert found["serde"].direct
    assert found["laputa-async"].origin is Origin.YOURS
    assert not found["laputa-async"].direct
    assert found["tool-core"].origin is Origin.YOURS
    assert found["tool-core"].in_repo
    assert found["@laputa-systems/ui-dashboard"].origin is Origin.YOURS
    assert found["actions/checkout"].owner == "github.com/actions"
    assert found["mystery"].origin is Origin.UNKNOWN
    assert value.packages[0].component.name == "serde"


def test_squarify_tiles_the_box_without_overlap() -> None:
    box = Rect(0, 0, 300, 200)
    values = [40, 25, 10, 10, 8, 4, 2, 1]
    rects = squarify(values, box)

    for rect, value in zip(rects, values, strict=True):
        assert min(rect.x, rect.y) >= -1e-6
        assert rect.x + rect.w <= box.w + 1e-6
        assert rect.y + rect.h <= box.h + 1e-6
        assert rect.w * rect.h == pytest.approx(value / sum(values) * box.w * box.h)
    for first, second in itertools.combinations(rects, 2):
        overlap_w = min(first.x + first.w, second.x + second.w) - max(first.x, second.x)
        overlap_h = min(first.y + first.h, second.y + second.h) - max(first.y, second.y)
        assert overlap_w <= 1e-6 or overlap_h <= 1e-6


def test_enrich_looks_up_package_sources_once_and_retries_failures(tmp_path: Path) -> None:
    graph = build_graph(
        tmp_path,
        [
            repository(
                "acme/tool",
                (
                    instance(Ecosystem.CARGO, "serde", "1.0.0"),
                    instance(Ecosystem.NPM, "react", "18.0.0"),
                    instance(Ecosystem.GOLANG, "github.com/acme/mod", "v1.0.0"),
                ),
            )
        ],
    )
    cache_path = tmp_path / "cache.json"
    calls: list[tuple[str, str]] = []

    def sources(ecosystem: str, name: str) -> tuple[str, ...]:
        calls.append((ecosystem, name))
        if name == "react":
            raise audit.SourceLookupError("unavailable")
        return ("https://github.com/serde-rs/serde",)

    def latest(_ecosystem: str, _name: str) -> str | None:
        return None

    def osv(queries: list[dict[str, Any]]) -> list[list[dict[str, Any]]]:
        return [[] for _ in queries]

    first = enrich.enrich(
        graph, cache_path=cache_path, now=1000.0, latest_fetcher=latest, osv_fetcher=osv, source_fetcher=sources
    )
    assert sorted(calls) == [("crates", "serde"), ("npm", "react")]
    assert first.sources_fetched == 1
    assert enrich.load_cache(cache_path).sources == {"pkg:cargo/serde": ("https://github.com/serde-rs/serde",)}

    calls.clear()
    enrich.enrich(
        graph, cache_path=cache_path, now=1001.0, latest_fetcher=latest, osv_fetcher=osv, source_fetcher=sources
    )
    assert calls == [("npm", "react")]


def test_crate_sources_come_from_paced_batches(monkeypatch: pytest.MonkeyPatch) -> None:
    requested: list[str] = []
    sleeps: list[float] = []

    def document(url: str, _headers: dict[str, str] | None = None) -> dict[str, Any] | None:
        requested.append(url)
        if len(requested) == 2:
            return None
        return {"crates": [{"id": "serde", "repository": "https://github.com/serde-rs/serde", "homepage": None}]}

    monkeypatch.setattr(audit, "_registry_document", document)
    monkeypatch.setattr(audit, "_CRATES_BATCH", 2)
    monkeypatch.setattr(audit.time, "sleep", sleeps.append)

    found = audit.resolve_source_urls([("crates", "serde"), ("crates", "private-crate"), ("crates", "zzz")])

    assert found == {
        ("crates", "serde"): ("https://github.com/serde-rs/serde",),
        ("crates", "private-crate"): (),
    }
    assert len(requested) == 2
    assert "ids%5B%5D=private-crate" in requested[0]
    assert sleeps == [1]
