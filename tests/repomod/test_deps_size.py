from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
from deps_fixtures import build_graph, instance, repository

from repomod import audit
from repomod.audit import PackageSize
from repomod.dependencies import enrich
from repomod.dependencies.model import Ecosystem, SourceKind

NPM = {"os": (), "cpu": (), "libc": ()}


def _npm(size: int | None, files: int | None = None, **platform: tuple[str, ...]) -> PackageSize:
    return PackageSize(size, "unpacked", files, None, **{**NPM, **platform})


def _wheel(filename: str, size: int) -> dict[str, Any]:
    return {"filename": filename, "size": size, "packagetype": "bdist_wheel"}


def test_npm_size_reads_the_version_manifest_and_the_packument() -> None:
    manifest = {"version": "1.2.3", "dist": {"unpackedSize": 4096, "fileCount": 7, "tarball": "x"}}
    packument = {"versions": {"1.2.3": manifest, "1.2.4": {"dist": {}}}}

    assert audit.npm_size(manifest, "1.2.3") == _npm(4096, 7)
    assert audit.npm_size(packument, "1.2.3") == _npm(4096, 7)
    assert audit.npm_size(packument, "1.2.4") == _npm(None)
    assert audit.npm_size(packument, "9.9.9") == _npm(None)
    assert audit.npm_size({"dist": {"unpackedSize": 0}}, "1.0.0") == _npm(None)


def test_npm_size_reads_the_platform_lists_of_the_version() -> None:
    manifest = {
        "dist": {"unpackedSize": 10},
        "os": ["linux", "!win32"],
        "cpu": "x64",
        "libc": ["musl"],
    }
    packument = {"versions": {"1.0.0": manifest, "2.0.0": {"dist": {"unpackedSize": 20}}}}

    expected = _npm(10, os=("linux", "!win32"), cpu=("x64",), libc=("musl",))
    assert audit.npm_size(manifest, "1.0.0") == expected
    assert audit.npm_size(packument, "1.0.0") == expected
    assert audit.npm_size(packument, "2.0.0") == _npm(20)
    assert audit.npm_size({"os": [1, "darwin", None]}, "1.0.0") == _npm(None, os=("darwin",))


def test_crate_size_reads_the_archive_size_of_one_version() -> None:
    versions = [{"num": "1.0.1", "crate_size": 900}, {"num": "1.0.0", "crate_size": 800}, "junk"]

    assert audit.crate_size(versions, "1.0.0") == PackageSize(800, "crate")
    assert audit.crate_size(versions, "2.0.0") == PackageSize(None, "crate")


@pytest.mark.parametrize(
    ("files", "expected"),
    [
        (
            [
                _wheel("p-1-py3-none-any.whl", 10),
                _wheel("p-1-cp312-cp312-manylinux_2_17_x86_64.manylinux2014_x86_64.whl", 20),
                _wheel("p-1-cp312-cp312-macosx_11_0_arm64.whl", 30),
                {"filename": "p-1.tar.gz", "size": 40, "packagetype": "sdist"},
            ],
            PackageSize(30, "wheel", platform="macos-arm64"),
        ),
        (
            [
                _wheel("p-1-cp312-cp312-macosx_10_9_x86_64.whl", 25),
                _wheel("p-1-cp312-cp312-musllinux_1_2_aarch64.whl", 21),
                _wheel("p-1-cp312-cp312-manylinux_2_28_x86_64.whl", 22),
                _wheel("p-1-py3-none-any.whl", 10),
            ],
            PackageSize(22, "wheel", platform="linux-x86_64"),
        ),
        (
            [_wheel("p-1-cp312-cp312-manylinux2014_aarch64.whl", 23), _wheel("p-1-py3-none-any.whl", 10)],
            PackageSize(23, "wheel", platform="linux-aarch64"),
        ),
        (
            [
                _wheel("p-1-cp312-cp312-win_amd64.whl", 50),
                _wheel("p-1-py3-none-any.whl", 10),
                {"filename": "p-1.tar.gz", "size": 40, "packagetype": "sdist"},
            ],
            PackageSize(10, "wheel", platform="any"),
        ),
        (
            [
                _wheel("p-1-cp312-cp312-win_amd64.whl", 50),
                {"filename": "p-1.tar.gz", "size": 40, "packagetype": "sdist"},
            ],
            PackageSize(40, "sdist"),
        ),
        (
            [
                _wheel("p-1-cp313-cp313-macosx_11_0_arm64.whl", 31),
                _wheel("p-1-cp312-cp312-macosx_11_0_arm64.whl", 30),
                _wheel("p-1-cp312-cp312-macosx_10_13_universal2.whl", 99),
            ],
            PackageSize(99, "wheel", platform="macos-arm64"),
        ),
        ([_wheel("p-1-cp312-cp312-win_amd64.whl", 50)], PackageSize(None, "wheel")),
        ([], PackageSize(None, "wheel")),
    ],
)
def test_pypi_size_picks_one_file_by_platform_preference(files: list[dict[str, Any]], expected: PackageSize) -> None:
    assert audit.pypi_size({"urls": files}) == expected


def test_pypi_size_is_deterministic_regardless_of_file_order() -> None:
    files = [
        _wheel("p-1-cp313-cp313-macosx_11_0_arm64.whl", 31),
        _wheel("p-1-cp312-cp312-macosx_11_0_arm64.whl", 30),
    ]

    assert audit.pypi_size({"urls": files}) == audit.pypi_size({"urls": files[::-1]})
    assert audit.pypi_size({"urls": files}).bytes == 30


def test_resolve_sizes_reads_each_registry_and_batches_crate_versions(monkeypatch: pytest.MonkeyPatch) -> None:
    published: dict[str, dict[str, Any] | None] = {
        "https://registry.npmjs.org/@scope%2Fpkg/1.0.0": {"dist": {"unpackedSize": 500, "fileCount": 3}},
        "https://registry.npmjs.org/gone/1.0.0": None,
        "https://pypi.org/pypi/requests/2.0.0/json": {"urls": [_wheel("r-2-py3-none-any.whl", 70)]},
    }
    requested: list[str] = []
    crate_requests: list[str] = []

    def document(url: str) -> dict[str, Any] | None:
        requested.append(url)
        return published[url]

    def crates(url: str) -> dict[str, Any] | None:
        crate_requests.append(url)
        if url.endswith("/lost/versions"):
            return {}
        return {"versions": [{"num": "1.0.0", "crate_size": 80}, {"num": "1.1.0", "crate_size": 90}]}

    monkeypatch.setattr(audit, "_published_document", document)
    monkeypatch.setattr(audit, "_crates_published", crates)

    found = audit.resolve_sizes(
        [
            ("npm", "@scope/pkg", "1.0.0"),
            ("npm", "gone", "1.0.0"),
            ("pypi", "requests", "2.0.0"),
            ("crates", "serde", "1.0.0"),
            ("crates", "serde", "1.1.0"),
            ("crates", "lost", "1.0.0"),
            ("golang", "example.com/mod", "v1.0.0"),
        ]
    )

    assert found == {
        ("npm", "@scope/pkg", "1.0.0"): _npm(500, 3),
        ("npm", "gone", "1.0.0"): _npm(None),
        ("pypi", "requests", "2.0.0"): PackageSize(70, "wheel", platform="any"),
        ("crates", "serde", "1.0.0"): PackageSize(80, "crate"),
        ("crates", "serde", "1.1.0"): PackageSize(90, "crate"),
        ("crates", "lost", "1.0.0"): PackageSize(None, "crate"),
    }
    assert sorted(crate_requests) == [
        "https://crates.io/api/v1/crates/lost/versions",
        "https://crates.io/api/v1/crates/serde/versions",
    ]
    assert len(requested) == 3


def test_resolve_sizes_omits_failed_lookups_so_a_later_run_retries(monkeypatch: pytest.MonkeyPatch) -> None:
    def document(url: str) -> dict[str, Any] | None:
        if "flaky" in url:
            raise audit.SourceLookupError("HTTP 503")
        return {"dist": {"unpackedSize": 10}}

    monkeypatch.setattr(audit, "_published_document", document)
    monkeypatch.setattr(audit, "_crates_published", lambda _url: None)

    found = audit.resolve_sizes(
        [("npm", "flaky", "1.0.0"), ("npm", "fine", "1.0.0"), ("crates", "serde", "1.0.0")]
    )

    assert found == {("npm", "fine", "1.0.0"): _npm(10)}


def test_resolve_sizes_stops_when_crates_io_keeps_rate_limiting(monkeypatch: pytest.MonkeyPatch) -> None:
    def limited(_url: str) -> dict[str, Any] | None:
        raise audit.RegistryRateLimitedError("slow down")

    monkeypatch.setattr(audit, "_crates_published", limited)
    monkeypatch.setattr(audit, "_published_document", lambda _url: {"dist": {"unpackedSize": 10}})

    found = audit.resolve_sizes([("crates", "a", "1.0.0"), ("crates", "b", "1.0.0"), ("npm", "n", "1.0.0")])

    assert all(key[0] != "crates" for key in found)


def _graph(tmp_path: Path):
    return build_graph(
        tmp_path,
        [
            repository(
                "acme/tool",
                (
                    instance(Ecosystem.CARGO, "serde", "1.0.0"),
                    instance(Ecosystem.CARGO, "forked", "0.1.0", source_kind=SourceKind.GIT),
                    instance(Ecosystem.CARGO, "tool-core", "0.1.0", source_kind=SourceKind.WORKSPACE, first_party=True),
                    instance(Ecosystem.PYPI, "requests", "2.0.0"),
                    instance(Ecosystem.NPM, "react", "18.0.0"),
                    instance(Ecosystem.GOLANG, "example.com/mod", "v1.0.0"),
                ),
            )
        ],
    )


def _latest(_ecosystem: str, _name: str) -> str | None:
    return "9.9.9"


def _osv(queries: list[dict[str, Any]]) -> list[list[dict[str, Any]]]:
    return [[] for _ in queries]


def _size(ecosystem: str, name: str, version: str) -> PackageSize | None:
    return {"crates": PackageSize(800, "crate"), "pypi": PackageSize(70, "wheel", platform="any")}.get(
        ecosystem, _npm(4096, 7)
    )


def test_enrich_caches_sizes_beside_the_existing_record_and_skips_other_ecosystems(tmp_path: Path) -> None:
    graph = _graph(tmp_path)
    cache_path = tmp_path / "cache.json"
    seen: list[tuple[str, str, str]] = []

    def fetch(ecosystem: str, name: str, version: str) -> PackageSize | None:
        seen.append((ecosystem, name, version))
        return _size(ecosystem, name, version)

    report = enrich.enrich(
        graph, cache_path=cache_path, now=1000.0, latest_fetcher=_latest, osv_fetcher=_osv, size_fetcher=fetch
    )

    assert sorted(seen) == [("crates", "serde", "1.0.0"), ("npm", "react", "18.0.0"), ("pypi", "requests", "2.0.0")]
    assert report.sizes_fetched == 3
    assert report.errors == ()
    results = json.loads(cache_path.read_text(encoding="utf-8"))["results"]
    assert results["pkg:cargo/serde@1.0.0"]["size"] == {"bytes": 800, "kind": "crate"}
    assert results["pkg:cargo/serde@1.0.0"]["latest"] == "9.9.9"
    assert results["pkg:npm/react@18.0.0"]["size"] == {
        "bytes": 4096, "kind": "unpacked", "files": 7, "os": [], "cpu": [], "libc": [],
    }
    assert results["pkg:pypi/requests@2.0.0"]["size"] == {"bytes": 70, "kind": "wheel", "platform": "any"}
    assert "size" not in results["pkg:cargo/forked@0.1.0"]
    assert "size" not in results["pkg:golang/example.com/mod@v1.0.0"]
    assert enrich.cached_size(report.cache.results["pkg:npm/react@18.0.0"]) == _npm(4096, 7)
    assert enrich.cached_size(report.cache.results["pkg:golang/example.com/mod@v1.0.0"]) is None


def test_cached_sizes_are_fetched_once_and_survive_a_vulnerability_refresh(tmp_path: Path) -> None:
    graph = _graph(tmp_path)
    cache_path = tmp_path / "cache.json"
    enrich.enrich(
        graph, cache_path=cache_path, now=1000.0, latest_fetcher=_latest, osv_fetcher=_osv, size_fetcher=_size
    )

    def unexpected(*_args: str) -> PackageSize | None:
        raise AssertionError("a cached size must not be fetched again")

    later = 1000.0 + enrich.MAX_AGE_SECONDS + 1
    report = enrich.enrich(
        graph, cache_path=cache_path, now=later, latest_fetcher=_latest, osv_fetcher=_osv, size_fetcher=unexpected
    )

    assert report.fetched == 6
    assert report.sizes_fetched == 0
    assert enrich.cached_size(report.cache.results["pkg:cargo/serde@1.0.0"]) == PackageSize(800, "crate")
    refreshed = enrich.enrich(
        graph, cache_path=cache_path, refresh=True, now=later, latest_fetcher=_latest, osv_fetcher=_osv,
        size_fetcher=lambda *_args: PackageSize(1, "crate"),
    )
    assert refreshed.sizes_fetched == 3
    assert enrich.cached_size(refreshed.cache.results["pkg:cargo/serde@1.0.0"]) == PackageSize(1, "crate")


def test_a_cache_from_before_sizes_gains_them_without_refetching_vulnerabilities(tmp_path: Path) -> None:
    graph = _graph(tmp_path)
    cache_path = tmp_path / "cache.json"
    enrich.enrich(graph, cache_path=cache_path, now=1000.0, latest_fetcher=_latest, osv_fetcher=_osv)
    assert all("size" not in entry for entry in json.loads(cache_path.read_text(encoding="utf-8"))["results"].values())

    report = enrich.enrich(
        graph, cache_path=cache_path, now=1001.0, latest_fetcher=_latest,
        osv_fetcher=lambda _queries: (_ for _ in ()).throw(AssertionError("fresh advisories")),
        size_fetcher=_size,
    )

    assert report.fetched == 0
    assert report.sizes_fetched == 3
    assert report.checked == 6
    assert enrich.cached_size(report.cache.results["pkg:npm/react@18.0.0"]) == _npm(4096, 7)


def test_a_version_the_registry_reports_no_size_for_is_recorded_and_not_asked_again(tmp_path: Path) -> None:
    graph = _graph(tmp_path)
    cache_path = tmp_path / "cache.json"
    calls: list[str] = []

    def none(ecosystem: str, _name: str, _version: str) -> PackageSize | None:
        calls.append(ecosystem)
        return _npm(None)

    enrich.enrich(
        graph, cache_path=cache_path, now=1000.0, latest_fetcher=_latest, osv_fetcher=_osv, size_fetcher=none
    )
    enrich.enrich(
        graph, cache_path=cache_path, now=1001.0, latest_fetcher=_latest, osv_fetcher=_osv, size_fetcher=none
    )

    assert len(calls) == 3
    entry = json.loads(cache_path.read_text(encoding="utf-8"))["results"]["pkg:npm/react@18.0.0"]
    assert entry["size"] == {"bytes": None, "kind": "unpacked", "os": [], "cpu": [], "libc": []}


def test_failed_size_lookups_are_reported_and_retried(tmp_path: Path) -> None:
    graph = _graph(tmp_path)
    cache_path = tmp_path / "cache.json"

    report = enrich.enrich(
        graph, cache_path=cache_path, now=1000.0, latest_fetcher=_latest, osv_fetcher=_osv,
        size_fetcher=lambda *_args: None,
    )

    assert report.sizes_fetched == 0
    assert report.errors == ("3 package size lookups failed; run enrich again",)
    assert all("size" not in entry for entry in report.cache.results.values())
    retried = enrich.enrich(
        graph, cache_path=cache_path, now=1001.0, latest_fetcher=_latest, osv_fetcher=_osv, size_fetcher=_size
    )
    assert retried.sizes_fetched == 3
    assert retried.errors == ()


def test_offline_enrichment_never_asks_for_sizes(tmp_path: Path) -> None:
    graph = _graph(tmp_path)
    cache_path = tmp_path / "cache.json"
    enrich.enrich(graph, cache_path=cache_path, now=1000.0, latest_fetcher=_latest, osv_fetcher=_osv)

    def unexpected(*_args: str) -> PackageSize | None:
        raise AssertionError("offline mode must not fetch")

    report = enrich.enrich(graph, cache_path=cache_path, offline=True, now=1001.0, size_fetcher=unexpected)

    assert report.sizes_fetched == 0
    assert report.errors == ()


def test_cached_size_ignores_malformed_records() -> None:
    assert enrich.cached_size(None) is None
    assert enrich.cached_size({}) is None
    assert enrich.cached_size({"size": "big"}) is None
    assert enrich.cached_size({"size": {"bytes": "10", "kind": "unpacked"}}) is None
    assert enrich.cached_size({"size": {"bytes": True, "kind": "unpacked"}}) is None
    assert enrich.cached_size({"size": {"bytes": 5}}) is None
    assert enrich.cached_size({"size": {"bytes": None, "kind": "crate"}}) == PackageSize(None, "crate")


def test_npm_sizes_cached_before_platform_fields_are_refetched_and_other_registries_are_not(tmp_path: Path) -> None:
    graph = _graph(tmp_path)
    cache_path = tmp_path / "cache.json"
    enrich.enrich(
        graph, cache_path=cache_path, now=1000.0, latest_fetcher=_latest, osv_fetcher=_osv, size_fetcher=_size
    )
    document = json.loads(cache_path.read_text(encoding="utf-8"))
    for key in ("os", "cpu", "libc"):
        del document["results"]["pkg:npm/react@18.0.0"]["size"][key]
    cache_path.write_text(json.dumps(document), encoding="utf-8")
    seen: list[str] = []

    def fetch(ecosystem: str, name: str, version: str) -> PackageSize | None:
        seen.append(ecosystem)
        return _npm(4096, 7, os=("darwin",), cpu=("arm64",))

    report = enrich.enrich(
        graph, cache_path=cache_path, now=1001.0, latest_fetcher=_latest, osv_fetcher=_osv, size_fetcher=fetch
    )

    assert seen == ["npm"]
    assert report.sizes_fetched == 1
    cached = enrich.cached_size(report.cache.results["pkg:npm/react@18.0.0"])
    assert cached == _npm(4096, 7, os=("darwin",), cpu=("arm64",))
    assert enrich.cached_size(report.cache.results["pkg:cargo/serde@1.0.0"]) == PackageSize(800, "crate")
