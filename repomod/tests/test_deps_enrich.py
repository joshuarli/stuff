from __future__ import annotations

import argparse
import json
import socket
import time
from pathlib import Path
from typing import Any

import pytest
from deps_fixtures import build_graph, instance, repository

from repomod.cmd import common, deps
from repomod.dependencies import enrich
from repomod.dependencies.model import Ecosystem, SourceKind


def _fixture_graph(tmp_path: Path):
    return build_graph(
        tmp_path,
        [
            repository(
                "acme/tool",
                (
                    instance(Ecosystem.CARGO, "serde", "1.0.0"),
                    instance(Ecosystem.PYPI, "requests", "2.0.0"),
                    instance(Ecosystem.NPM, "react", "18.0.0"),
                    instance(Ecosystem.GOLANG, "example.com/mod", "v1.0.0"),
                    instance(Ecosystem.GENERIC, "rust", "1.90.0", source_kind=SourceKind.URL),
                ),
            )
        ],
    )


def _latest(_ecosystem: str, name: str) -> str | None:
    return {
        "serde": "1.0.219",
        "requests": "2.32.0",
        "react": "19.0.0",
        "example.com/mod": "v1.1.0",
    }.get(name)


def _osv(queries: list[dict[str, Any]]) -> list[list[dict[str, Any]]]:
    return [
        [{"id": f"OSV-{query['package']['name']}"}] for query in queries
    ]


def test_fetches_latest_versions_and_vulnerabilities(tmp_path: Path) -> None:
    graph = _fixture_graph(tmp_path)
    cache_path = tmp_path / "cache.json"

    report = enrich.enrich(
        graph,
        cache_path=cache_path,
        now=1000.0,
        latest_fetcher=_latest,
        osv_fetcher=_osv,
    )

    assert report.fetched == 4
    assert report.checked == 4
    assert report.vulnerable == 4
    assert report.not_checked == 1
    assert report.total == 5
    document = json.loads(cache_path.read_text(encoding="utf-8"))
    assert document["fetched_at"] == 1000.0
    serde = document["results"]["pkg:cargo/serde@1.0.0"]
    assert serde["latest"] == "1.0.219"
    assert serde["vulns"] == [{"id": "OSV-serde"}]
    assert serde["fetched_at"] == 1000.0
    assert "pkg:generic/rust@1.90.0" not in document["results"]


def test_fresh_cache_is_not_refetched(tmp_path: Path) -> None:
    graph = _fixture_graph(tmp_path)
    cache_path = tmp_path / "cache.json"
    enrich.enrich(
        graph, cache_path=cache_path, now=1000.0, latest_fetcher=_latest, osv_fetcher=_osv
    )

    def unexpected(ecosystem: str, name: str) -> str | None:
        raise AssertionError("fresh cache must not fetch")

    report = enrich.enrich(
        graph,
        cache_path=cache_path,
        now=1001.0,
        latest_fetcher=unexpected,
        osv_fetcher=lambda queries: (_ for _ in ()).throw(AssertionError("no OSV")),
    )

    assert report.fetched == 0
    assert report.checked == 4


def test_stale_cache_is_refetched(tmp_path: Path) -> None:
    graph = _fixture_graph(tmp_path)
    cache_path = tmp_path / "cache.json"
    enrich.enrich(
        graph, cache_path=cache_path, now=1000.0, latest_fetcher=_latest, osv_fetcher=_osv
    )

    report = enrich.enrich(
        graph,
        cache_path=cache_path,
        now=1000.0 + enrich.MAX_AGE_SECONDS + 1,
        latest_fetcher=lambda ecosystem, name: "9.9.9",
        osv_fetcher=_osv,
    )

    assert report.fetched == 4
    document = json.loads(cache_path.read_text(encoding="utf-8"))
    assert document["results"]["pkg:cargo/serde@1.0.0"]["latest"] == "9.9.9"


def test_refresh_forces_refetch(tmp_path: Path) -> None:
    graph = _fixture_graph(tmp_path)
    cache_path = tmp_path / "cache.json"
    enrich.enrich(
        graph, cache_path=cache_path, now=1000.0, latest_fetcher=_latest, osv_fetcher=_osv
    )

    report = enrich.enrich(
        graph,
        cache_path=cache_path,
        refresh=True,
        now=1001.0,
        latest_fetcher=lambda ecosystem, name: "2.0.0",
        osv_fetcher=_osv,
    )

    assert report.fetched == 4
    document = json.loads(cache_path.read_text(encoding="utf-8"))
    assert document["results"]["pkg:cargo/serde@1.0.0"]["latest"] == "2.0.0"


def test_offline_uses_the_cache_without_network(tmp_path: Path) -> None:
    graph = _fixture_graph(tmp_path)
    cache_path = tmp_path / "cache.json"
    enrich.enrich(
        graph, cache_path=cache_path, now=1000.0, latest_fetcher=_latest, osv_fetcher=_osv
    )

    def unexpected(ecosystem: str, name: str) -> str | None:
        raise AssertionError("offline mode must not fetch")

    report = enrich.enrich(
        graph,
        cache_path=cache_path,
        offline=True,
        now=1001.0,
        latest_fetcher=unexpected,
        osv_fetcher=lambda queries: (_ for _ in ()).throw(AssertionError("no OSV")),
    )

    assert report.fetched == 0
    assert report.checked == 4


def test_offline_without_a_cache_writes_nothing(tmp_path: Path) -> None:
    graph = _fixture_graph(tmp_path)
    cache_path = tmp_path / "cache.json"

    report = enrich.enrich(graph, cache_path=cache_path, offline=True, now=1000.0)

    assert report.fetched == 0
    assert report.checked == 0
    assert report.not_checked == 5
    assert not cache_path.exists()


def test_malformed_cache_is_ignored(tmp_path: Path) -> None:
    graph = _fixture_graph(tmp_path)
    cache_path = tmp_path / "cache.json"
    cache_path.write_text("{", encoding="utf-8")

    report = enrich.enrich(
        graph, cache_path=cache_path, now=1000.0, latest_fetcher=_latest, osv_fetcher=_osv
    )

    assert report.fetched == 4
    assert report.checked == 4


def test_enrich_cli_reads_the_cache_offline(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    graph = _fixture_graph(tmp_path)
    cache_path = tmp_path / "cache.json"
    enrich.enrich(
        graph,
        cache_path=cache_path,
        now=time.time(),
        latest_fetcher=_latest,
        osv_fetcher=_osv,
    )
    catalog = _catalog(tmp_path)
    inventory = tmp_path / "data" / "dependencies" / "v1"
    monkeypatch.setattr(deps, "_inventory", lambda: inventory)
    monkeypatch.setattr(common, "default_catalog", lambda: catalog)
    monkeypatch.setattr(enrich, "cache_file", lambda name: cache_path)

    args = argparse.Namespace(operation="enrich", offline=True, refresh=False, json=True)
    assert deps._deps(args) == 0
    value = json.loads(capsys.readouterr().out)
    assert value["checked"] == 4
    assert value["not_checked"] == 1
    assert value["total"] == 5


def _catalog(tmp_path: Path):
    from repomod.catalog import load_catalog

    path = tmp_path / "catalog.toml"
    path.write_text(
        "version = 1\n\n[workspace]\nroot = \"~/repos\"\npersonal_owner = \"acme\"\n\n"
        '[repos."acme"]\ntool = ["tool"]\n',
        encoding="utf-8",
    )
    return load_catalog(path, {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path / "fleet")})


def test_offline_enrich_does_not_open_sockets(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    graph = _fixture_graph(tmp_path)
    cache_path = tmp_path / "cache.json"
    enrich.enrich(
        graph,
        cache_path=cache_path,
        now=time.time(),
        latest_fetcher=_latest,
        osv_fetcher=_osv,
    )

    def blocked(*arguments: object, **keywords: object) -> None:
        raise AssertionError("network access is not allowed")

    monkeypatch.setattr(socket.socket, "connect", blocked)
    monkeypatch.setattr(socket, "create_connection", blocked)

    report = enrich.enrich(
        graph, cache_path=cache_path, offline=True, now=time.time()
    )
    assert report.checked == 4


def test_legacy_rustsec_cache_is_refreshed() -> None:
    from repomod.dependencies.enrich import is_fresh

    old = {"fetched_at": 1000.0, "vulns": [{"id": "RUSTSEC-2026-0441"}]}
    classified = {"fetched_at": 1000.0, "vulns": [{"id": "RUSTSEC-2026-0001", "affected": []}]}
    assert not is_fresh(old, 1000.0)
    assert is_fresh(classified, 1000.0)
