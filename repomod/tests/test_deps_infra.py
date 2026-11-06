from __future__ import annotations

from repomod.dependencies.adapters.actions import ActionsAdapter
from repomod.dependencies.adapters.cloudflare import CloudflareAdapter
from repomod.dependencies.adapters.cmake import CmakeAdapter
from repomod.dependencies.adapters.containers import ContainersAdapter
from repomod.dependencies.adapters.submodules import SubmodulesAdapter
from repomod.dependencies.adapters.toolchains import ToolchainsAdapter
from repomod.dependencies.adapters.vendored import is_vendored, vendored_roots
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

SHA = "3d3c42e5aac5ba805825da76410c181273ba90b1"
SHA256 = "9f4199d4e90b4b125738390ac737c24f5ba6d2335205008c5288bbfe3fc9ddc9"
DIGEST = "sha256:020dfcbaaf4cc1078bf2d9c7ba31a8466e334061dcd2f248001d68f79e52c000"

type Adapter = (
    ActionsAdapter
    | CloudflareAdapter
    | CmakeAdapter
    | ContainersAdapter
    | SubmodulesAdapter
    | ToolchainsAdapter
)


def _extract(
    adapter: Adapter,
    files: dict[str, str],
    *,
    vendored: tuple[str, ...] = (),
    gitlinks: tuple[tuple[str, str], ...] = (),
) -> AdapterResult:
    inputs = tuple(
        InputContent(path, text, path in vendored, gitlinks if path.endswith(".gitmodules") else ())
        for path, text in files.items()
    )
    return adapter.extract(inputs)


def _instances(
    result: AdapterResult, name: str, ecosystem: Ecosystem = Ecosystem.GENERIC
) -> list[Instance]:
    base = purl(ecosystem, name)
    return [
        instance
        for instance in result.instances
        if instance.component == base or instance.component.startswith(base + "?")
    ]


def _component(result: AdapterResult, component: str) -> Instance:
    return next(instance for instance in result.instances if instance.component == component)


def _direct(
    result: AdapterResult, name: str, ecosystem: Ecosystem = Ecosystem.GENERIC
) -> list[Edge]:
    targets = {instance.id for instance in _instances(result, name, ecosystem)}
    return [
        edge for edge in result.edges if edge.relation is Relation.DIRECT and edge.to_id in targets
    ]


def test_workflow_actions_are_resolved_with_comments() -> None:
    workflow = (
        "jobs:\n"
        "  test:\n"
        "    runs-on: ubuntu-latest\n"
        "    steps:\n"
        f"      - uses: actions/checkout@{SHA} # v4.1.0\n"
        "      - uses: dtolnay/rust-toolchain@stable\n"
        "      - uses: ./.github/actions/local\n"
        "      - uses: docker://alpine:3.18\n"
    )
    result = _extract(ActionsAdapter(), {".github/workflows/ci.yml": workflow})

    assert result.issues == ()
    checkout = _instances(result, "actions/checkout", Ecosystem.GITHUB)[0]
    assert checkout.commit == SHA
    checkout_edge = _direct(result, "actions/checkout", Ecosystem.GITHUB)[0]
    assert checkout_edge.constraint == "v4.1.0"
    assert checkout_edge.resolution is Resolution.RESOLVED

    floating = _direct(result, "dtolnay/rust-toolchain", Ecosystem.GITHUB)[0]
    assert floating.constraint == "stable"
    assert floating.resolution is Resolution.DECLARED_ONLY

    local = _instances(result, ".github/actions/local")[0]
    assert local.source_kind is SourceKind.PATH
    assert local.first_party is True

    image = _instances(result, "alpine", Ecosystem.DOCKER)[0]
    assert image.version == "3.18"
    assert _direct(result, "alpine", Ecosystem.DOCKER)[0].scope is Scope.IMAGE


def test_reusable_workflow_and_expressions() -> None:
    workflow = (
        "jobs:\n"
        "  call:\n"
        "    uses: owner/repo/.github/workflows/ci.yml@v1\n"
        "  dynamic:\n"
        "    uses: ${{ inputs.repo }}/.github/workflows/ci.yml@v1\n"
    )
    result = _extract(ActionsAdapter(), {".github/workflows/ci.yml": workflow})

    instance = _component(result, "pkg:github/owner/repo#.github/workflows/ci.yml")
    assert instance.version == "v1"
    edge = next(item for item in result.edges if item.to_id == instance.id)
    assert edge.scope is Scope.CI
    assert edge.resolution is Resolution.DECLARED_ONLY
    assert [issue.kind for issue in result.issues] == [CoverageKind.DYNAMIC_VALUE]


def test_container_and_service_images() -> None:
    workflow = (
        "jobs:\n"
        "  build:\n"
        "    container:\n"
        f"      image: ghcr.io/owner/image:1.0@{DIGEST}\n"
        "    services:\n"
        "      redis: redis:7\n"
    )
    result = _extract(ActionsAdapter(), {".github/workflows/ci.yml": workflow})

    container = _instances(result, "owner/image", Ecosystem.DOCKER)[0]
    assert container.version == "1.0"
    assert container.component == (
        "pkg:docker/owner/image?digest=sha256%3A020dfcbaaf4cc1078bf2d9c7ba31a8466e334061dcd2f248001d68f79e52c000"
        "&repository_url=https%3A%2F%2Fghcr.io%2F"
    )
    container_edge = _direct(result, "owner/image", Ecosystem.DOCKER)[0]
    assert container_edge.resolution is Resolution.RESOLVED
    redis = _direct(result, "redis", Ecosystem.DOCKER)[0]
    assert redis.resolution is Resolution.DECLARED_ONLY


def test_workflow_artifacts_pinned_and_unpinned() -> None:
    workflow = (
        "jobs:\n"
        "  build:\n"
        "    steps:\n"
        "      - run: |\n"
        f"          curl -fsSL https://example.com/tool.tar.gz -o tool.tar.gz && echo {SHA256} | sha256sum -c -\n"
        "          curl -fsSL https://example.com/data.zip -o data.zip\n"
        "          curl -fsSL https://example.com/${VERSION}/dynamic.tar.gz\n"
    )
    result = _extract(ActionsAdapter(), {".github/workflows/ci.yml": workflow})

    pinned = _direct(result, "tool.tar.gz", Ecosystem.GENERIC)[0]
    assert pinned.resolution is Resolution.RESOLVED
    unpinned = _direct(result, "data.zip", Ecosystem.GENERIC)[0]
    assert unpinned.resolution is Resolution.DECLARED_ONLY
    dynamic = [issue for issue in result.issues if issue.kind is CoverageKind.DYNAMIC_VALUE]
    assert len(dynamic) == 1
    assert "dynamic.tar.gz" in dynamic[0].message


def test_artifact_userinfo_is_redacted() -> None:
    workflow = (
        "jobs:\n"
        "  build:\n"
        "    steps:\n"
        "      - run: curl -fsSL https://user:secret@example.com/tool.tar.gz -o tool.tar.gz\n"
    )
    result = _extract(ActionsAdapter(), {".github/workflows/ci.yml": workflow})

    instance = _instances(result, "tool.tar.gz")[0]
    assert "secret" not in instance.purl
    assert "user" not in instance.purl
    assert instance.component == (
        "pkg:generic/tool.tar.gz?download_url=https%3A%2F%2Fexample.com%2Ftool.tar.gz"
    )


def test_invalid_workflow_is_a_parse_error() -> None:
    result = _extract(ActionsAdapter(), {".github/workflows/ci.yml": "jobs: ["})

    assert [issue.kind for issue in result.issues] == [CoverageKind.PARSE_ERROR]


def test_dockerfile_digest_arg_and_multi_stage() -> None:
    dockerfile = (
        "ARG BASE=alpine:3.24\n"
        f"FROM ${{BASE}}@{DIGEST} AS build\n"
        "FROM scratch AS final\n"
        "COPY --from=build /out /out\n"
        "COPY --from=busybox:1.36 /bin/busybox /bin/busybox\n"
    )
    result = _extract(ContainersAdapter(), {"Dockerfile": dockerfile})

    assert result.issues == ()
    alpine = _instances(result, "alpine", Ecosystem.DOCKER)[0]
    assert alpine.version == "3.24"
    assert alpine.component == (
        "pkg:docker/alpine?digest=sha256%3A020dfcbaaf4cc1078bf2d9c7ba31a8466e334061dcd2f248001d68f79e52c000"
    )
    edge = _direct(result, "alpine", Ecosystem.DOCKER)[0]
    assert edge.resolution is Resolution.RESOLVED
    assert edge.scope is Scope.IMAGE
    busybox = _direct(result, "busybox", Ecosystem.DOCKER)[0]
    assert busybox.resolution is Resolution.DECLARED_ONLY


def test_dockerfile_apk_pins() -> None:
    dockerfile = "FROM alpine:3.24\nRUN apk add --no-cache bash=5.3.9-r1 curl file\n"
    result = _extract(ContainersAdapter(), {"Dockerfile": dockerfile})

    bash = _instances(result, "bash", Ecosystem.APK)[0]
    assert bash.version == "5.3.9-r1"
    assert _direct(result, "bash", Ecosystem.APK)[0].resolution is Resolution.RESOLVED
    assert _direct(result, "bash", Ecosystem.APK)[0].scope is Scope.SYSTEM
    curl = _direct(result, "curl", Ecosystem.APK)[0]
    assert curl.resolution is Resolution.DECLARED_ONLY
    assert curl.resolved_version is None


def test_dockerfile_unresolved_from_is_dynamic() -> None:
    result = _extract(
        ContainersAdapter(), {"Dockerfile": "FROM alpine-${TARGETARCH}\n"}
    )

    dynamic = [issue for issue in result.issues if issue.kind is CoverageKind.DYNAMIC_VALUE]
    assert len(dynamic) == 1
    assert "TARGETARCH" in dynamic[0].message


def test_dockerfile_artifacts() -> None:
    dockerfile = (
        "FROM alpine:3.24\n"
        f"RUN curl -fsSL https://example.com/tool.tar.gz && echo {SHA256} | sha256sum -c -\n"
        "RUN curl -fsSL https://example.com/other.tar.gz\n"
    )
    result = _extract(ContainersAdapter(), {"Dockerfile": dockerfile})

    pinned = _direct(result, "tool.tar.gz")[0]
    assert pinned.resolution is Resolution.RESOLVED
    assert pinned.scope is Scope.BUILD
    unpinned = _direct(result, "other.tar.gz")[0]
    assert unpinned.resolution is Resolution.DECLARED_ONLY


def test_toolchain_files() -> None:
    result = _extract(
        ToolchainsAdapter(),
        {
            "rust-toolchain.toml": "[toolchain]\nchannel = \"nightly-2026-09-15\"\n",
            ".node-version": "v22.14.0\n",
            ".python-version": "3.15.1\n3.14.0\n",
            ".tool-versions": "nodejs 22.14.0\npython 3.15.1\n",
            "package.json": '{"packageManager": "pnpm@10.0.0+sha512.abc"}',
        },
    )

    rust = _instances(result, "rust")[0]
    assert rust.version == "nightly-2026-09-15"
    node = _instances(result, "node")[0]
    assert node.version == "v22.14.0"
    python = _instances(result, "python")
    assert sorted(instance.version or "" for instance in python) == ["3.14.0", "3.15.1"]
    assert _instances(result, "nodejs")[0].version == "22.14.0"
    pnpm = _instances(result, "pnpm")[0]
    assert pnpm.version == "10.0.0"
    assert _direct(result, "pnpm")[0].resolution is Resolution.RESOLVED
    assert _direct(result, "pnpm")[0].scope is Scope.TOOLCHAIN


def test_toolchain_aliases_are_declared_only() -> None:
    result = _extract(ToolchainsAdapter(), {".nvmrc": "lts/*\n"})

    node = _direct(result, "node")
    assert len(node) == 1
    assert node[0].resolution is Resolution.DECLARED_ONLY


def test_invalid_package_json_is_a_parse_error() -> None:
    result = _extract(ToolchainsAdapter(), {"package.json": "{"})

    assert [issue.kind for issue in result.issues] == [CoverageKind.PARSE_ERROR]


def test_gitmodules_edges_use_gitlink_commits() -> None:
    gitmodules = (
        "[submodule \"a\"]\n"
        "\tpath = sub/a\n"
        "\turl = https://github.com/owner/a.git\n"
        "\tbranch = main\n"
        "[submodule \"b\"]\n"
        "\tpath = sub/b\n"
        "\turl = https://git.example.com/owner/b.git\n"
    )
    result = _extract(
        SubmodulesAdapter(),
        {".gitmodules": gitmodules},
        gitlinks=(("sub/a", SHA),),
    )

    assert result.issues == ()
    first = _instances(result, "owner/a", Ecosystem.GITHUB)[0]
    assert first.commit == SHA
    first_edge = _direct(result, "owner/a", Ecosystem.GITHUB)[0]
    assert first_edge.constraint == "main"
    assert first_edge.resolution is Resolution.RESOLVED
    second = _instances(result, "b")[0]
    assert second.component == (
        "pkg:generic/b?vcs_url=https%3A%2F%2Fgit.example.com%2Fowner%2Fb.git"
    )
    assert _direct(result, "b")[0].resolution is Resolution.DECLARED_ONLY


def test_submodule_userinfo_is_redacted() -> None:
    gitmodules = (
        "[submodule \"a\"]\n"
        "\tpath = sub/a\n"
        "\turl = https://user:secret@git.example.com/owner/a.git\n"
    )
    result = _extract(SubmodulesAdapter(), {".gitmodules": gitmodules})

    instance = _instances(result, "a")[0]
    assert "secret" not in instance.purl
    assert "user" not in instance.purl
    assert instance.vcs_url == "https://git.example.com/owner/a.git"


def test_vendored_helpers() -> None:
    paths = ("vendor/a/Cargo.toml", "third_party/b/x", "third-party/c/y", "src/x")
    roots = vendored_roots(paths)
    assert roots == ("third-party", "third_party", "vendor")
    assert is_vendored("vendor/a/Cargo.toml", roots) is True
    assert is_vendored("src/x", roots) is False


def test_cmake_literals_and_dynamic_declarations() -> None:
    cmake = (
        "cmake_minimum_required(VERSION 3.30)\n"
        "find_package(ZLIB 1.3 REQUIRED)\n"
        "pkg_check_modules(GLIB REQUIRED glib-2.0>=2.80 gio-2.0)\n"
        "FetchContent_Declare(\n"
        "  fmt\n"
        "  URL https://example.com/fmt-11.0.0.tar.gz\n"
        f"  URL_HASH SHA256={SHA256}\n"
        ")\n"
        "FetchContent_Declare(dynamic URL ${BASE}/x.tar.gz)\n"
        "FetchContent_Declare(\n"
        "  gitdep\n"
        "  GIT_REPOSITORY https://github.com/foo/gitdep.git\n"
        f"  GIT_TAG {SHA}\n"
        ")\n"
    )
    result = _extract(CmakeAdapter(), {"CMakeLists.txt": cmake})

    zlib = _direct(result, "ZLIB")[0]
    assert zlib.scope is Scope.SYSTEM
    assert zlib.constraint == "1.3"
    assert zlib.resolution is Resolution.DECLARED_ONLY
    glib = _direct(result, "glib-2.0")[0]
    assert glib.constraint == "2.80"
    gio = _direct(result, "gio-2.0")[0]
    assert gio.resolution is Resolution.DECLARED_ONLY
    fmt = _direct(result, "fmt")[0]
    assert fmt.resolution is Resolution.RESOLVED
    assert fmt.scope is Scope.BUILD
    assert fmt.constraint == f"SHA256={SHA256}"
    gitdep = _direct(result, "gitdep")[0]
    assert gitdep.resolution is Resolution.RESOLVED
    assert _instances(result, "gitdep")[0].commit == SHA
    dynamic = [issue for issue in result.issues if issue.kind is CoverageKind.DYNAMIC_VALUE]
    assert len(dynamic) == 1
    assert "dynamic" in dynamic[0].message


def test_cmake_comments_are_ignored() -> None:
    cmake = (
        "# find_package(NOTREAL REQUIRED)\n"
        "include(FetchContent)\n"
        "# FetchContent_Declare(ghostty GIT_REPOSITORY https://example.com/x GIT_TAG main)\n"
    )
    result = _extract(CmakeAdapter(), {"CMakeLists.txt": cmake})

    assert result.issues == ()
    assert result.edges == ()


def test_wrangler_toml_bindings() -> None:
    wrangler = (
        'name = "worker"\n\n'
        "[[d1_databases]]\n"
        'binding = "DB"\n'
        'database_name = "main"\n'
        'database_id = "abc"\n\n'
        "[[kv_namespaces]]\n"
        'binding = "CACHE"\n'
        'id = "xyz"\n\n'
        "[[r2_buckets]]\n"
        'binding = "BUCKET"\n'
        'bucket_name = "assets"\n\n'
        "[durable_objects]\n"
        "[[durable_objects.bindings]]\n"
        'name = "ROOM"\n'
        'class_name = "Room"\n\n'
        "[[services]]\n"
        'binding = "API"\n'
        'service = "api-worker"\n\n'
        "[[queues.producers]]\n"
        'binding = "JOBS"\n'
        'queue = "jobs"\n\n'
        "[[queues.consumers]]\n"
        'queue = "jobs"\n'
    )
    result = _extract(CloudflareAdapter(), {"wrangler.toml": wrangler})

    assert result.issues == ()
    assert [edge.scope for edge in result.edges] == [Scope.SERVICE] * 7
    assert _direct(result, "cloudflare-d1/DB")[0].constraint == "abc"
    assert _direct(result, "cloudflare-kv/CACHE")[0].constraint == "xyz"
    assert _direct(result, "cloudflare-r2/BUCKET")[0].constraint == "assets"
    assert _direct(result, "cloudflare-durable-object/ROOM")[0].constraint == "Room"
    assert _direct(result, "cloudflare-service/API")[0].constraint == "api-worker"
    assert _direct(result, "cloudflare-queue/JOBS")[0].constraint == "jobs"
    assert _direct(result, "cloudflare-queue/jobs")[0].constraint == "jobs"


def test_wrangler_jsonc_bindings() -> None:
    wrangler = (
        "{\n"
        "  // test configuration\n"
        "  \"durable_objects\": {\n"
        "    \"bindings\": [\n"
        "      {\"name\": \"INSTALLATION\", \"class_name\": \"Installation\"},\n"
        "    ],\n"
        "  },\n"
        "}\n"
    )
    result = _extract(
        CloudflareAdapter(), {"cloudflare/wrangler.test.jsonc": wrangler}
    )

    assert result.issues == ()
    edge = _direct(result, "cloudflare-durable-object/INSTALLATION")[0]
    assert edge.evidence[0].locator == "durable_objects.bindings[0].name"


def test_invalid_wrangler_jsonc_is_a_parse_error() -> None:
    result = _extract(CloudflareAdapter(), {"wrangler.jsonc": "{"})

    assert [issue.kind for issue in result.issues] == [CoverageKind.PARSE_ERROR]


def test_adapter_matchers() -> None:
    assert ActionsAdapter().matches(".github/workflows/ci.yml")
    assert not ActionsAdapter().matches(".github/workflows/nested/ci.yml")
    assert not ActionsAdapter().matches("ci.yml")
    assert ContainersAdapter().matches("Dockerfile")
    assert ContainersAdapter().matches("docker/linux-musl.Dockerfile")
    assert ContainersAdapter().matches("Dockerfile.alpine")
    assert not ContainersAdapter().matches("dockerfile")
    assert ToolchainsAdapter().matches(".python-version")
    assert ToolchainsAdapter().matches("package.json")
    assert SubmodulesAdapter().matches(".gitmodules")
    assert CmakeAdapter().matches("CMakeLists.txt")
    assert CloudflareAdapter().matches("wrangler.test.jsonc")
    assert not CloudflareAdapter().matches("wrangler.yaml")
