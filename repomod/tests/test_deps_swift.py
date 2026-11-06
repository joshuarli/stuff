from __future__ import annotations

import json

from repomod.dependencies.adapters.swift import SwiftAdapter
from repomod.dependencies.model import (
    AdapterResult,
    CoverageKind,
    Ecosystem,
    Edge,
    InputContent,
    Instance,
    Relation,
    Resolution,
    SourceKind,
    purl,
)

SHA = "6a52f3251125d74daf04fcbd5e6f08a75d074382"
FOO = "https://github.com/foo/foo.git"


def _v2(*pins: dict[str, object]) -> str:
    return json.dumps({"version": 2, "pins": list(pins)})


def _pin(
    identity: str,
    url: str,
    *,
    version: str | None = "1.0.0",
    revision: str = SHA,
    kind: str = "remoteSourceControl",
) -> dict[str, object]:
    state: dict[str, object] = {"revision": revision}
    if version is not None:
        state["version"] = version
    return {"identity": identity, "kind": kind, "location": url, "state": state}


def _extract(files: dict[str, str], *, vendored: tuple[str, ...] = ()) -> AdapterResult:
    adapter = SwiftAdapter()
    inputs = tuple(
        InputContent(path, text, path in vendored) for path, text in files.items()
    )
    return adapter.extract(inputs)


def _instances(
    result: AdapterResult, name: str, ecosystem: Ecosystem = Ecosystem.SWIFT
) -> list[Instance]:
    base = purl(ecosystem, name)
    return [
        instance
        for instance in result.instances
        if instance.component == base or instance.component.startswith(base + "?")
    ]


def _direct(
    result: AdapterResult, name: str, ecosystem: Ecosystem = Ecosystem.SWIFT
) -> list[Edge]:
    targets = {instance.id for instance in _instances(result, name, ecosystem)}
    return [
        edge for edge in result.edges if edge.relation is Relation.DIRECT and edge.to_id in targets
    ]


def _transitive(
    result: AdapterResult, name: str, ecosystem: Ecosystem = Ecosystem.SWIFT
) -> list[Edge]:
    targets = {instance.id for instance in _instances(result, name, ecosystem)}
    return [
        edge
        for edge in result.edges
        if edge.relation is Relation.TRANSITIVE and edge.to_id in targets
    ]


def test_resolved_v2_pins_create_transitive_edges() -> None:
    result = _extract({"Package.resolved": _v2(_pin("foo", FOO))})

    assert result.issues == ()
    assert [project.id for project in result.projects] == [".:swift"]
    instance = _instances(result, "foo")[0]
    assert instance.purl == (
        "pkg:swift/foo@1.0.0?vcs_url=https%3A%2F%2Fgithub.com%2Ffoo%2Ffoo.git"
    )
    assert instance.source_kind is SourceKind.GIT
    assert instance.vcs_url == FOO
    assert instance.commit == SHA
    edge = _transitive(result, "foo")[0]
    assert edge.resolution is Resolution.RESOLVED
    assert edge.evidence[0].locator == "pins[0]"


def test_resolved_v1_and_v3_are_supported() -> None:
    v1 = json.dumps(
        {
            "version": 1,
            "object": {
                "pins": [
                    {
                        "package": "legacy",
                        "repositoryURL": "https://github.com/foo/legacy",
                        "state": {"revision": SHA, "version": "0.9.0"},
                    }
                ]
            },
        }
    )
    v3 = json.dumps({"version": 3, "pins": [_pin("modern", "https://github.com/foo/modern")]})

    result = _extract({"Package.resolved": v1, "tools/Package.resolved": v3})

    assert result.issues == ()
    assert sorted(project.id for project in result.projects) == [".:swift", "tools:swift"]
    assert _instances(result, "legacy")[0].version == "0.9.0"
    assert _instances(result, "modern")[0].version == "1.0.0"
    assert _transitive(result, "legacy", Ecosystem.SWIFT)[0].evidence[0].locator == "object.pins[0]"


def test_manifest_literal_declarations() -> None:
    manifest = (
        "// swift-tools-version: 6.2\n"
        "let package = Package(\n"
        "  name: \"App\",\n"
        "  dependencies: [\n"
        f"    .package(url: \"https://github.com/apple/swift-argument-parser.git\", from: \"1.2.3\"),\n"
        f"    .package(url: \"https://github.com/foo/exact.git\", exact: \"2.0.0\"),\n"
        f"    .package(url: \"https://github.com/foo/branch.git\", branch: \"main\"),\n"
        f"    .package(url: \"https://github.com/foo/sha.git\", revision: \"{SHA}\"),\n"
        f"    .package(url: \"https://github.com/foo/up.git\", .upToNextMajor(from: \"3.0.0\")),\n"
        f"    .package(path: \"libs/local\"),\n"
        "  ]\n"
        ")\n"
    )
    result = _extract({"Package.swift": manifest})

    assert result.issues == ()
    assert _direct(result, "swift-argument-parser")[0].resolution is Resolution.DECLARED_ONLY
    assert _direct(result, "exact")[0].constraint == "2.0.0"
    assert _direct(result, "branch")[0].constraint == "main"
    sha = _direct(result, "sha")[0]
    assert sha.resolution is Resolution.RESOLVED
    assert _instances(result, "sha")[0].commit == SHA
    up = _direct(result, "up")[0]
    assert up.constraint == "3.0.0"
    assert up.resolution is Resolution.DECLARED_ONLY
    local = _instances(result, "local", Ecosystem.GENERIC)[0]
    assert local.source_kind is SourceKind.PATH
    assert local.first_party is True
    assert _direct(result, "local", Ecosystem.GENERIC)[0].resolution is Resolution.RESOLVED
    assert all(edge.evidence[0].locator.startswith("L") for edge in result.edges)


def test_manifest_and_resolved_resolve_direct_edges() -> None:
    manifest = (
        "let package = Package(\n"
        "  name: \"App\",\n"
        "  dependencies: [\n"
        f"    .package(url: \"{FOO}\", exact: \"1.2.3\"),\n"
        "  ]\n"
        ")\n"
    )
    resolved = _v2(
        _pin("foo", FOO, version="1.2.3"),
        _pin("bar", "https://github.com/foo/bar.git", version="4.5.6"),
    )
    result = _extract({"Package.swift": manifest, "Package.resolved": resolved})

    assert result.issues == ()
    foo = _direct(result, "foo")
    assert len(foo) == 1
    assert foo[0].resolved_version == "1.2.3"
    assert foo[0].resolution is Resolution.RESOLVED
    assert _transitive(result, "foo") == []
    assert len(_transitive(result, "bar")) == 1


def test_variable_declarations_are_dynamic_values() -> None:
    manifest = (
        "let sourceURL = \"https://github.com/foo/foo.git\"\n"
        "let version = \"1.0.0\"\n"
        "let package = Package(\n"
        "  dependencies: [\n"
        "    .package(url: sourceURL, from: \"1.0.0\"),\n"
        "    .package(url: \"https://github.com/foo/bar.git\", exact: version),\n"
        "  ]\n"
        ")\n"
    )
    result = _extract({"Package.swift": manifest})

    dynamic = [issue for issue in result.issues if issue.kind is CoverageKind.DYNAMIC_VALUE]
    assert len(dynamic) == 2
    assert result.edges == ()


def test_commented_declarations_are_ignored_and_traits_are_tolerated() -> None:
    manifest = (
        "// .package(url: \"https://github.com/foo/commented.git\", from: \"1.0.0\")\n"
        "/* .package(url: \"https://github.com/foo/blocked.git\", from: \"1.0.0\") */\n"
        "let package = Package(\n"
        "  dependencies: [\n"
        "    .package(url: \"https://github.com/foo/live.git\", from: \"1.0.0\", traits: [.default]),\n"
        "  ]\n"
        ")\n"
    )
    result = _extract({"Package.swift": manifest})

    assert result.issues == ()
    assert len(_direct(result, "live")) == 1
    assert _instances(result, "commented") == []
    assert _instances(result, "blocked") == []


def test_local_pin_is_a_path_package() -> None:
    resolved = json.dumps(
        {
            "version": 2,
            "pins": [
                {
                    "identity": "local",
                    "kind": "localSourceControl",
                    "location": "packages/local",
                    "state": {"version": "0.1.0"},
                }
            ],
        }
    )
    result = _extract({"Package.resolved": resolved})

    local = _instances(result, "local", Ecosystem.GENERIC)[0]
    assert local.source_kind is SourceKind.PATH
    assert local.first_party is True
    assert local.version == "0.1.0"


def test_unsupported_resolved_version() -> None:
    result = _extract({"Package.resolved": json.dumps({"version": 4, "pins": []})})

    assert [issue.kind for issue in result.issues] == [CoverageKind.UNSUPPORTED_FORMAT]


def test_malformed_resolved_json_is_a_parse_error() -> None:
    result = _extract({"Package.resolved": "{"})

    assert [issue.kind for issue in result.issues] == [CoverageKind.PARSE_ERROR]


def test_resolved_without_pins_is_a_parse_error() -> None:
    result = _extract({"Package.resolved": json.dumps({"version": 2})})

    assert [issue.kind for issue in result.issues] == [CoverageKind.PARSE_ERROR]


def test_vendored_manifest_marks_refs() -> None:
    manifest = (
        "let package = Package(\n"
        f"  dependencies: [.package(url: \"{FOO}\", from: \"1.0.0\")]\n"
        ")\n"
    )
    result = _extract({"vendor/thing/Package.swift": manifest}, vendored=("vendor/thing/Package.swift",))

    edge = _direct(result, "foo")[0]
    assert edge.source_kind is SourceKind.VENDORED
    assert _instances(result, "foo")[0].source_kind is SourceKind.VENDORED


def test_swift_adapter_matches_swift_inputs() -> None:
    adapter = SwiftAdapter()
    assert adapter.matches("Package.swift")
    assert adapter.matches("tools/Package.swift")
    assert adapter.matches("Package.resolved")
    assert not adapter.matches("Package.swift.bak")
    assert not adapter.matches("Package.resolved.bak")


def test_extraction_is_deterministic() -> None:
    files = {
        "Package.swift": (
            "let package = Package(\n"
            f"  dependencies: [.package(url: \"{FOO}\", from: \"1.0.0\")]\n"
            ")\n"
        )
    }

    result = _extract(files)
    assert _extract(files) == result
    assert len(result.edges) == 1
