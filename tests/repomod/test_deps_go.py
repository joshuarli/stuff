from __future__ import annotations

from repomod.dependencies.adapters.go import GoAdapter
from repomod.dependencies.model import (
    AdapterResult,
    CoverageKind,
    Ecosystem,
    Edge,
    InputContent,
    Instance,
    Relation,
    Resolution,
    Scope,
    SourceKind,
    purl,
)


def _extract(files: dict[str, str], *, vendored: tuple[str, ...] = ()) -> AdapterResult:
    adapter = GoAdapter()
    inputs = tuple(
        InputContent(path, text, path in vendored) for path, text in files.items()
    )
    return adapter.extract(inputs)


def _instances(
    result: AdapterResult, name: str, ecosystem: Ecosystem = Ecosystem.GOLANG
) -> list[Instance]:
    base = purl(ecosystem, name)
    return [
        instance
        for instance in result.instances
        if instance.component == base or instance.component.startswith(base + "?")
    ]


def _direct(
    result: AdapterResult, name: str, ecosystem: Ecosystem = Ecosystem.GOLANG
) -> list[Edge]:
    targets = {instance.id for instance in _instances(result, name, ecosystem)}
    return [
        edge for edge in result.edges if edge.relation is Relation.DIRECT and edge.to_id in targets
    ]


def _transitive(
    result: AdapterResult, name: str, ecosystem: Ecosystem = Ecosystem.GOLANG
) -> list[Edge]:
    targets = {instance.id for instance in _instances(result, name, ecosystem)}
    return [
        edge
        for edge in result.edges
        if edge.relation is Relation.TRANSITIVE and edge.to_id in targets
    ]


def test_requires_direct_and_indirect() -> None:
    result = _extract(
        {
            "go.mod": (
                "module example.com/app\n\n"
                "go 1.24\n\n"
                "require github.com/foo/bar v1.2.3\n\n"
                "require (\n"
                "\tgithub.com/baz/qux v0.4.5 // indirect\n"
                ")\n"
            ),
            "go.sum": (
                "github.com/foo/bar v1.2.3 h1:aaa\n"
                "github.com/foo/bar v1.2.3/go.mod h1:bbb\n"
                "github.com/baz/qux v0.4.5 h1:ccc\n"
            ),
        }
    )

    assert result.issues == ()
    assert [project.id for project in result.projects] == [".:golang"]
    direct = _direct(result, "github.com/foo/bar")
    assert len(direct) == 1
    assert direct[0].resolution is Resolution.RESOLVED
    assert direct[0].resolved_version == "v1.2.3"
    assert direct[0].constraint == "v1.2.3"
    assert direct[0].evidence[0].locator == "L5"
    indirect = _transitive(result, "github.com/baz/qux")
    assert len(indirect) == 1
    assert indirect[0].resolved_version == "v0.4.5"
    assert len(result.edges) == 2


def test_pseudo_versions_are_preserved() -> None:
    result = _extract(
        {
            "go.mod": (
                "module example.com/app\n\ngo 1.24\n\n"
                "require github.com/foo/bar v0.0.0-20220104043353-73e0943537d2\n"
            )
        }
    )

    instance = _instances(result, "github.com/foo/bar")[0]
    assert instance.version == "v0.0.0-20220104043353-73e0943537d2"
    assert instance.purl == (
        "pkg:golang/github.com/foo/bar@v0.0.0-20220104043353-73e0943537d2"
    )


def test_replace_to_another_module_version() -> None:
    result = _extract(
        {
            "go.mod": (
                "module example.com/app\n\ngo 1.24\n\n"
                "require example.com/old v1.0.0\n\n"
                "replace example.com/old => example.com/new v2.0.0\n"
            )
        }
    )

    assert result.issues == ()
    edge = _direct(result, "example.com/new")[0]
    assert edge.constraint == "v1.0.0"
    assert edge.resolved_version == "v2.0.0"
    assert edge.resolution is Resolution.RESOLVED
    assert _instances(result, "example.com/old") == []


def test_replace_to_local_paths() -> None:
    result = _extract(
        {
            "go.mod": (
                "module example.com/app\n\ngo 1.24\n\n"
                "require (\n"
                "\texample.com/inside v0.1.0\n"
                "\texample.com/outside v0.2.0\n"
                ")\n\n"
                "replace example.com/inside => ./internal/inside\n"
                "replace example.com/outside => /opt/outside\n"
            )
        }
    )

    inside = _instances(result, "example.com/inside", Ecosystem.GENERIC)[0]
    assert inside.source_kind is SourceKind.PATH
    assert inside.first_party is True
    inside_edge = _direct(result, "example.com/inside", Ecosystem.GENERIC)[0]
    assert inside_edge.constraint == "v0.1.0"
    assert inside_edge.resolution is Resolution.RESOLVED
    outside = _instances(result, "example.com/outside", Ecosystem.GENERIC)[0]
    assert outside.first_party is False


def test_tool_and_toolchain_scopes() -> None:
    result = _extract(
        {
            "go.mod": (
                "module example.com/app\n\n"
                "go 1.24\n\n"
                "toolchain go1.24.3\n\n"
                "require (\n"
                "\texample.com/mod v1.0.0\n"
                "\tgolang.org/x/tools v0.51.0 // indirect\n"
                ")\n\n"
                "tool example.com/mod/cmd/gen\n"
                "tool golang.org/x/tools/cmd/goimports\n"
            )
        }
    )

    assert result.issues == ()
    toolchain = [edge for edge in result.edges if edge.scope is Scope.TOOLCHAIN]
    assert {edge.to_id for edge in toolchain} == {
        "pkg:golang/example.com/mod@v1.0.0#",
        "pkg:golang/golang.org/x/tools@v0.51.0#",
        "pkg:generic/go@1.24.3#",
    }
    go = _instances(result, "go", Ecosystem.GENERIC)[0]
    assert go.purl == "pkg:generic/go@1.24.3"


def test_exclude_is_ignored() -> None:
    result = _extract(
        {
            "go.mod": (
                "module example.com/app\n\ngo 1.24\n\n"
                "require example.com/mod v1.0.0\n\n"
                "exclude example.com/mod v1.0.1\n"
            )
        }
    )

    assert result.issues == ()
    assert [edge.resolved_version for edge in _direct(result, "example.com/mod")] == ["v1.0.0"]


def test_go_work_and_go_sum_do_not_create_edges() -> None:
    result = _extract(
        {
            "go.work": "go 1.24\n\nuse ./module\n",
            "go.sum": "example.com/mod v1.0.0 h1:aaa\n",
        }
    )

    assert result.projects == ()
    assert result.instances == ()
    assert result.edges == ()
    assert result.issues == ()


def test_malformed_require_is_a_parse_error() -> None:
    result = _extract({"go.mod": "module example.com/app\n\nrequire example.com/mod\n"})

    assert [issue.kind for issue in result.issues] == [CoverageKind.PARSE_ERROR]


def test_missing_module_line_is_a_parse_error() -> None:
    result = _extract({"go.mod": "go 1.24\n"})

    assert [issue.kind for issue in result.issues] == [CoverageKind.PARSE_ERROR]


def test_non_version_requirement_is_dynamic() -> None:
    result = _extract(
        {
            "go.mod": (
                "module example.com/app\n\ngo 1.24\n\n"
                "require example.com/mod latest\n"
            )
        }
    )

    dynamic = [issue for issue in result.issues if issue.kind is CoverageKind.DYNAMIC_VALUE]
    assert len(dynamic) == 1
    edge = _direct(result, "example.com/mod")[0]
    assert edge.resolution is Resolution.UNKNOWN


def test_go_adapter_matches_go_inputs() -> None:
    adapter = GoAdapter()
    assert adapter.matches("go.mod")
    assert adapter.matches("c/go/go.mod")
    assert adapter.matches("go.sum")
    assert adapter.matches("go.work")
    assert not adapter.matches("go.mod.bak")
    assert not adapter.matches("Cargo.toml")


def test_extraction_is_deterministic() -> None:
    files = {
        "go.mod": (
            "module example.com/app\n\ngo 1.24\n\n"
            "require (\n\texample.com/a v1.0.0\n\texample.com/b v2.0.0\n)\n"
        )
    }

    result = _extract(files)
    assert _extract(files) == result
    assert len(result.edges) == 2
