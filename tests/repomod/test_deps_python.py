from __future__ import annotations

from packaging.utils import canonicalize_name

from repomod.dependencies.adapters.python import PythonAdapter
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

PYPI = "https://pypi.org/simple"
MIRROR = "https://pypi.17166969.xyz/current/simple/"
SHA = "770427abdcafc516531dd275423ceee0c0b1fb2e"


def _ref(
    name: str,
    *,
    marker: str | None = None,
    version: str | None = None,
    source: str | None = None,
) -> str:
    parts = [f'name = "{name}"']
    if marker is not None:
        parts.append(f'marker = "{marker}"')
    if version is not None:
        parts.append(f'version = "{version}"')
    if source is not None:
        parts.append(f"source = {source}")
    return "{ " + ", ".join(parts) + " }"


def _package(
    name: str,
    version: str | None = None,
    *,
    source: str | None = None,
    dependencies: tuple[str, ...] = (),
    optional: dict[str, tuple[str, ...]] | None = None,
    dev: dict[str, tuple[str, ...]] | None = None,
    markers: tuple[str, ...] = (),
) -> str:
    lines = ["[[package]]", f'name = "{name}"']
    if version is not None:
        lines.append(f'version = "{version}"')
    if source is not None:
        lines.append(f"source = {source}")
    if markers:
        lines.append("resolution-markers = [" + ", ".join(f'"{item}"' for item in markers) + "]")
    if dependencies:
        lines.append("dependencies = [")
        lines.extend(f"  {item}," for item in dependencies)
        lines.append("]")
    for section, groups in (("optional-dependencies", optional), ("dev-dependencies", dev)):
        if groups is not None:
            lines.append(f"[package.{section}]")
            for group, refs in groups.items():
                lines.append(f"{group} = [")
                lines.extend(f"  {item}," for item in refs)
                lines.append("]")
    return "\n".join(lines)


def _lock(*packages: str) -> str:
    return "version = 1\nrevision = 5\n\n" + "\n\n".join(packages) + "\n"


def _extract(
    files: dict[str, str],
    *,
    index: str | None = PYPI,
    vendored: tuple[str, ...] = (),
) -> AdapterResult:
    adapter = PythonAdapter(index=index)
    inputs = tuple(
        InputContent(path, text, path in vendored) for path, text in files.items()
    )
    return adapter.extract(inputs)


def _instances(
    result: AdapterResult, name: str, ecosystem: Ecosystem = Ecosystem.PYPI
) -> list[Instance]:
    base = purl(ecosystem, canonicalize_name(name) if ecosystem is Ecosystem.PYPI else name)
    return [
        instance
        for instance in result.instances
        if instance.component == base or instance.component.startswith(base + "?")
    ]


def _direct(
    result: AdapterResult, name: str, ecosystem: Ecosystem = Ecosystem.PYPI
) -> list[Edge]:
    targets = {instance.id for instance in _instances(result, name, ecosystem)}
    return [
        edge for edge in result.edges if edge.relation is Relation.DIRECT and edge.to_id in targets
    ]


def _transitive(
    result: AdapterResult, name: str, ecosystem: Ecosystem = Ecosystem.PYPI
) -> list[Edge]:
    targets = {instance.id for instance in _instances(result, name, ecosystem)}
    return [
        edge
        for edge in result.edges
        if edge.relation is Relation.TRANSITIVE and edge.to_id in targets
    ]


def test_simple_project_resolves_runtime_and_dev_dependencies() -> None:
    result = _extract(
        {
            "pyproject.toml": (
                "[project]\nname = \"app\"\nversion = \"0.1.0\"\n"
                "dependencies = [\"pytest>=9.0\"]\n\n"
                "[dependency-groups]\ndev = [\"ruff==0.17.2\"]\n"
            ),
            "uv.lock": _lock(
                _package(
                    "app",
                    "0.1.0",
                    source='{ editable = "." }',
                    dependencies=(_ref("pytest"),),
                    dev={"dev": (_ref("ruff"),)},
                ),
                _package("pytest", "9.2.0", source=f'{{ registry = "{PYPI}" }}', dependencies=(_ref("pluggy"),)),
                _package("pluggy", "1.7.0", source=f'{{ registry = "{PYPI}" }}'),
                _package("ruff", "0.17.2", source=f'{{ registry = "{PYPI}" }}'),
            ),
        }
    )

    assert result.issues == ()
    assert [project.id for project in result.projects] == [".:pypi"]
    pytest_instances = _instances(result, "pytest")
    assert len(pytest_instances) == 1
    assert pytest_instances[0].purl == "pkg:pypi/pytest@9.2.0"
    assert pytest_instances[0].registry_url == PYPI
    assert pytest_instances[0].repository_url is None
    assert pytest_instances[0].id == "pkg:pypi/pytest@9.2.0#"

    runtime = _direct(result, "pytest")
    assert len(runtime) == 1
    assert runtime[0].scope is Scope.RUNTIME
    assert runtime[0].resolution is Resolution.RESOLVED
    assert runtime[0].resolved_version == "9.2.0"
    assert runtime[0].constraint == ">=9.0"
    assert [(item.path, item.locator) for item in runtime[0].evidence] == [
        ("pyproject.toml", "project.dependencies[0]"),
        ("uv.lock", "package[name=app,version=0.1.0].dependencies[0]"),
    ]

    dev = _direct(result, "ruff")
    assert len(dev) == 1
    assert dev[0].scope is Scope.DEV
    assert dev[0].condition == "group == 'dev'"
    assert dev[0].resolution is Resolution.CONDITIONAL
    assert dev[0].resolved_version == "0.17.2"

    transitive = _transitive(result, "pluggy")
    assert len(transitive) == 1
    assert transitive[0].from_id == pytest_instances[0].id
    assert transitive[0].resolution is Resolution.RESOLVED


def test_optional_dependencies_are_conditional() -> None:
    result = _extract(
        {
            "pyproject.toml": (
                "[project]\nname = \"tool\"\nversion = \"0.1.0\"\n"
                "dependencies = []\n\n"
                "[project.optional-dependencies]\nbrowser = [\"shadowdriver\"]\n\n"
                "[tool.uv.sources]\nshadowdriver = { path = \"../shadowdriver/py\" }\n"
            ),
            "uv.lock": _lock(
                _package(
                    "tool",
                    "0.1.0",
                    source='{ editable = "." }',
                    optional={"browser": (_ref("shadowdriver"),)},
                ),
                _package("shadowdriver", source='{ directory = "../shadowdriver/py" }'),
            ),
        }
    )

    shadowdriver = _instances(result, "shadowdriver", Ecosystem.GENERIC)
    assert len(shadowdriver) == 1
    assert shadowdriver[0].source_kind is SourceKind.PATH
    assert shadowdriver[0].first_party is False
    assert shadowdriver[0].version is None
    edge = _direct(result, "shadowdriver", Ecosystem.GENERIC)[0]
    assert edge.scope is Scope.OPTIONAL
    assert edge.optional is True
    assert edge.condition == "extra == 'browser'"
    assert edge.resolution is Resolution.CONDITIONAL


def test_markers_produce_conditional_edges() -> None:
    result = _extract(
        {
            "pyproject.toml": (
                "[project]\nname = \"app\"\nversion = \"0.1.0\"\n"
                "dependencies = [\"pyobjc>=11 ; sys_platform == 'darwin'\"]\n"
            ),
            "uv.lock": _lock(
                _package(
                    "app",
                    "0.1.0",
                    source='{ editable = "." }',
                    dependencies=(_ref("pyobjc", marker="sys_platform == 'darwin'"),),
                ),
                _package("pyobjc", "12.2.2", source=f'{{ registry = "{PYPI}" }}'),
            ),
        }
    )

    edge = _direct(result, "pyobjc")[0]
    assert edge.condition == 'sys_platform == "darwin"'
    assert edge.resolution is Resolution.CONDITIONAL
    assert edge.resolved_version == "12.2.2"


def test_workspace_members_and_path_dependencies() -> None:
    result = _extract(
        {
            "pyproject.toml": '[tool.uv.workspace]\nmembers = ["a", "b"]\n',
            "a/pyproject.toml": (
                "[project]\nname = \"a\"\nversion = \"0.1.0\"\n"
                "dependencies = [\"b\"]\n"
            ),
            "b/pyproject.toml": "[project]\nname = \"b\"\nversion = \"0.2.0\"\n",
            "uv.lock": _lock(
                _package(
                    "a",
                    "0.1.0",
                    source='{ editable = "a" }',
                    dependencies=(_ref("b"),),
                ),
                _package("b", "0.2.0", source='{ editable = "b" }'),
            ),
        }
    )

    assert result.issues == ()
    assert [project.id for project in result.projects] == ["a:pypi", "b:pypi"]
    b = _instances(result, "b", Ecosystem.GENERIC)
    assert len(b) == 1
    assert b[0].source_kind is SourceKind.WORKSPACE
    assert b[0].first_party is True
    assert b[0].version == "0.2.0"
    edge = _direct(result, "b", Ecosystem.GENERIC)[0]
    assert edge.from_id == "a:pypi"
    assert edge.resolution is Resolution.RESOLVED


def test_workspace_source_inheritance_without_a_lock() -> None:
    result = _extract(
        {
            "pyproject.toml": (
                "[tool.uv.workspace]\nmembers = [\"a\", \"b\"]\n\n"
                "[tool.uv.sources]\nb = { path = \"b\" }\n"
            ),
            "a/pyproject.toml": (
                "[project]\nname = \"a\"\nversion = \"0.1.0\"\n"
                "dependencies = [\"b\"]\n"
            ),
            "b/pyproject.toml": "[project]\nname = \"b\"\nversion = \"0.2.0\"\n",
        }
    )

    assert [issue.kind for issue in result.issues] == [CoverageKind.MISSING_LOCK]
    assert not [issue for issue in result.issues if issue.kind is CoverageKind.DYNAMIC_VALUE]
    edge = _direct(result, "b", Ecosystem.GENERIC)[0]
    assert edge.from_id == "a:pypi"
    assert edge.resolution is Resolution.RESOLVED
    assert edge.resolved_version == "0.2.0"


def test_tool_uv_sources_git_path_url_and_editable_without_a_lock() -> None:
    result = _extract(
        {
            "pyproject.toml": (
                "[project]\nname = \"app\"\nversion = \"0.1.0\"\n"
                "dependencies = [\"gitdep\", \"pathdep\", \"urldep\", \"editdep\"]\n\n"
                "[tool.uv.sources]\n"
                f"gitdep = {{ git = \"https://github.com/acme/gitdep\", rev = \"{SHA}\" }}\n"
                "pathdep = { path = \"libs/pathdep\" }\n"
                "editdep = { path = \"libs/editdep\", editable = true }\n"
                "urldep = { url = \"https://example.com/urldep-1.0-py3-none-any.whl\" }\n"
            ),
            "libs/pathdep/pyproject.toml": "[project]\nname = \"pathdep\"\nversion = \"0.3.0\"\n",
            "libs/editdep/pyproject.toml": "[project]\nname = \"editdep\"\nversion = \"0.4.0\"\n",
        }
    )

    assert [issue.kind for issue in result.issues] == [CoverageKind.MISSING_LOCK]

    git = _instances(result, "gitdep")[0]
    assert git.source_kind is SourceKind.GIT
    assert git.vcs_url == "https://github.com/acme/gitdep"
    assert git.commit == SHA
    assert git.purl == "pkg:pypi/gitdep?vcs_url=https%3A%2F%2Fgithub.com%2Facme%2Fgitdep"
    assert _direct(result, "gitdep")[0].resolution is Resolution.RESOLVED

    path = _instances(result, "pathdep", Ecosystem.GENERIC)[0]
    assert path.source_kind is SourceKind.PATH
    assert path.version == "0.3.0"
    assert path.first_party is True
    assert _direct(result, "pathdep", Ecosystem.GENERIC)[0].resolution is Resolution.RESOLVED

    editable = _instances(result, "editdep", Ecosystem.GENERIC)[0]
    assert editable.source_kind is SourceKind.PATH
    assert editable.version == "0.4.0"
    assert editable.first_party is True

    url = _instances(result, "urldep")[0]
    assert url.source_kind is SourceKind.URL
    assert url.component == (
        "pkg:pypi/urldep?download_url=https%3A%2F%2Fexample.com%2Furldep-1.0-py3-none-any.whl"
    )
    url_edge = _direct(result, "urldep")[0]
    assert url_edge.resolution is Resolution.DECLARED_ONLY
    assert url_edge.resolved_version is None


def test_multiple_versions_per_resolution_marker() -> None:
    result = _extract(
        {
            "pyproject.toml": (
                "[project]\nname = \"app\"\nversion = \"0.1.0\"\n"
                "dependencies = [\"foo>=1\"]\n"
            ),
            "uv.lock": _lock(
                _package(
                    "app",
                    "0.1.0",
                    source='{ editable = "." }',
                    dependencies=(_ref("foo", version="1.0.0"),),
                    markers=("sys_platform == 'darwin'",),
                ),
                _package(
                    "app",
                    "0.1.0",
                    source='{ editable = "." }',
                    dependencies=(_ref("foo", version="2.0.0"),),
                    markers=("sys_platform == 'linux'",),
                ),
                _package(
                    "foo",
                    "1.0.0",
                    source=f'{{ registry = "{PYPI}" }}',
                    markers=("sys_platform == 'darwin'",),
                ),
                _package(
                    "foo",
                    "2.0.0",
                    source=f'{{ registry = "{PYPI}" }}',
                    markers=("sys_platform == 'linux'",),
                ),
            ),
        }
    )

    assert result.issues == ()
    edges = _direct(result, "foo")
    assert len(edges) == 2
    by_version = {edge.resolved_version: edge for edge in edges}
    assert by_version["1.0.0"].condition == 'sys_platform == "darwin"'
    assert by_version["2.0.0"].condition == 'sys_platform == "linux"'
    assert all(edge.resolution is Resolution.CONDITIONAL for edge in edges)


def test_same_version_under_two_markers_gets_distinct_contexts() -> None:
    result = _extract(
        {
            "pyproject.toml": (
                "[project]\nname = \"app\"\nversion = \"0.1.0\"\n"
                "dependencies = [\"foo\"]\n"
            ),
            "uv.lock": _lock(
                _package(
                    "app",
                    "0.1.0",
                    source='{ editable = "." }',
                    dependencies=(_ref("foo"),),
                    markers=("sys_platform == 'darwin'",),
                ),
                _package(
                    "app",
                    "0.1.0",
                    source='{ editable = "." }',
                    dependencies=(_ref("foo"),),
                    markers=("sys_platform == 'linux'",),
                ),
                _package(
                    "foo",
                    "1.0.0",
                    source=f'{{ registry = "{PYPI}" }}',
                    markers=("sys_platform == 'darwin'",),
                ),
                _package(
                    "foo",
                    "1.0.0",
                    source=f'{{ registry = "{PYPI}" }}',
                    markers=("sys_platform == 'linux'",),
                ),
            ),
        }
    )

    instances = _instances(result, "foo")
    assert len(instances) == 2
    assert len({instance.context for instance in instances}) == 2
    assert all(instance.context for instance in instances)
    assert len({instance.id for instance in instances}) == 2
    assert {edge.to_id for edge in _direct(result, "foo")} == {instance.id for instance in instances}


def test_requirements_txt_is_declared_only() -> None:
    result = _extract(
        {
            "requirements.txt": (
                "# Python build requirements\n"
                "--index-url https://example.com/simple\n"
                "ast_serialize==0.7.0\n"
                "cryptography==50.0.0\n"
            )
        }
    )

    assert [project.id for project in result.projects] == [".:pypi"]
    assert [issue.kind for issue in result.issues] == [CoverageKind.MISSING_LOCK]
    edges = [edge for edge in result.edges if edge.from_id == ".:pypi"]
    assert len(edges) == 2
    assert {edge.resolution for edge in edges} == {Resolution.DECLARED_ONLY}
    assert {edge.constraint for edge in edges} == {"==0.7.0", "==50.0.0"}
    assert {edge.evidence[0].locator for edge in edges} == {"L3", "L4"}
    assert all(edge.evidence[0].path == "requirements.txt" for edge in edges)
    instance = _instances(result, "ast_serialize")[0]
    assert instance.component == "pkg:pypi/ast-serialize"
    assert instance.version is None


def test_requirements_txt_merges_into_a_pyproject_project() -> None:
    result = _extract(
        {
            "pyproject.toml": (
                "[project]\nname = \"app\"\nversion = \"0.1.0\"\n"
                "dependencies = [\"pytest>=9\"]\n"
            ),
            "requirements.txt": "pluggy==1.7.0\n",
            "uv.lock": _lock(
                _package("app", "0.1.0", source='{ editable = "." }', dependencies=(_ref("pytest"),)),
                _package("pytest", "9.2.0", source=f'{{ registry = "{PYPI}" }}'),
            ),
        }
    )

    assert [project.id for project in result.projects] == [".:pypi"]
    assert not [issue for issue in result.issues if issue.kind is CoverageKind.MISSING_LOCK]
    pluggy = _direct(result, "pluggy")
    assert len(pluggy) == 1
    assert pluggy[0].resolution is Resolution.DECLARED_ONLY
    assert pluggy[0].evidence[0].path == "requirements.txt"


def test_lock_mismatch_is_unknown_and_typed() -> None:
    result = _extract(
        {
            "pyproject.toml": (
                "[project]\nname = \"app\"\nversion = \"0.1.0\"\n"
                "dependencies = [\"anyio>=4\"]\n"
            ),
            "uv.lock": _lock(_package("app", "0.1.0", source='{ editable = "." }')),
        }
    )

    issues = [issue for issue in result.issues if issue.kind is CoverageKind.LOCK_MISMATCH]
    assert len(issues) == 1
    assert issues[0].locator == "project.dependencies[0]"
    edge = _direct(result, "anyio")[0]
    assert edge.resolution is Resolution.UNKNOWN
    assert edge.resolved_version is None
    assert _instances(result, "anyio")[0].version is None


def test_missing_lock_is_declared_only() -> None:
    result = _extract(
        {
            "pyproject.toml": (
                "[project]\nname = \"app\"\nversion = \"0.1.0\"\n"
                "dependencies = [\"pytest>=9\"]\n"
            )
        }
    )

    assert [issue.kind for issue in result.issues] == [CoverageKind.MISSING_LOCK]
    assert result.issues[0].path == "uv.lock"
    edge = _direct(result, "pytest")[0]
    assert edge.resolution is Resolution.DECLARED_ONLY
    assert edge.constraint == ">=9"


def test_lock_sources_produce_distinct_identities() -> None:
    result = _extract(
        {
            "pyproject.toml": (
                "[project]\nname = \"app\"\nversion = \"0.1.0\"\n"
                "dependencies = [\"mirrored\", \"native\", \"gitdep\", \"urldep\"]\n"
            ),
            "uv.lock": _lock(
                _package(
                    "app",
                    "0.1.0",
                    source='{ editable = "." }',
                    dependencies=(
                        _ref("mirrored"),
                        _ref("native"),
                        _ref("gitdep"),
                        _ref("urldep"),
                    ),
                ),
                _package("mirrored", "1.0.0", source=f'{{ registry = "{MIRROR}" }}'),
                _package("native", "1.0.0", source='{ registry = "https://example.com/simple" }'),
                _package(
                    "gitdep",
                    "0.2.0",
                    source=f'{{ git = "https://github.com/acme/gitdep?rev={SHA}#{SHA}" }}',
                ),
                _package("urldep", "0.5.0", source='{ url = "https://example.com/urldep-0.5.0.whl" }'),
            ),
        },
        index=MIRROR,
    )

    mirrored = _instances(result, "mirrored")[0]
    assert mirrored.purl == "pkg:pypi/mirrored@1.0.0"
    assert mirrored.registry_url == MIRROR
    assert mirrored.repository_url is None

    native = _instances(result, "native")[0]
    assert native.component == "pkg:pypi/native?repository_url=https%3A%2F%2Fexample.com%2Fsimple"
    assert native.repository_url == "https://example.com/simple"

    git = _instances(result, "gitdep")[0]
    assert git.source_kind is SourceKind.GIT
    assert git.vcs_url == "https://github.com/acme/gitdep"
    assert git.commit == SHA

    url = _instances(result, "urldep")[0]
    assert url.source_kind is SourceKind.URL
    assert url.component == "pkg:pypi/urldep?download_url=https%3A%2F%2Fexample.com%2Furldep-0.5.0.whl"


def test_vendored_project_marks_edges_and_instances() -> None:
    result = _extract(
        {
            "vendor/foo/pyproject.toml": (
                "[project]\nname = \"foo\"\nversion = \"0.1.0\"\n"
                "dependencies = [\"pytest>=9\"]\n"
            )
        },
        vendored=("vendor/foo/pyproject.toml",),
    )

    assert [issue.kind for issue in result.issues] == [CoverageKind.MISSING_LOCK]
    assert result.issues[0].path == "vendor/foo/uv.lock"
    edge = _direct(result, "pytest")[0]
    assert edge.source_kind is SourceKind.VENDORED
    assert edge.from_id == "vendor/foo:pypi"
    assert _instances(result, "pytest")[0].source_kind is SourceKind.VENDORED


def test_invalid_inputs_are_parse_errors() -> None:
    result = _extract(
        {
            "pyproject.toml": "[project\nname =",
            "uv.lock": "version = 1\n\n[[package]]\nname = \"foo\"\n",
            "requirements.txt": "not a requirement @@@\n",
        }
    )

    errors = [issue for issue in result.issues if issue.kind is CoverageKind.PARSE_ERROR]
    assert {issue.path for issue in errors} == {"pyproject.toml", "uv.lock", "requirements.txt"}


def test_malformed_lock_dependency_is_a_parse_error() -> None:
    result = _extract(
        {
            "pyproject.toml": "[project]\nname = \"app\"\nversion = \"0.1.0\"\n",
            "uv.lock": _lock(
                "[[package]]\nname = \"app\"\nversion = \"0.1.0\"\nsource = { editable = \".\" }\n"
                "dependencies = [\"not-a-table\"]\n"
            ),
        }
    )

    errors = [issue for issue in result.issues if issue.kind is CoverageKind.PARSE_ERROR]
    assert len(errors) == 1
    assert errors[0].path == "uv.lock"


def test_pypi_names_are_normalized_with_a_display_name() -> None:
    result = _extract(
        {
            "pyproject.toml": (
                "[project]\nname = \"app\"\nversion = \"0.1.0\"\n"
                "dependencies = [\"Mypy_Extensions>=1\"]\n"
            ),
            "uv.lock": _lock(
                _package("app", "0.1.0", source='{ editable = "." }', dependencies=(_ref("Mypy_Extensions"),)),
                _package("Mypy_Extensions", "1.1.1", source=f'{{ registry = "{PYPI}" }}'),
            ),
        }
    )

    instance = _instances(result, "mypy-extensions")[0]
    assert instance.purl == "pkg:pypi/mypy-extensions@1.1.1"
    component = next(
        item for item in result.components if item.purl == "pkg:pypi/mypy-extensions"
    )
    assert component.name == "mypy-extensions"
    assert component.display_name == "Mypy_Extensions"


def test_python_adapter_matches_uv_inputs() -> None:
    adapter = PythonAdapter()
    assert adapter.matches("pyproject.toml")
    assert adapter.matches("crates/foo/pyproject.toml")
    assert adapter.matches("uv.lock")
    assert adapter.matches("requirements.txt")
    assert adapter.matches("requirements-dev.txt")
    assert not adapter.matches("requirements.in")
    assert not adapter.matches("pyproject.toml.bak")
    assert not adapter.matches("uv.lock.bak")
    assert not adapter.matches("setup.py")


def test_extraction_is_deterministic() -> None:
    files = {
        "pyproject.toml": (
            "[project]\nname = \"app\"\nversion = \"0.1.0\"\n"
            "dependencies = [\"pytest>=9\", \"anyio>=4\"]\n"
        ),
        "uv.lock": _lock(
            _package(
                "app",
                "0.1.0",
                source='{ editable = "." }',
                dependencies=(_ref("pytest"), _ref("anyio")),
            ),
            _package("pytest", "9.2.0", source=f'{{ registry = "{PYPI}" }}'),
            _package("anyio", "4.15.1", source=f'{{ registry = "{PYPI}" }}'),
        ),
    }

    result = _extract(files)
    assert _extract(files) == result
    assert len(result.edges) == 2
