from __future__ import annotations

from collections.abc import Iterable
from dataclasses import dataclass

from repomod.dependencies.adapters.vendored import VENDORED_MANIFESTS, vendored_roots
from repomod.dependencies.git_tree import TreeEntry
from repomod.dependencies.model import Ecosystem

_NAME_RULES: dict[str, tuple[Ecosystem, str]] = {
    "Cargo.toml": (Ecosystem.CARGO, "Cargo.toml"),
    "Cargo.lock": (Ecosystem.CARGO, "Cargo.lock"),
    "pyproject.toml": (Ecosystem.PYPI, "pyproject.toml"),
    "uv.lock": (Ecosystem.PYPI, "uv.lock"),
    "pnpm-lock.yaml": (Ecosystem.NPM, "pnpm-lock.yaml"),
    "package-lock.json": (Ecosystem.NPM, "package-lock.json"),
    "package.json": (Ecosystem.NPM, "package.json"),
    "yarn.lock": (Ecosystem.NPM, "yarn.lock"),
    "bun.lock": (Ecosystem.NPM, "bun.lock"),
    "bun.lockb": (Ecosystem.NPM, "bun.lockb"),
    "go.mod": (Ecosystem.GOLANG, "go.mod"),
    "go.sum": (Ecosystem.GOLANG, "go.sum"),
    "go.work": (Ecosystem.GOLANG, "go.work"),
    "Package.swift": (Ecosystem.SWIFT, "Package.swift"),
    "Package.resolved": (Ecosystem.SWIFT, "Package.resolved"),
    "build.zig.zon": (Ecosystem.GENERIC, "build.zig.zon"),
    "rust-toolchain.toml": (Ecosystem.GENERIC, "rust-toolchain.toml"),
    ".node-version": (Ecosystem.GENERIC, ".node-version"),
    ".python-version": (Ecosystem.GENERIC, ".python-version"),
    ".tool-versions": (Ecosystem.GENERIC, ".tool-versions"),
    ".nvmrc": (Ecosystem.GENERIC, ".nvmrc"),
    "CMakeLists.txt": (Ecosystem.GENERIC, "CMakeLists.txt"),
    "wrangler.toml": (Ecosystem.GENERIC, "wrangler.toml"),
    "wrangler.json": (Ecosystem.GENERIC, "wrangler.json"),
    "wrangler.jsonc": (Ecosystem.GENERIC, "wrangler.jsonc"),
    "meson.build": (Ecosystem.GENERIC, "meson.build"),
    "conanfile.py": (Ecosystem.GENERIC, "conanfile.py"),
    "conanfile.txt": (Ecosystem.GENERIC, "conanfile.txt"),
    "vcpkg.json": (Ecosystem.GENERIC, "vcpkg.json"),
    "flake.nix": (Ecosystem.GENERIC, "flake.nix"),
    "Makefile": (Ecosystem.GENERIC, "Makefile"),
    "makefile": (Ecosystem.GENERIC, "makefile"),
    "GNUmakefile": (Ecosystem.GENERIC, "GNUmakefile"),
    "build.gradle": (Ecosystem.GENERIC, "build.gradle"),
    "build.gradle.kts": (Ecosystem.GENERIC, "build.gradle.kts"),
    "settings.gradle": (Ecosystem.GENERIC, "settings.gradle"),
    "settings.gradle.kts": (Ecosystem.GENERIC, "settings.gradle.kts"),
    "pom.xml": (Ecosystem.GENERIC, "pom.xml"),
    "Gemfile": (Ecosystem.GENERIC, "Gemfile"),
}


@dataclass(frozen=True, slots=True)
class DiscoveredInput:
    path: str
    ecosystem: Ecosystem
    label: str


@dataclass(frozen=True, slots=True)
class Discovery:
    inputs: tuple[DiscoveredInput, ...]
    symlinks: tuple[DiscoveredInput, ...]
    vendored_roots: tuple[str, ...]
    vendored_unknown: tuple[str, ...]


def classify(path: str) -> DiscoveredInput | None:
    name = path.rsplit("/", 1)[-1]
    if name == ".gitmodules":
        if path != ".gitmodules":
            return None
        return DiscoveredInput(path, Ecosystem.GENERIC, ".gitmodules")
    rule = _NAME_RULES.get(name)
    if rule is not None:
        return DiscoveredInput(path, *rule)
    if name.startswith("requirements") and name.endswith(".txt"):
        return DiscoveredInput(path, Ecosystem.PYPI, "requirements*.txt")
    if name.endswith(".pc"):
        return DiscoveredInput(path, Ecosystem.GENERIC, "*.pc")
    if name == "Dockerfile" or name.startswith("Dockerfile.") or name.endswith(".Dockerfile"):
        return DiscoveredInput(path, Ecosystem.DOCKER, "Dockerfile")
    workflow = path.removeprefix(".github/workflows/")
    if path.startswith(".github/workflows/") and "/" not in workflow and workflow.endswith((".yml", ".yaml")):
        return DiscoveredInput(path, Ecosystem.GITHUB, ".github/workflows/*.yml")
    if name.startswith("wrangler.") and name.endswith(".jsonc"):
        return DiscoveredInput(path, Ecosystem.GENERIC, "wrangler.*.jsonc")
    return None


def discover(entries: Iterable[TreeEntry]) -> Discovery:
    ordered = tuple(entries)
    inputs: list[DiscoveredInput] = []
    symlinks: list[DiscoveredInput] = []
    for entry in ordered:
        if entry.kind == "gitlink":
            continue
        found = classify(entry.path)
        if found is None:
            continue
        if entry.kind == "symlink":
            symlinks.append(found)
        else:
            inputs.append(found)
    inputs.sort(key=lambda item: item.path)
    symlinks.sort(key=lambda item: item.path)
    roots = vendored_roots(entry.path for entry in ordered if entry.kind != "gitlink")
    manifests = [item.path for item in inputs if item.path.rsplit("/", 1)[-1] in VENDORED_MANIFESTS]
    unknown = tuple(
        root for root in roots if not any(path.startswith(root + "/") for path in manifests)
    )
    return Discovery(tuple(inputs), tuple(symlinks), roots, unknown)
