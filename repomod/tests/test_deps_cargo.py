from __future__ import annotations

from repomod.dependencies.adapters.cargo import CargoAdapter
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

MIRROR = "sparse+https://crates.17166969.xyz/current/crates-io/"
NATIVE = "sparse+https://crates.17166969.xyz/current/registry/"
SHA = "770427abdcafc516531dd275423ceee0c0b1fb2e"


def _package(
    name: str,
    version: str,
    *,
    source: str | None = None,
    dependencies: tuple[str, ...] = (),
) -> str:
    lines = ["[[package]]", f'name = "{name}"', f'version = "{version}"']
    if source is not None:
        lines.append(f'source = "{source}"')
    if dependencies:
        lines.append("dependencies = [")
        lines.extend(f'  "{item}",' for item in dependencies)
        lines.append("]")
    return "\n".join(lines)


def _lock(*packages: str) -> str:
    return "version = 4\n\n" + "\n\n".join(packages) + "\n"


def _extract(
    files: dict[str, str],
    *,
    vendored: tuple[str, ...] = (),
    source: str | None = None,
    native: str | None = None,
) -> AdapterResult:
    adapter = CargoAdapter(source=source, native=native)
    inputs = tuple(
        InputContent(path, text, path in vendored) for path, text in files.items()
    )
    return adapter.extract(inputs)


def _instances(result: AdapterResult, name: str, ecosystem: Ecosystem = Ecosystem.CARGO) -> list[Instance]:
    base = purl(ecosystem, name)
    return [
        instance
        for instance in result.instances
        if instance.component == base or instance.component.startswith(base + "?")
    ]


def _direct(
    result: AdapterResult, name: str, ecosystem: Ecosystem = Ecosystem.CARGO
) -> list[Edge]:
    targets = {instance.id for instance in _instances(result, name, ecosystem)}
    return [
        edge
        for edge in result.edges
        if edge.relation is Relation.DIRECT and edge.to_id in targets
    ]


def _transitive(
    result: AdapterResult, name: str, ecosystem: Ecosystem = Ecosystem.CARGO
) -> list[Edge]:
    targets = {instance.id for instance in _instances(result, name, ecosystem)}
    return [
        edge
        for edge in result.edges
        if edge.relation is Relation.TRANSITIVE and edge.to_id in targets
    ]


def test_simple_package_resolves_direct_and_transitive_dependencies() -> None:
    result = _extract(
        {
            "Cargo.toml": (
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n"
                "[dependencies]\nserde = \"1.0\"\n\n"
                "[dev-dependencies]\ntempfile = \"3\"\n\n"
                "[build-dependencies]\ncc = \"1\"\n"
            ),
            "Cargo.lock": _lock(
                _package("app", "0.1.0", dependencies=("cc", "serde", "tempfile")),
                _package("cc", "1.2.59", source="registry+https://github.com/rust-lang/crates.io-index"),
                _package(
                    "serde",
                    "1.0.219",
                    source="registry+https://github.com/rust-lang/crates.io-index",
                    dependencies=("serde_derive",),
                ),
                _package(
                    "serde_derive",
                    "1.0.219",
                    source="registry+https://github.com/rust-lang/crates.io-index",
                ),
                _package("tempfile", "3.15.0", source="registry+https://github.com/rust-lang/crates.io-index"),
            ),
        }
    )

    assert result.issues == ()
    assert [project.id for project in result.projects] == [".:cargo"]
    assert result.projects[0].ecosystem is Ecosystem.CARGO
    assert result.projects[0].directory == "."

    serde = _instances(result, "serde")
    assert len(serde) == 1
    assert serde[0].version == "1.0.219"
    assert serde[0].source_kind is SourceKind.REGISTRY
    assert serde[0].registry_url == "https://github.com/rust-lang/crates.io-index"
    assert serde[0].repository_url is None
    assert serde[0].id == "pkg:cargo/serde@1.0.219#"

    runtime = _direct(result, "serde")
    assert len(runtime) == 1
    assert runtime[0].scope is Scope.RUNTIME
    assert runtime[0].resolution is Resolution.RESOLVED
    assert runtime[0].resolved_version == "1.0.219"
    assert runtime[0].constraint == "1.0"
    assert runtime[0].from_id == ".:cargo"
    assert [(item.path, item.locator) for item in runtime[0].evidence] == [
        ("Cargo.toml", "dependencies.serde"),
        ("Cargo.lock", "package[name=app,version=0.1.0].dependencies[1]"),
    ]

    assert [edge.scope for edge in _direct(result, "tempfile")] == [Scope.DEV]
    assert [edge.scope for edge in _direct(result, "cc")] == [Scope.BUILD]

    transitive = _transitive(result, "serde_derive")
    assert len(transitive) == 1
    assert transitive[0].from_id == serde[0].id
    assert transitive[0].resolution is Resolution.RESOLVED
    assert transitive[0].resolved_version == "1.0.219"
    assert transitive[0].scope is Scope.RUNTIME


def test_workspace_inheritance_resolves_versions_and_dependencies() -> None:
    result = _extract(
        {
            "Cargo.toml": (
                "[workspace]\nmembers = [\"crates/a\", \"crates/b\"]\n\n"
                "[workspace.package]\nversion = \"0.3.0\"\n\n"
                "[workspace.dependencies]\nserde = { version = \"1.0\", features = [\"derive\"] }\n"
            ),
            "crates/a/Cargo.toml": (
                "[package]\nname = \"a\"\nversion.workspace = true\n\n"
                "[dependencies]\nserde = { workspace = true }\n"
            ),
            "crates/b/Cargo.toml": (
                "[package]\nname = \"b\"\nversion.workspace = true\n\n"
                "[dependencies]\nserde = { workspace = true, features = [\"alloc\"] }\n"
            ),
            "Cargo.lock": _lock(
                _package("a", "0.3.0", dependencies=("serde",)),
                _package("b", "0.3.0", dependencies=("serde",)),
                _package("serde", "1.0.219", source="registry+https://github.com/rust-lang/crates.io-index"),
            ),
        }
    )

    assert result.issues == ()
    assert [project.id for project in result.projects] == ["crates/a:cargo", "crates/b:cargo"]
    for project in ("crates/a:cargo", "crates/b:cargo"):
        edges = [edge for edge in _direct(result, "serde") if edge.from_id == project]
        assert len(edges) == 1
        assert edges[0].resolution is Resolution.RESOLVED
        assert edges[0].resolved_version == "1.0.219"


def test_rename_uses_the_real_package_name() -> None:
    result = _extract(
        {
            "Cargo.toml": (
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n"
                "[dependencies]\nfoo = { package = \"serde\", version = \"1\" }\n"
            ),
            "Cargo.lock": _lock(
                _package("app", "0.1.0", dependencies=("serde",)),
                _package("serde", "1.0.219", source="registry+https://github.com/rust-lang/crates.io-index"),
            ),
        }
    )

    assert result.issues == ()
    edges = _direct(result, "serde")
    assert len(edges) == 1
    assert edges[0].constraint == "1"
    assert edges[0].evidence[0].locator == "dependencies.foo"
    assert all(instance.component != purl(Ecosystem.CARGO, "foo") for instance in result.instances)


def test_features_and_default_features_are_tolerated() -> None:
    result = _extract(
        {
            "Cargo.toml": (
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n"
                "[dependencies]\nserde = { version = \"1\", default-features = false, features = [\"derive\"] }\n"
            ),
            "Cargo.lock": _lock(
                _package("app", "0.1.0", dependencies=("serde",)),
                _package("serde", "1.0.219", source="registry+https://github.com/rust-lang/crates.io-index"),
            ),
        }
    )

    assert result.issues == ()
    edges = _direct(result, "serde")
    assert len(edges) == 1
    assert edges[0].resolution is Resolution.RESOLVED


def test_target_dependencies_are_conditional() -> None:
    result = _extract(
        {
            "Cargo.toml": (
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n"
                "[target.'cfg(unix)'.dependencies]\nlibc = \"0.2\"\n"
            ),
            "Cargo.lock": _lock(
                _package("app", "0.1.0", dependencies=("libc",)),
                _package("libc", "0.2.174", source="registry+https://github.com/rust-lang/crates.io-index"),
            ),
        }
    )

    edges = _direct(result, "libc")
    assert len(edges) == 1
    assert edges[0].condition == "cfg(unix)"
    assert edges[0].resolution is Resolution.CONDITIONAL
    assert edges[0].evidence[0].locator == "target.'cfg(unix)'.dependencies.libc"


def test_optional_dependencies_are_conditional() -> None:
    result = _extract(
        {
            "Cargo.toml": (
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n"
                "[dependencies]\nserde = { version = \"1\", optional = true }\n"
                "tracing = { version = \"0.1\", optional = true }\n\n"
                "[features]\nserde = [\"dep:serde\"]\n"
            ),
            "Cargo.lock": _lock(
                _package("app", "0.1.0", dependencies=("serde", "tracing")),
                _package("serde", "1.0.219", source="registry+https://github.com/rust-lang/crates.io-index"),
                _package("tracing", "0.1.41", source="registry+https://github.com/rust-lang/crates.io-index"),
            ),
        }
    )

    serde = _direct(result, "serde")
    assert len(serde) == 1
    assert serde[0].optional is True
    assert serde[0].condition == "feature:serde"
    assert serde[0].resolution is Resolution.CONDITIONAL

    tracing = _direct(result, "tracing")
    assert len(tracing) == 1
    assert tracing[0].condition == "feature:tracing"
    assert tracing[0].resolution is Resolution.CONDITIONAL


def test_git_and_path_sources() -> None:
    result = _extract(
        {
            "Cargo.toml": (
                "[workspace]\nmembers = [\"crates/local\"]\n\n"
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n"
                "[dependencies]\nlocal = { path = \"crates/local\" }\n"
                f"gitdep = {{ git = \"https://github.com/acme/gitdep\", rev = \"{SHA}\" }}\n"
            ),
            "crates/local/Cargo.toml": "[package]\nname = \"local\"\nversion = \"0.1.0\"\n",
            "Cargo.lock": _lock(
                _package("app", "0.1.0", dependencies=("gitdep", "local")),
                _package("local", "0.1.0"),
                _package(
                    "gitdep",
                    "0.2.0",
                    source=f"git+https://github.com/acme/gitdep?rev={SHA}#{SHA}",
                ),
            ),
        }
    )

    local = _instances(result, "local", Ecosystem.GENERIC)
    assert len(local) == 1
    assert local[0].source_kind is SourceKind.WORKSPACE
    assert local[0].first_party is True
    assert local[0].version == "0.1.0"
    local_edge = [edge for edge in result.edges if edge.to_id == local[0].id and edge.from_id == ".:cargo"]
    assert len(local_edge) == 1
    assert local_edge[0].source_kind is SourceKind.WORKSPACE
    assert local_edge[0].resolution is Resolution.RESOLVED

    git = _instances(result, "gitdep")
    assert len(git) == 1
    assert git[0].source_kind is SourceKind.GIT
    assert git[0].vcs_url == "https://github.com/acme/gitdep"
    assert git[0].commit == SHA
    assert git[0].purl == "pkg:cargo/gitdep@0.2.0?vcs_url=https%3A%2F%2Fgithub.com%2Facme%2Fgitdep"
    git_edge = _direct(result, "gitdep")
    assert len(git_edge) == 1
    assert git_edge[0].source_kind is SourceKind.GIT
    assert git_edge[0].resolved_version == "0.2.0"


def test_path_dependency_without_a_lock_is_declared_only() -> None:
    result = _extract(
        {
            "Cargo.toml": (
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n"
                "[dependencies]\nlocal = { path = \"crates/local\" }\n"
            ),
            "crates/local/Cargo.toml": "[package]\nname = \"local\"\nversion = \"0.4.0\"\n",
        }
    )

    assert [issue.kind for issue in result.issues] == [CoverageKind.MISSING_LOCK]
    local = _instances(result, "local", Ecosystem.GENERIC)
    assert len(local) == 1
    assert local[0].version == "0.4.0"
    assert local[0].source_kind is SourceKind.PATH
    edges = _direct(result, "local", Ecosystem.GENERIC)
    assert len(edges) == 1
    assert edges[0].resolution is Resolution.RESOLVED


def test_mirror_and_native_registries_map_to_distinct_components() -> None:
    result = _extract(
        {
            "Cargo.toml": (
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n"
                "[dependencies]\nserde = \"1\"\nnative = \"1\"\nother = \"1\"\n"
            ),
            "Cargo.lock": _lock(
                _package("app", "0.1.0", dependencies=("native", "other", "serde")),
                _package("serde", "1.0.219", source=MIRROR),
                _package("native", "1.0.0", source=NATIVE),
                _package("other", "1.0.0", source="registry+https://example.com/index"),
            ),
        },
        source=MIRROR,
        native=NATIVE,
    )

    serde = _instances(result, "serde")
    assert len(serde) == 1
    assert serde[0].purl == "pkg:cargo/serde@1.0.219"
    assert serde[0].registry_url == "https://crates.17166969.xyz/current/crates-io/"
    assert serde[0].repository_url is None

    native = _instances(result, "native")
    assert len(native) == 1
    assert native[0].purl == (
        "pkg:cargo/native@1.0.0?repository_url=https%3A%2F%2Fcrates.17166969.xyz%2Fcurrent%2Fregistry%2F"
    )
    assert native[0].repository_url == "https://crates.17166969.xyz/current/registry/"

    other = _instances(result, "other")
    assert len(other) == 1
    assert other[0].component == "pkg:cargo/other?repository_url=https%3A%2F%2Fexample.com%2Findex"
    assert other[0].repository_url == "https://example.com/index"

    assert _direct(result, "serde")[0].source_kind is SourceKind.REGISTRY


def test_canonical_crates_io_sources_are_public() -> None:
    result = _extract(
        {
            "Cargo.toml": "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1\"\n",
            "Cargo.lock": _lock(
                _package("app", "0.1.0", dependencies=("serde",)),
                _package("serde", "1.0.0", source="sparse+https://index.crates.io/"),
            ),
        }
    )

    serde = _instances(result, "serde")
    assert len(serde) == 1
    assert serde[0].purl == "pkg:cargo/serde@1.0.0"


def test_multiple_versions_are_disambiguated_by_the_lock_reference() -> None:
    result = _extract(
        {
            "Cargo.toml": (
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n"
                "[dependencies]\nsyn = \"2\"\nfoo = \"1\"\n"
            ),
            "Cargo.lock": _lock(
                _package("app", "0.1.0", dependencies=("foo", "syn 2.0.119")),
                _package(
                    "foo",
                    "1.0.0",
                    source="registry+https://github.com/rust-lang/crates.io-index",
                    dependencies=("syn 1.0.109",),
                ),
                _package("syn", "1.0.109", source="registry+https://github.com/rust-lang/crates.io-index"),
                _package("syn", "2.0.119", source="registry+https://github.com/rust-lang/crates.io-index"),
            ),
        }
    )

    syn_instances = _instances(result, "syn")
    assert sorted(instance.version or "" for instance in syn_instances) == ["1.0.109", "2.0.119"]
    assert len({instance.component for instance in syn_instances}) == 1

    direct = _direct(result, "syn")
    assert len(direct) == 1
    assert direct[0].resolved_version == "2.0.119"

    transitive = _transitive(result, "syn")
    assert len(transitive) == 1
    assert transitive[0].resolved_version == "1.0.109"
    foo = _instances(result, "foo")[0]
    assert transitive[0].from_id == foo.id


def test_lock_mismatch_is_unknown_and_typed() -> None:
    result = _extract(
        {
            "Cargo.toml": (
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n"
                "[dependencies]\nanyhow = \"1\"\n"
            ),
            "Cargo.lock": _lock(_package("app", "0.1.0")),
        }
    )

    issues = [issue for issue in result.issues if issue.kind is CoverageKind.LOCK_MISMATCH]
    assert len(issues) == 1
    assert issues[0].path == "Cargo.toml"
    assert issues[0].locator == "dependencies.anyhow"
    edges = _direct(result, "anyhow")
    assert len(edges) == 1
    assert edges[0].resolution is Resolution.UNKNOWN
    assert edges[0].resolved_version is None
    anyhow = _instances(result, "anyhow")
    assert len(anyhow) == 1
    assert anyhow[0].version is None


def test_missing_lock_produces_declared_only_edges() -> None:
    result = _extract(
        {
            "Cargo.toml": (
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n"
                "[dependencies]\nserde = \"1\"\n"
                f"pinned = {{ git = \"https://github.com/acme/pinned\", rev = \"{SHA}\" }}\n"
                "floating = { git = \"https://github.com/acme/floating\", branch = \"main\" }\n"
            )
        }
    )

    assert [issue.kind for issue in result.issues] == [CoverageKind.MISSING_LOCK]
    assert result.issues[0].path == "Cargo.lock"

    serde = _direct(result, "serde")
    assert len(serde) == 1
    assert serde[0].resolution is Resolution.DECLARED_ONLY
    assert serde[0].resolved_version is None
    assert serde[0].constraint == "1"
    assert _instances(result, "serde")[0].version is None

    pinned = _direct(result, "pinned")
    assert len(pinned) == 1
    assert pinned[0].resolution is Resolution.RESOLVED
    assert _instances(result, "pinned")[0].commit == SHA

    floating = _direct(result, "floating")
    assert len(floating) == 1
    assert floating[0].resolution is Resolution.DECLARED_ONLY
    assert floating[0].constraint == "main"


def test_malformed_inputs_are_parse_errors() -> None:
    result = _extract(
        {
            "Cargo.toml": "[package\nname =",
            "Cargo.lock": "version = 4\n\n[[package]]\nname = \"x\"\n",
            "crates/broken/Cargo.toml": "not = [valid",
            "crates/broken/Cargo.lock": "[package]",
        }
    )

    errors = [issue for issue in result.issues if issue.kind is CoverageKind.PARSE_ERROR]
    assert {issue.path for issue in errors} == {
        "Cargo.toml",
        "Cargo.lock",
        "crates/broken/Cargo.toml",
        "crates/broken/Cargo.lock",
    }


def test_excluded_and_standalone_projects_do_not_inherit_workspace_dependencies() -> None:
    result = _extract(
        {
            "Cargo.toml": (
                "[workspace]\nmembers = [\"member\"]\nexclude = [\"excluded\"]\n\n"
                "[workspace.dependencies]\nserde = \"1\"\n"
            ),
            "member/Cargo.toml": (
                "[package]\nname = \"member\"\nversion = \"0.1.0\"\n\n"
                "[dependencies]\nserde = { workspace = true }\n"
            ),
            "excluded/Cargo.toml": (
                "[workspace]\n\n[package]\nname = \"excluded\"\nversion = \"0.2.0\"\n\n"
                "[dependencies]\nserde = \"1\"\n"
            ),
            "standalone/Cargo.toml": (
                "[workspace]\n\n[package]\nname = \"standalone\"\nversion = \"0.3.0\"\n\n"
                "[dependencies]\nserde = \"1\"\n"
            ),
        }
    )

    assert [project.id for project in result.projects] == [
        "excluded:cargo",
        "member:cargo",
        "standalone:cargo",
    ]
    assert not [issue for issue in result.issues if issue.kind is CoverageKind.DYNAMIC_VALUE]
    assert len(_direct(result, "serde")) == 3
    assert {issue.path for issue in result.issues if issue.kind is CoverageKind.MISSING_LOCK} == {
        "Cargo.lock",
        "excluded/Cargo.lock",
        "standalone/Cargo.lock",
    }


def test_excluded_member_using_inheritance_is_a_dynamic_value() -> None:
    result = _extract(
        {
            "Cargo.toml": (
                "[workspace]\nmembers = []\nexclude = [\"excluded\"]\n\n"
                "[workspace.dependencies]\nserde = \"1\"\n"
            ),
            "excluded/Cargo.toml": (
                "[package]\nname = \"excluded\"\nversion = \"0.1.0\"\n\n"
                "[dependencies]\nserde = { workspace = true }\n"
            ),
        }
    )

    assert [issue.kind for issue in result.issues if issue.kind is CoverageKind.DYNAMIC_VALUE] == [
        CoverageKind.DYNAMIC_VALUE
    ]
    dynamic = [issue for issue in result.issues if issue.kind is CoverageKind.DYNAMIC_VALUE]
    assert "workspace dependency serde is not declared" in dynamic[0].message


def test_named_registries_and_unknown_sources_are_dynamic_values() -> None:
    result = _extract(
        {
            "Cargo.toml": (
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n"
                "[dependencies]\nnative = { version = \"1\", registry = \"mycrates\" }\n"
            ),
            "Cargo.lock": _lock(
                _package("app", "0.1.0", dependencies=("native",)),
                _package("native", "1.0.0", source="registry+https://example.com/index"),
            ),
        }
    )

    dynamic = [issue for issue in result.issues if issue.kind is CoverageKind.DYNAMIC_VALUE]
    assert len(dynamic) == 1
    assert "mycrates" in dynamic[0].message
    native = _instances(result, "native")[0]
    assert native.component == "pkg:cargo/native?repository_url=https%3A%2F%2Fexample.com%2Findex"


def test_unsupported_lock_source_is_dynamic_value() -> None:
    result = _extract(
        {
            "Cargo.toml": "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nwidget = \"1\"\n",
            "Cargo.lock": _lock(
                _package("app", "0.1.0", dependencies=("widget",)),
                _package("widget", "1.0.0", source="file:///opt/widgets"),
            ),
        }
    )

    dynamic = [issue for issue in result.issues if issue.kind is CoverageKind.DYNAMIC_VALUE]
    assert len(dynamic) == 1
    assert "file:///opt/widgets" in dynamic[0].message
    widget = _instances(result, "widget")[0]
    assert widget.component == "pkg:cargo/widget?repository_url=file%3A%2F%2F%2Fopt%2Fwidgets"


def test_vendored_manifest_edges_and_instances_are_vendored() -> None:
    result = _extract(
        {
            "Cargo.toml": "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
            "vendor/foo/Cargo.toml": (
                "[package]\nname = \"foo\"\nversion = \"0.1.0\"\n\n"
                "[dependencies]\nserde = \"1\"\n"
            ),
        },
        vendored=("vendor/foo/Cargo.toml",),
    )

    edge = _direct(result, "serde")[0]
    assert edge.source_kind is SourceKind.VENDORED
    assert edge.from_id == "vendor/foo:cargo"
    serde = _instances(result, "serde")[0]
    assert serde.source_kind is SourceKind.VENDORED
    assert serde.id == "pkg:cargo/serde#"
    assert [issue.kind for issue in result.issues] == [CoverageKind.MISSING_LOCK]


def test_projects_without_packages_or_dependencies_are_valid() -> None:
    result = _extract(
        {
            "Cargo.toml": "[workspace]\nmembers = []\n",
            "empty/Cargo.toml": "[package]\nname = \"empty\"\nversion = \"0.1.0\"\n",
        }
    )

    assert result.issues == ()
    assert [project.id for project in result.projects] == ["empty:cargo"]
    assert [edge for edge in result.edges if edge.from_id == "empty:cargo"] == []


def test_cargo_adapter_matches_only_cargo_files() -> None:
    adapter = CargoAdapter()
    assert adapter.matches("Cargo.toml")
    assert adapter.matches("crates/foo/Cargo.toml")
    assert adapter.matches("Cargo.lock")
    assert adapter.matches("vendor/foo/Cargo.lock")
    assert not adapter.matches("Cargo.toml.bak")
    assert not adapter.matches("cargo.lock")
    assert not adapter.matches("pyproject.toml")


def test_nested_workspaces_use_the_nearest_root() -> None:
    result = _extract(
        {
            "Cargo.toml": "[workspace]\nmembers = [\"outer\"]\n",
            "outer/Cargo.toml": (
                "[workspace]\nmembers = [\"inner\"]\n\n"
                "[package]\nname = \"outer\"\nversion = \"0.1.0\"\n\n"
                "[workspace.dependencies]\nserde = \"1\"\n\n"
                "[dependencies]\nserde = { workspace = true }\n"
            ),
            "outer/inner/Cargo.toml": (
                "[package]\nname = \"inner\"\nversion = \"0.1.0\"\n\n"
                "[dependencies]\nserde = { workspace = true }\n"
            ),
            "outer/Cargo.lock": _lock(
                _package("outer", "0.1.0", dependencies=("serde",)),
                _package("inner", "0.1.0", dependencies=("serde",)),
                _package("serde", "1.0.219", source="registry+https://github.com/rust-lang/crates.io-index"),
            ),
        }
    )

    assert result.issues == ()
    assert [project.id for project in result.projects] == ["outer:cargo", "outer/inner:cargo"]
    assert len(_direct(result, "serde")) == 2


def test_same_version_from_two_sources_is_disambiguated_by_source() -> None:
    git_source = f"git+https://github.com/acme/fork?rev={SHA}#{SHA}"
    result = _extract(
        {
            "Cargo.toml": (
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n"
                f"[dependencies]\nfork = {{ git = \"https://github.com/acme/fork\", rev = \"{SHA}\" }}\n"
            ),
            "Cargo.lock": _lock(
                _package("app", "0.1.0", dependencies=(f"fork 1.0.0 ({git_source})",)),
                _package("fork", "1.0.0", source=git_source),
                _package("fork", "1.0.0", source="registry+https://github.com/rust-lang/crates.io-index"),
            ),
        }
    )

    forks = _instances(result, "fork")
    assert len(forks) == 2
    assert {instance.source_kind for instance in forks} == {SourceKind.GIT, SourceKind.REGISTRY}
    direct = _direct(result, "fork")
    assert len(direct) == 1
    assert direct[0].source_kind is SourceKind.GIT
    git_instance = next(instance for instance in forks if instance.source_kind is SourceKind.GIT)
    assert direct[0].to_id == git_instance.id


def test_extraction_is_deterministic() -> None:
    files = {
        "Cargo.toml": (
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n"
            "[dependencies]\nserde = \"1\"\nb = \"2\"\na = \"3\"\n"
        ),
        "Cargo.lock": _lock(
            _package("app", "0.1.0", dependencies=("a", "b", "serde")),
            _package("serde", "1.0.0", source="registry+https://github.com/rust-lang/crates.io-index"),
            _package("a", "1.0.0", source="registry+https://github.com/rust-lang/crates.io-index"),
            _package("b", "1.0.0", source="registry+https://github.com/rust-lang/crates.io-index"),
        ),
    }

    result = _extract(files)
    assert _extract(files) == result
    assert len(result.edges) == 3
