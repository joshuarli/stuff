from __future__ import annotations

import json
import time
import tomllib
from collections.abc import Callable, Iterable, Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any, cast

from repomod.audit import (
    LockedDependency,
    RegistrySources,
    resolve_latest,
    resolve_vulns,
)
from repomod.catalog import atomic_write
from repomod.dependencies.graph import Graph
from repomod.dependencies.model import Ecosystem, RepositoryData
from repomod.io import cache_file

CACHE_NAME = "deps-enrichment.json"
MAX_AGE_SECONDS = 86400
_CHECKED = frozenset({Ecosystem.CARGO, Ecosystem.PYPI, Ecosystem.NPM, Ecosystem.GOLANG})
_ECOSYSTEMS = {
    Ecosystem.CARGO: "crates",
    Ecosystem.PYPI: "pypi",
    Ecosystem.NPM: "npm",
    Ecosystem.GOLANG: "go",
}


@dataclass(frozen=True, slots=True)
class Enrichment:
    path: Path
    fetched_at: float | None
    results: Mapping[str, Mapping[str, object]]


@dataclass(frozen=True, slots=True)
class EnrichReport:
    cache: Enrichment
    fetched: int
    checked: int
    stale: int
    not_checked: int
    vulnerable: int
    total: int


@dataclass(frozen=True, slots=True)
class _Item:
    purl: str
    ecosystem: str
    name: str
    version: str


def load_cache(path: Path | None = None) -> Enrichment:
    target = path if path is not None else cache_file(CACHE_NAME)
    try:
        document = json.loads(target.read_text(encoding="utf-8"))
    except (FileNotFoundError, json.JSONDecodeError, UnicodeError):
        return Enrichment(target, None, {})
    if not isinstance(document, dict):
        return Enrichment(target, None, {})
    fetched_at = document.get("fetched_at")
    raw_results = document.get("results")
    if not isinstance(raw_results, dict):
        return Enrichment(target, None, {})
    results: dict[str, Mapping[str, object]] = {
        purl: cast(Mapping[str, object], entry)
        for purl, entry in raw_results.items()
        if isinstance(purl, str) and isinstance(entry, dict)
    }
    return Enrichment(
        target,
        float(fetched_at) if isinstance(fetched_at, (int, float)) and not isinstance(fetched_at, bool) else None,
        results,
    )


def is_fresh(entry: Mapping[str, object], now: float) -> bool:
    fetched_at = entry.get("fetched_at")
    if not isinstance(fetched_at, (int, float)) or isinstance(fetched_at, bool):
        return False
    return now - float(fetched_at) < MAX_AGE_SECONDS


def enrich(
    graph: Graph,
    repositories: Iterable[str] | None = None,
    *,
    offline: bool = False,
    refresh: bool = False,
    cache_path: Path | None = None,
    now: float | None = None,
    latest_fetcher: Callable[[str, str], str | None] | None = None,
    osv_fetcher: Callable[[list[dict[str, Any]]], list[list[dict[str, Any]]]] | None = None,
    sources: RegistrySources | None = None,
) -> EnrichReport:
    current = time.time() if now is None else now
    cache = load_cache(cache_path)
    selected = {name.casefold() for name in repositories} if repositories is not None else None
    items = _items(graph, selected)
    pending = [
        item
        for item in items
        if not offline
        and (
            refresh
            or item.purl not in cache.results
            or not is_fresh(cache.results[item.purl], current)
        )
    ]
    registry = sources if sources is not None else registry_sources()
    latest_map: dict[tuple[str, str], str | None] = {}
    vulns_map: dict[tuple[str, str, str], list[dict[str, Any]]] = {}
    if pending:
        latest_items: dict[tuple[str, str], LockedDependency] = {}
        for item in pending:
            key = (item.ecosystem, item.name)
            latest_items.setdefault(
                key, LockedDependency(item.ecosystem, item.name, item.version, True)
            )
        latest_map = resolve_latest(
            list(latest_items.values()),
            offline=False,
            refresh=True,
            cache={},
            now=current,
            fetcher=latest_fetcher,
            sources=registry,
        )
        vulns_map = resolve_vulns(
            [
                LockedDependency(item.ecosystem, item.name, item.version, True)
                for item in pending
            ],
            offline=False,
            refresh=True,
            cache={},
            now=current,
            osv_fetcher=osv_fetcher,
        )
    results: dict[str, Mapping[str, object]] = dict(cache.results)
    for item in pending:
        results[item.purl] = {
            "latest": latest_map.get((item.ecosystem, item.name)),
            "latest_source": _source_for(registry, item.ecosystem),
            "vulns": vulns_map.get((item.ecosystem, item.name, item.version), []),
            "fetched_at": current,
        }
    fetched_at = current if pending else cache.fetched_at
    if pending:
        _write(cache.path, {"fetched_at": fetched_at, "results": results})
    checked = stale = not_checked = vulnerable = 0
    total = 0
    for data in _selected_data(graph, selected):
        components = {component.purl: component for component in data.components}
        for instance in data.instances:
            total += 1
            component = components.get(instance.component)
            if (
                component is None
                or component.ecosystem not in _CHECKED
                or instance.version is None
            ):
                not_checked += 1
                continue
            entry = results.get(instance.purl)
            if entry is None:
                not_checked += 1
            elif not is_fresh(entry, current):
                stale += 1
            else:
                checked += 1
                vulns = entry.get("vulns")
                if isinstance(vulns, list) and vulns:
                    vulnerable += 1
    return EnrichReport(
        Enrichment(cache.path, fetched_at, results),
        len(pending),
        checked,
        stale,
        not_checked,
        vulnerable,
        total,
    )


def _selected_data(
    graph: Graph, selected: set[str] | None
) -> tuple[RepositoryData, ...]:
    return tuple(
        data
        for name, data in graph.repositories.items()
        if selected is None or name.casefold() in selected
    )


def _items(graph: Graph, selected: set[str] | None) -> tuple[_Item, ...]:
    items: dict[str, _Item] = {}
    for name, data in graph.repositories.items():
        if selected is not None and name.casefold() not in selected:
            continue
        components = {component.purl: component for component in data.components}
        for instance in data.instances:
            component = components.get(instance.component)
            if component is None or component.ecosystem not in _CHECKED:
                continue
            if instance.version is None:
                continue
            ecosystem = _ECOSYSTEMS[component.ecosystem]
            items.setdefault(
                instance.purl,
                _Item(instance.purl, ecosystem, component.name, instance.version),
            )
    return tuple(sorted(items.values(), key=lambda item: item.purl))


def _source_for(sources: RegistrySources, ecosystem: str) -> str:
    if ecosystem == "crates":
        return sources.cargo or "public"
    if ecosystem == "npm":
        return sources.npm or "public"
    if ecosystem == "pypi":
        return sources.python or "public"
    return "https://proxy.golang.org"


def _write(path: Path, value: Mapping[str, object]) -> None:
    content = json.dumps(value, indent=1, sort_keys=True, ensure_ascii=False) + "\n"
    atomic_write(path, content)


def registry_sources() -> RegistrySources:
    path = Path(__file__).resolve().parents[3] / "registries.toml"
    try:
        document = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError):
        return RegistrySources(None, None, None)
    cargo = _table(document.get("cargo"))
    npm = _table(document.get("npm"))
    python = _table(document.get("python"))
    return RegistrySources(
        cargo.get("source") if isinstance(cargo.get("source"), str) else None,
        npm.get("registry") if isinstance(npm.get("registry"), str) else None,
        python.get("index") if isinstance(python.get("index"), str) else None,
    )


def _table(value: object) -> Mapping[str, Any]:
    return cast(Mapping[str, Any], value) if isinstance(value, dict) else {}


def summary_text(report: EnrichReport) -> str:
    return (
        f"{report.checked} checked, {report.stale} stale, "
        f"{report.not_checked} not checked, {report.vulnerable} vulnerable "
        f"of {report.total} instances; fetched {report.fetched} entries "
        f"to {report.cache.path}"
    )
