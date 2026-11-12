from __future__ import annotations

import json

from repomod.dependencies.adapters.javascript import JavaScriptAdapter
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

PUBLIC = "https://registry.npmjs.org/"
MIRROR = "https://npm.17166969.xyz/current/"
SHA = "770427abdcafc516531dd275423ceee0c0b1fb2e"


def _pnpm(*documents: str) -> str:
    return "\n---\n".join(document.strip("\n") for document in documents) + "\n"


def _npm(packages: dict[str, object], version: int = 3) -> str:
    return json.dumps({"lockfileVersion": version, "packages": packages}, indent=1)


def _extract(
    files: dict[str, str],
    *,
    registry: str | None = PUBLIC,
    vendored: tuple[str, ...] = (),
) -> AdapterResult:
    adapter = JavaScriptAdapter(registry=registry)
    inputs = tuple(
        InputContent(path, text, path in vendored) for path, text in files.items()
    )
    return adapter.extract(inputs)


def _instances(
    result: AdapterResult, name: str, ecosystem: Ecosystem = Ecosystem.NPM
) -> list[Instance]:
    base = purl(ecosystem, name)
    return [
        instance
        for instance in result.instances
        if instance.component == base or instance.component.startswith(base + "?")
    ]


def _direct(
    result: AdapterResult, name: str, ecosystem: Ecosystem = Ecosystem.NPM
) -> list[Edge]:
    targets = {instance.id for instance in _instances(result, name, ecosystem)}
    return [
        edge for edge in result.edges if edge.relation is Relation.DIRECT and edge.to_id in targets
    ]


def _transitive(
    result: AdapterResult, name: str, ecosystem: Ecosystem = Ecosystem.NPM
) -> list[Edge]:
    targets = {instance.id for instance in _instances(result, name, ecosystem)}
    return [
        edge
        for edge in result.edges
        if edge.relation is Relation.TRANSITIVE and edge.to_id in targets
    ]


def test_two_document_pnpm_lock_separates_toolchain_and_project() -> None:
    result = _extract(
        {
            "pnpm-lock.yaml": _pnpm(
                """
lockfileVersion: '9.0'
importers:
  .:
    packageManagerDependencies:
      pnpm:
        specifier: 10.0.0
        version: 10.0.0
packages:
  pnpm@10.0.0:
    resolution: {integrity: sha512-pnpm}
snapshots:
  pnpm@10.0.0: {}
""",
                """
lockfileVersion: '9.0'
importers:
  .:
    dependencies:
      react:
        specifier: ^19.0.0
        version: 19.0.0
packages:
  react@19.0.0:
    resolution: {integrity: sha512-react}
snapshots:
  react@19.0.0: {}
""",
            )
        }
    )

    assert result.issues == ()
    assert [project.id for project in result.projects] == [".:npm"]
    toolchain = _direct(result, "pnpm")
    assert len(toolchain) == 1
    assert toolchain[0].scope is Scope.TOOLCHAIN
    assert toolchain[0].resolution is Resolution.RESOLVED
    react = _direct(result, "react")
    assert len(react) == 1
    assert react[0].scope is Scope.RUNTIME
    assert react[0].constraint == "^19.0.0"
    assert react[0].resolved_version == "19.0.0"
    assert [(item.path, item.locator) for item in react[0].evidence] == [
        ("pnpm-lock.yaml", "importers[.].dependencies.react")
    ]
    instance = _instances(result, "react")[0]
    assert instance.purl == "pkg:npm/react@19.0.0"
    assert instance.registry_url == PUBLIC
    assert instance.id == "pkg:npm/react@19.0.0#"


def test_multiple_importers_and_versions() -> None:
    result = _extract(
        {
            "pnpm-lock.yaml": _pnpm(
                """
lockfileVersion: '9.0'
importers:
  .:
    dependencies:
      react:
        specifier: ^18.0.0
        version: 18.3.1
  web:
    dependencies:
      react:
        specifier: ^19.0.0
        version: 19.0.0
packages:
  react@18.3.1:
    resolution: {integrity: sha512-react18}
  react@19.0.0:
    resolution: {integrity: sha512-react19}
snapshots:
  react@18.3.1: {}
  react@19.0.0: {}
"""
            )
        }
    )

    assert [project.id for project in result.projects] == [".:npm", "web:npm"]
    assert sorted(instance.version or "" for instance in _instances(result, "react")) == [
        "18.3.1",
        "19.0.0",
    ]
    versions = {edge.from_id: edge.resolved_version for edge in _direct(result, "react")}
    assert versions == {".:npm": "18.3.1", "web:npm": "19.0.0"}


def test_peer_suffixed_snapshots_are_distinct_instances() -> None:
    result = _extract(
        {
            "pnpm-lock.yaml": _pnpm(
                """
lockfileVersion: '9.0'
importers:
  .:
    devDependencies:
      vitest:
        specifier: ^5.0.5
        version: 5.0.5(vite@8.3.5)(@types/node@26.6.6)
packages:
  vitest@5.0.5:
    resolution: {integrity: sha512-vitest}
  vite@8.3.5:
    resolution: {integrity: sha512-vite}
  '@types/node@26.6.6':
    resolution: {integrity: sha512-types}
snapshots:
  vitest@5.0.5(vite@8.3.5)(@types/node@26.6.6):
    dependencies:
      vite: 8.3.5(@types/node@26.6.6)
  vitest@5.0.5: {}
  vite@8.3.5(@types/node@26.6.6): {}
  '@types/node@26.6.6': {}
"""
            )
        }
    )

    vitest = _instances(result, "vitest")
    assert {instance.context for instance in vitest} == {"", "(vite@8.3.5)(@types/node@26.6.6)"}
    edge = _direct(result, "vitest")[0]
    assert edge.to_id == "pkg:npm/vitest@5.0.5#(vite@8.3.5)(@types/node@26.6.6)"
    assert edge.scope is Scope.DEV
    transitive = _transitive(result, "vite")
    assert len(transitive) == 1
    assert transitive[0].from_id == edge.to_id
    assert transitive[0].to_id == "pkg:npm/vite@8.3.5#(@types/node@26.6.6)"
    scoped = _instances(result, "@types/node")[0]
    assert scoped.purl == "pkg:npm/%40types/node@26.6.6"


def test_alias_resolves_to_the_real_package() -> None:
    result = _extract(
        {
            "pnpm-lock.yaml": _pnpm(
                """
lockfileVersion: '9.0'
importers:
  .:
    dependencies:
      foo:
        specifier: npm:bar@^1.0.0
        version: bar@1.2.3
packages:
  bar@1.2.3:
    resolution: {integrity: sha512-bar}
snapshots:
  bar@1.2.3: {}
"""
            )
        }
    )

    edge = _direct(result, "bar")[0]
    assert edge.constraint == "npm:bar@^1.0.0"
    assert edge.resolved_version == "1.2.3"
    assert edge.evidence[0].locator == "importers[.].dependencies.foo"
    assert _instances(result, "foo") == []


def test_link_and_workspace_specs_are_first_party_generic_packages() -> None:
    result = _extract(
        {
            "pnpm-lock.yaml": _pnpm(
                """
lockfileVersion: '9.0'
importers:
  .:
    dependencies:
      local:
        specifier: link:packages/local
        version: link:packages/local
      shared:
        specifier: workspace:*
        version: link:packages/shared
      external:
        specifier: link:../external
        version: link:../external
packages: {}
snapshots: {}
"""
            )
        }
    )

    local = _instances(result, "local", Ecosystem.GENERIC)[0]
    assert local.source_kind is SourceKind.WORKSPACE
    assert local.first_party is True
    assert local.purl == "pkg:generic/local"
    shared = _instances(result, "shared", Ecosystem.GENERIC)[0]
    assert shared.source_kind is SourceKind.WORKSPACE
    external = _instances(result, "external", Ecosystem.GENERIC)[0]
    assert external.source_kind is SourceKind.PATH
    assert external.first_party is False
    assert len(_direct(result, "local", Ecosystem.GENERIC)) == 1


def test_optional_dependencies_are_optional() -> None:
    result = _extract(
        {
            "pnpm-lock.yaml": _pnpm(
                """
lockfileVersion: '9.0'
importers:
  .:
    optionalDependencies:
      fsevents:
        specifier: ^2.3.3
        version: 2.3.3
packages:
  fsevents@2.3.3:
    resolution: {integrity: sha512-fsevents}
snapshots:
  fsevents@2.3.3: {}
"""
            )
        }
    )

    edge = _direct(result, "fsevents")[0]
    assert edge.scope is Scope.OPTIONAL
    assert edge.optional is True


def test_pnpm_overrides_and_ignored_optional_dependencies_are_tolerated() -> None:
    result = _extract(
        {
            "pnpm-lock.yaml": _pnpm(
                """
lockfileVersion: '9.0'
overrides:
  react: 19.0.0
ignoredOptionalDependencies:
  - fsevents
importers:
  .:
    dependencies:
      react:
        specifier: ^19.0.0
        version: 19.0.0
packages:
  react@19.0.0:
    resolution: {integrity: sha512-react}
snapshots:
  react@19.0.0: {}
"""
            )
        }
    )

    assert result.issues == ()
    assert [edge.resolved_version for edge in _direct(result, "react")] == ["19.0.0"]


def test_pnpm_v8_is_unsupported() -> None:
    result = _extract(
        {
            "pnpm-lock.yaml": "lockfileVersion: '6.0'\nimporters:\n  .: {}\n",
        }
    )

    assert [issue.kind for issue in result.issues] == [CoverageKind.UNSUPPORTED_FORMAT]


def test_npm_v3_packages_and_nesting() -> None:
    result = _extract(
        {
            "package-lock.json": _npm(
                {
                    "": {
                        "name": "app",
                        "version": "1.0.0",
                        "dependencies": {"foo": "^1.0.0", "dup": "^2.0.0"},
                        "devDependencies": {"bar": "^2.0.0"},
                        "optionalDependencies": {"fsevents": "^2.3.3"},
                    },
                    "node_modules/foo": {
                        "version": "1.2.3",
                        "resolved": "https://registry.npmjs.org/foo/-/foo-1.2.3.tgz",
                        "dependencies": {"dup": "^1.0.0"},
                    },
                    "node_modules/foo/node_modules/dup": {
                        "version": "1.0.0",
                        "resolved": "https://registry.npmjs.org/dup/-/dup-1.0.0.tgz",
                    },
                    "node_modules/dup": {
                        "version": "2.0.0",
                        "resolved": "https://registry.npmjs.org/dup/-/dup-2.0.0.tgz",
                    },
                    "node_modules/bar": {
                        "version": "2.0.0",
                        "dev": True,
                        "resolved": "https://registry.npmjs.org/bar/-/bar-2.0.0.tgz",
                    },
                    "node_modules/fsevents": {
                        "version": "2.3.3",
                        "optional": True,
                        "resolved": "https://registry.npmjs.org/fsevents/-/fsevents-2.3.3.tgz",
                    },
                }
            )
        }
    )

    assert result.issues == ()
    assert [project.id for project in result.projects] == [".:npm"]
    assert sorted(instance.version or "" for instance in _instances(result, "dup")) == [
        "1.0.0",
        "2.0.0",
    ]
    assert {edge.from_id: edge.resolved_version for edge in _direct(result, "dup")} == {
        ".:npm": "2.0.0"
    }
    transitive = _transitive(result, "dup")
    assert len(transitive) == 1
    assert transitive[0].resolved_version == "1.0.0"
    assert _direct(result, "foo")[0].scope is Scope.RUNTIME
    assert _direct(result, "bar")[0].scope is Scope.DEV
    assert _direct(result, "fsevents")[0].optional is True
    assert _direct(result, "foo")[0].evidence[0].locator == "packages[''].dependencies.foo"


def test_npm_scoped_and_aliased_packages() -> None:
    result = _extract(
        {
            "package-lock.json": _npm(
                {
                    "": {
                        "name": "app",
                        "version": "1.0.0",
                        "dependencies": {"@scope/pkg": "^1.0.0", "alias": "^2.0.0"},
                    },
                    "node_modules/@scope/pkg": {
                        "version": "1.2.0",
                        "resolved": "https://registry.npmjs.org/@scope/pkg/-/pkg-1.2.0.tgz",
                    },
                    "node_modules/alias": {
                        "name": "real-package",
                        "version": "2.1.0",
                        "resolved": "https://registry.npmjs.org/real-package/-/real-package-2.1.0.tgz",
                    },
                }
            )
        }
    )

    scoped = _instances(result, "@scope/pkg")[0]
    assert scoped.purl == "pkg:npm/%40scope/pkg@1.2.0"
    alias = _instances(result, "real-package")[0]
    assert alias.purl == "pkg:npm/real-package@2.1.0"
    assert [edge.to_id for edge in _direct(result, "real-package")] == [alias.id]


def test_npm_workspace_projects() -> None:
    result = _extract(
        {
            "package-lock.json": _npm(
                {
                    "": {"name": "root", "version": "1.0.0", "workspaces": ["packages/*"]},
                    "packages/a": {
                        "name": "a",
                        "version": "0.1.0",
                        "dependencies": {"foo": "^1.0.0"},
                    },
                    "node_modules/a": {"resolved": "packages/a", "link": True},
                    "node_modules/foo": {
                        "version": "1.0.0",
                        "resolved": "https://registry.npmjs.org/foo/-/foo-1.0.0.tgz",
                    },
                }
            )
        }
    )

    assert [project.id for project in result.projects] == [".:npm", "packages/a:npm"]
    edge = [item for item in _direct(result, "foo") if item.from_id == "packages/a:npm"]
    assert len(edge) == 1
    assert edge[0].resolution is Resolution.RESOLVED
    linked = _instances(result, "a", Ecosystem.GENERIC)[0]
    assert linked.source_kind is SourceKind.WORKSPACE
    assert linked.first_party is True


def test_npm_peer_dependency_without_a_target_is_declared_only() -> None:
    result = _extract(
        {
            "package-lock.json": _npm(
                {
                    "": {
                        "name": "app",
                        "version": "1.0.0",
                        "peerDependencies": {"react": "^19.0.0"},
                    }
                }
            )
        }
    )

    edge = _direct(result, "react")[0]
    assert edge.scope is Scope.PEER
    assert edge.resolution is Resolution.DECLARED_ONLY
    assert edge.resolved_version is None


def test_npm_missing_dependency_is_a_parse_error() -> None:
    result = _extract(
        {
            "package-lock.json": _npm(
                {
                    "": {
                        "name": "app",
                        "version": "1.0.0",
                        "dependencies": {"missing": "^1.0.0"},
                    }
                }
            )
        }
    )

    errors = [issue for issue in result.issues if issue.kind is CoverageKind.PARSE_ERROR]
    assert len(errors) == 1
    assert errors[0].locator == "packages[''].dependencies.missing"


def test_non_v3_package_lock_is_unsupported() -> None:
    result = _extract(
        {
            "package-lock.json": _npm({"": {"name": "app"}}, version=2),
        }
    )

    assert [issue.kind for issue in result.issues] == [CoverageKind.UNSUPPORTED_FORMAT]


def test_yarn_and_bun_locks_are_unsupported() -> None:
    result = _extract(
        {
            "yarn.lock": "# yarn lockfile v1\n",
            "bun.lock": "{}\n",
        }
    )

    assert [issue.kind for issue in result.issues] == [
        CoverageKind.UNSUPPORTED_FORMAT,
        CoverageKind.UNSUPPORTED_FORMAT,
    ]
    assert {issue.path for issue in result.issues} == {"yarn.lock", "bun.lock"}


def test_invalid_inputs_are_parse_errors() -> None:
    result = _extract(
        {
            "pnpm-lock.yaml": "lockfileVersion: [\n",
            "package-lock.json": "{",
        }
    )

    assert [issue.kind for issue in result.issues] == [
        CoverageKind.PARSE_ERROR,
        CoverageKind.PARSE_ERROR,
    ]


def test_mirror_and_non_public_registries_are_distinct() -> None:
    result = _extract(
        {
            "pnpm-lock.yaml": _pnpm(
                """
lockfileVersion: '9.0'
importers:
  .:
    dependencies:
      mirrored:
        specifier: ^1.0.0
        version: 1.0.0
packages:
  mirrored@1.0.0:
    resolution: {integrity: sha512-mirrored}
snapshots:
  mirrored@1.0.0: {}
"""
            ),
            "package-lock.json": _npm(
                {
                    "": {
                        "name": "app",
                        "version": "1.0.0",
                        "dependencies": {"private": "^1.0.0"},
                    },
                    "node_modules/private": {
                        "version": "1.0.0",
                        "resolved": "https://example.com/index/private/-/private-1.0.0.tgz",
                    },
                }
            ),
        },
        registry=MIRROR,
    )

    mirrored = _instances(result, "mirrored")[0]
    assert mirrored.purl == "pkg:npm/mirrored@1.0.0"
    assert mirrored.registry_url == MIRROR
    private = _instances(result, "private")[0]
    assert private.component == "pkg:npm/private?repository_url=https%3A%2F%2Fexample.com%2F"
    assert private.repository_url == "https://example.com/"


def test_vendored_lock_marks_refs() -> None:
    result = _extract(
        {
            "vendor/thing/package-lock.json": _npm(
                {
                    "": {"name": "thing", "version": "1.0.0", "dependencies": {"foo": "^1"}},
                    "node_modules/foo": {"version": "1.0.0"},
                }
            )
        },
        vendored=("vendor/thing/package-lock.json",),
    )

    edge = _direct(result, "foo")[0]
    assert edge.source_kind is SourceKind.VENDORED
    assert _instances(result, "foo")[0].source_kind is SourceKind.VENDORED


def test_javascript_adapter_matches_lock_files() -> None:
    adapter = JavaScriptAdapter()
    assert adapter.matches("pnpm-lock.yaml")
    assert adapter.matches("web/pnpm-lock.yaml")
    assert adapter.matches("package-lock.json")
    assert adapter.matches("yarn.lock")
    assert adapter.matches("bun.lock")
    assert adapter.matches("bun.lockb")
    assert not adapter.matches("package.json")
    assert not adapter.matches("pnpm-workspace.yaml")
    assert not adapter.matches("lockfile.yaml")


def test_extraction_is_deterministic() -> None:
    files = {
        "package-lock.json": _npm(
            {
                "": {"name": "app", "version": "1.0.0", "dependencies": {"foo": "^1"}},
                "node_modules/foo": {"version": "1.0.0"},
            }
        )
    }

    result = _extract(files)
    assert _extract(files) == result
    assert len(result.edges) == 1
