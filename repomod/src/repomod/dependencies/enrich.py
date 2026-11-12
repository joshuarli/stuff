from __future__ import annotations

import json
import time
import tomllib
from collections.abc import Callable, Iterable, Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any, cast

from repomod.audit import (
    CrateRecord,
    LockedDependency,
    MirrorCatalogError,
    RegistryRateLimitedError,
    RegistrySources,
    VulnerabilityLookupError,
    read_mirror_catalog,
    resolve_crates,
    resolve_latest,
    resolve_source_urls,
    resolve_vulns,
)
from repomod.catalog import atomic_write
from repomod.dependencies.graph import Graph
from repomod.dependencies.model import Ecosystem, RepositoryData
from repomod.io import cache_file

CACHE_NAME = "deps-enrichment.json"
MAX_AGE_SECONDS = 86400
_CHECKED = frozenset({Ecosystem.CARGO, Ecosystem.PYPI, Ecosystem.NPM, Ecosystem.GOLANG})
_SOURCED = frozenset({"crates", "pypi", "npm"})
_MIRRORED = (Ecosystem.CARGO, Ecosystem.NPM, Ecosystem.PYPI)
_ECOSYSTEMS = {
    Ecosystem.CARGO: "crates",
    Ecosystem.PYPI: "pypi",
    Ecosystem.NPM: "npm",
    Ecosystem.GOLANG: "go",
}


@dataclass(frozen=True, slots=True)
class MirrorCatalog:
    url: str
    fetched_at: float
    packages: Mapping[str, tuple[str, ...]]


@dataclass(frozen=True, slots=True)
class Enrichment:
    path: Path
    fetched_at: float | None
    results: Mapping[str, Mapping[str, object]]
    sources: Mapping[str, tuple[str, ...]]
    mirror: Mapping[Ecosystem, MirrorCatalog]


@dataclass(frozen=True, slots=True)
class EnrichReport:
    cache: Enrichment
    fetched: int
    checked: int
    stale: int
    not_checked: int
    vulnerable: int
    total: int
    sources_fetched: int
    errors: tuple[str, ...] = ()


@dataclass(frozen=True, slots=True)
class _Item:
    purl: str
    component: str
    ecosystem: str
    name: str
    version: str


def load_cache(path: Path | None = None) -> Enrichment:
    target = path if path is not None else cache_file(CACHE_NAME)
    try:
        document = json.loads(target.read_text(encoding="utf-8"))
    except (FileNotFoundError, json.JSONDecodeError, UnicodeError):
        return Enrichment(target, None, {}, {}, {})
    if not isinstance(document, dict):
        return Enrichment(target, None, {}, {}, {})
    fetched_at = document.get("fetched_at")
    raw_results = document.get("results")
    if not isinstance(raw_results, dict):
        return Enrichment(target, None, {}, {}, {})
    results: dict[str, Mapping[str, object]] = {
        purl: cast(Mapping[str, object], entry)
        for purl, entry in raw_results.items()
        if isinstance(purl, str) and isinstance(entry, dict)
    }
    raw_sources = document.get("sources")
    sources = {
        purl: tuple(url for url in urls if isinstance(url, str))
        for purl, urls in (raw_sources.items() if isinstance(raw_sources, dict) else ())
        if isinstance(purl, str) and isinstance(urls, list)
    }
    return Enrichment(
        target,
        float(fetched_at) if isinstance(fetched_at, (int, float)) and not isinstance(fetched_at, bool) else None,
        results,
        sources,
        _mirror(document.get("mirror")),
    )


def _mirror(value: object) -> dict[Ecosystem, MirrorCatalog]:
    catalogs: dict[Ecosystem, MirrorCatalog] = {}
    for ecosystem in _MIRRORED:
        entry = _table(value).get(ecosystem.value)
        if not isinstance(entry, dict):
            continue
        url, fetched_at, packages = entry.get("url"), entry.get("fetched_at"), entry.get("packages")
        if not isinstance(url, str) or not isinstance(fetched_at, (int, float)) or not isinstance(packages, dict):
            continue
        catalogs[ecosystem] = MirrorCatalog(
            url,
            float(fetched_at),
            {
                name: tuple(version for version in versions if isinstance(version, str))
                for name, versions in packages.items()
                if isinstance(name, str) and isinstance(versions, list)
            },
        )
    return catalogs


def _mirror_dict(catalogs: Mapping[Ecosystem, MirrorCatalog]) -> dict[str, object]:
    return {
        ecosystem.value: {
            "url": catalog.url,
            "fetched_at": catalog.fetched_at,
            "packages": {name: list(versions) for name, versions in sorted(catalog.packages.items())},
        }
        for ecosystem, catalog in sorted(catalogs.items(), key=lambda item: item[0].value)
    }


def is_fresh(entry: Mapping[str, object], now: float) -> bool:
    if entry.get("vuln_error") or not isinstance(entry.get("vulns"), list):
        return False
    vulns = entry.get("vulns")
    if isinstance(vulns, list) and any(
        isinstance(item, dict)
        and isinstance((item_id := item.get("id")), str)
        and item_id.startswith("RUSTSEC-")
        and "affected" not in item
        for item in vulns
    ):
        return False
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
    source_fetcher: Callable[[str, str], tuple[str, ...]] | None = None,
    mirror_fetcher: Callable[[str], Mapping[str, tuple[str, ...]]] | None = None,
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
            or cache.results[item.purl].get("upstream_checked") is not True
        )
    ]
    registry = sources if sources is not None else registry_sources()
    source_urls = dict(cache.sources)
    unsourced = {
        item.component: (item.ecosystem, item.name)
        for item in items
        if not offline
        and (source_fetcher is not None or latest_fetcher is None)
        and item.ecosystem in _SOURCED
        and (refresh or item.component not in source_urls)
    }
    crate_names = {
        *(item.name for item in pending if item.ecosystem == "crates" and latest_fetcher is None),
        *(name for ecosystem, name in unsourced.values() if ecosystem == "crates" and source_fetcher is None),
    }
    lookup_errors: list[str] = []
    crates: dict[str, CrateRecord | None] = {}
    if crate_names:
        try:
            crates = resolve_crates(sorted(crate_names))
        except RegistryRateLimitedError as error:
            lookup_errors.append(str(error))
    latest_map: dict[tuple[str, str], str | None] = {}
    latest_sources: dict[tuple[str, str], str] = {}
    vulns_map: dict[tuple[str, str, str], list[dict[str, Any]]] = {}
    vuln_error: str | None = None
    unresolved_latest: set[tuple[str, str]] = set()
    if pending:
        latest_items: dict[tuple[str, str], LockedDependency] = {}
        for item in pending:
            key = (item.ecosystem, item.name)
            latest_items.setdefault(
                key, LockedDependency(item.ecosystem, item.name, item.version, True)
            )
        public = RegistrySources(None, None, None)
        for key in list(latest_items):
            if key[0] != "crates" or latest_fetcher is not None:
                continue
            del latest_items[key]
            if key[1] not in crates:
                unresolved_latest.add(key)
                continue
            record = crates[key[1]]
            latest_map[key] = record.latest if record is not None else None
            latest_sources[key] = _source_for(public, "crates")
            if record is None and _source_for(registry, "crates") != _source_for(public, "crates"):
                latest_items[key] = LockedDependency("crates", key[1], "", True)
        upstream = [item for key, item in latest_items.items() if key not in latest_map]
        found_latest = resolve_latest(
            upstream,
            offline=False,
            refresh=True,
            cache={},
            now=current,
            fetcher=latest_fetcher,
        )
        latest_map.update(found_latest)
        latest_sources.update({key: _source_for(public, key[0]) for key in found_latest})
        missing = [
            item for key, item in latest_items.items()
            if latest_map.get(key) is None and _source_for(registry, item.ecosystem) != latest_sources.get(key)
        ]
        if missing and latest_fetcher is None:
            configured = resolve_latest(
                missing,
                offline=False,
                refresh=True,
                cache={},
                now=current,
                sources=registry,
            )
            latest_map.update(configured)
            latest_sources.update({key: _source_for(registry, key[0]) for key in configured})
        try:
            vulns_map = resolve_vulns(
                [LockedDependency(item.ecosystem, item.name, item.version, True) for item in pending],
                offline=False, refresh=True, cache={}, now=current, osv_fetcher=osv_fetcher,
            )
        except VulnerabilityLookupError as error:
            vuln_error = str(error)
    if unresolved_latest and not lookup_errors:
        lookup_errors.append(f"crates.io returned no data for {len(unresolved_latest)} crates; run enrich again")
    results: dict[str, Mapping[str, object]] = dict(cache.results)
    for item in pending:
        if vuln_error is not None:
            results[item.purl] = {**results.get(item.purl, {}), "vuln_error": vuln_error}
            continue
        if (item.ecosystem, item.name) in unresolved_latest:
            continue
        results[item.purl] = {
            "latest": latest_map.get((item.ecosystem, item.name)),
            "latest_source": latest_sources.get((item.ecosystem, item.name)),
            "upstream_checked": True,
            "vulns": vulns_map.get((item.ecosystem, item.name, item.version), []),
            "fetched_at": current,
        }
    found: dict[tuple[str, str], tuple[str, ...]] = {
        ("crates", name): record.urls if record is not None else () for name, record in crates.items()
    }
    others = [key for key in set(unsourced.values()) if key[0] != "crates" or source_fetcher is not None]
    if others:
        found.update(resolve_source_urls(others, fetcher=source_fetcher))
    fetched_sources = {
        component: found[key] for component, key in unsourced.items() if key in found
    }
    unresolved_sources = len(unsourced) - len(fetched_sources)
    if unresolved_sources:
        lookup_errors.append(f"{unresolved_sources} package source lookups failed; run enrich again")
    source_urls.update(fetched_sources)
    fetched_at = current if pending and vuln_error is None else cache.fetched_at
    mirror = dict(cache.mirror)
    fetched_mirror = False
    if not offline and (mirror_fetcher is not None or latest_fetcher is None):
        read = mirror_fetcher if mirror_fetcher is not None else read_mirror_catalog
        for ecosystem, url in zip(_MIRRORED, (registry.cargo, registry.npm, registry.python), strict=True):
            if url is None:
                continue
            try:
                mirror[ecosystem] = MirrorCatalog(url, current, dict(read(url)))
                fetched_mirror = True
            except MirrorCatalogError as error:
                lookup_errors.append(str(error))
    if pending or fetched_sources or fetched_mirror:
        _write(
            cache.path,
            {
                "fetched_at": fetched_at,
                "results": results,
                "sources": {component: list(urls) for component, urls in sorted(source_urls.items())},
                "mirror": _mirror_dict(mirror),
            },
        )
    errors = (*((vuln_error,) if vuln_error is not None else ()), *lookup_errors)
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
            if entry is None or not isinstance(entry.get("vulns"), list):
                not_checked += 1
            elif not is_fresh(entry, current):
                stale += 1
            else:
                checked += 1
                vulns = entry.get("vulns")
                if isinstance(vulns, list) and vulns:
                    vulnerable += 1
    return EnrichReport(
        Enrichment(cache.path, fetched_at, results, source_urls, mirror),
        len(pending),
        checked,
        stale,
        not_checked,
        vulnerable,
        total,
        len(fetched_sources),
        errors,
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
                _Item(instance.purl, component.purl, ecosystem, component.name, instance.version),
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
    summary = (
        f"{report.checked} checked, {report.stale} stale, "
        f"{report.not_checked} not checked, {report.vulnerable} vulnerable "
        f"of {report.total} instances; fetched {report.fetched} entries "
        f"and {report.sources_fetched} source URLs to {report.cache.path}"
    )
    return summary + ("; " + "; ".join(report.errors) if report.errors else "")
