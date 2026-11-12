from __future__ import annotations

from repomod.dependencies.adapters.zig import ZigAdapter
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


def _extract(files: dict[str, str], *, vendored: tuple[str, ...] = ()) -> AdapterResult:
    adapter = ZigAdapter()
    inputs = tuple(
        InputContent(path, text, path in vendored) for path, text in files.items()
    )
    return adapter.extract(inputs)


def _instances(result: AdapterResult, name: str) -> list[Instance]:
    base = purl(Ecosystem.GENERIC, name)
    return [
        instance
        for instance in result.instances
        if instance.component == base or instance.component.startswith(base + "?")
    ]


def _direct(result: AdapterResult, name: str) -> list[Edge]:
    targets = {instance.id for instance in _instances(result, name)}
    return [
        edge for edge in result.edges if edge.relation is Relation.DIRECT and edge.to_id in targets
    ]


def test_url_hash_and_lazy_dependencies() -> None:
    result = _extract(
        {
            "build.zig.zon": (
                ".{\n"
                "    .name = .app,\n"
                '    .version = "1.0.0",\n'
                "    .dependencies = .{\n"
                "        .libxev = .{\n"
                '            .url = "https://example.com/libxev.tar.gz",\n'
                '            .hash = "libxev-0.0.0-abc",\n'
                "            .lazy = true,\n"
                "        },\n"
                "        .z2d = .{\n"
                '            .url = "https://example.com/z2d.tar.gz",\n'
                '            .hash = "z2d-0.13.2-def",\n'
                "        },\n"
                "    },\n"
                "}\n"
            )
        }
    )

    assert result.issues == ()
    assert [project.id for project in result.projects] == [".:generic"]
    libxev = _instances(result, "libxev")[0]
    assert libxev.component == (
        "pkg:generic/libxev?download_url=https%3A%2F%2Fexample.com%2Flibxev.tar.gz"
    )
    assert libxev.source_kind is SourceKind.URL
    edge = _direct(result, "libxev")[0]
    assert edge.optional is True
    assert edge.condition == "lazy"
    assert edge.constraint == "libxev-0.0.0-abc"
    assert edge.resolution is Resolution.CONDITIONAL
    z2d = _direct(result, "z2d")[0]
    assert z2d.optional is False
    assert z2d.condition is None
    assert z2d.resolution is Resolution.RESOLVED


def test_path_dependencies() -> None:
    result = _extract(
        {
            "build.zig.zon": (
                ".{\n"
                "    .dependencies = .{\n"
                '        .inside = .{ .path = "./pkg/inside" },\n'
                '        .outside = .{ .path = "../outside" },\n'
                '        .lazy_path = .{ .path = "./pkg/lazy", .lazy = true },\n'
                "    },\n"
                "}\n"
            )
        }
    )

    inside = _instances(result, "inside")[0]
    assert inside.source_kind is SourceKind.PATH
    assert inside.first_party is True
    assert _direct(result, "inside")[0].resolution is Resolution.RESOLVED
    assert _instances(result, "outside")[0].first_party is False
    lazy = _direct(result, "lazy_path")[0]
    assert lazy.optional is True
    assert lazy.condition == "lazy"
    assert lazy.resolution is Resolution.CONDITIONAL


def test_url_without_hash_is_declared_only() -> None:
    result = _extract(
        {
            "build.zig.zon": (
                ".{\n"
                "    .dependencies = .{\n"
                '        .thing = .{ .url = "https://example.com/thing.tar.gz" },\n'
                "    },\n"
                "}\n"
            )
        }
    )

    edge = _direct(result, "thing")[0]
    assert edge.resolution is Resolution.DECLARED_ONLY
    assert edge.constraint == "https://example.com/thing.tar.gz"


def test_unknown_dependency_form_is_a_dynamic_value() -> None:
    result = _extract(
        {
            "build.zig.zon": (
                ".{\n"
                "    .dependencies = .{\n"
                '        .thing = .{ .git = "https://example.com/thing" },\n'
                "    },\n"
                "}\n"
            )
        }
    )

    assert [issue.kind for issue in result.issues] == [CoverageKind.DYNAMIC_VALUE]
    assert result.edges == ()


def test_comments_hex_fingerprint_and_paths_are_tolerated() -> None:
    result = _extract(
        {
            "build.zig.zon": (
                ".{\n"
                "    .name = .ghostty,\n"
                '    .version = "1.4.1-dev",\n'
                "    .fingerprint = 0x64407a2a0b4147e5,\n"
                '    .minimum_zig_version = "0.17.0",\n'
                "    // A comment before the dependencies.\n"
                "    .dependencies = .{\n"
                "        // The path dependency.\n"
                '        .translate_c = .{ .path = "./pkg/translate-c" },\n'
                "    },\n"
                '    .paths = .{ "", "build.zig", "src" },\n'
                "}\n"
            )
        }
    )

    assert result.issues == ()
    assert [edge.to_id for edge in _direct(result, "translate_c")] == ["pkg:generic/translate_c#"]


def test_malformed_zon_is_a_parse_error() -> None:
    result = _extract({"build.zig.zon": ".{ .dependencies = .{ .foo = "})

    assert [issue.kind for issue in result.issues] == [CoverageKind.PARSE_ERROR]


def test_manifest_without_dependencies_is_valid() -> None:
    result = _extract(
        {
            "build.zig.zon": (
                ".{\n"
                "    .name = .app,\n"
                '    .version = "0.1.0",\n'
                "    .paths = .{\"\"},\n"
                "}\n"
            )
        }
    )

    assert result.issues == ()
    assert [project.id for project in result.projects] == [".:generic"]
    assert result.edges == ()


def test_vendored_manifest_marks_refs() -> None:
    result = _extract(
        {
            "vendor/thing/build.zig.zon": (
                ".{ .dependencies = .{ .foo = .{ .path = \"./foo\" } } }\n"
            )
        },
        vendored=("vendor/thing/build.zig.zon",),
    )

    edge = _direct(result, "foo")[0]
    assert edge.source_kind is SourceKind.VENDORED
    assert _instances(result, "foo")[0].source_kind is SourceKind.VENDORED


def test_zig_adapter_matches_zon_files() -> None:
    adapter = ZigAdapter()
    assert adapter.matches("build.zig.zon")
    assert adapter.matches("pkg/foo/build.zig.zon")
    assert not adapter.matches("build.zig.zon.bak")
    assert not adapter.matches("build.zig")


def test_extraction_is_deterministic() -> None:
    files = {
        "build.zig.zon": (
            ".{\n"
            "    .dependencies = .{\n"
            '        .a = .{ .path = "./a" },\n'
            '        .b = .{ .path = "./b" },\n'
            "    },\n"
            "}\n"
        )
    }

    result = _extract(files)
    assert _extract(files) == result
    assert len(result.edges) == 2
