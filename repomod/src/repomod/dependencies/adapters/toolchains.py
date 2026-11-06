from __future__ import annotations

import json
import posixpath
import tomllib
from collections.abc import Sequence
from dataclasses import dataclass
from typing import ClassVar

from repomod.dependencies.model import (
    AdapterResult,
    Component,
    CoverageKind,
    Ecosystem,
    Edge,
    Evidence,
    InputContent,
    Instance,
    Issue,
    Project,
    Relation,
    Resolution,
    Scope,
    SourceKind,
    purl,
)

_TOOLCHAIN_FILES = frozenset(
    {".node-version", ".nvmrc", ".python-version", ".tool-versions", "rust-toolchain.toml", "package.json"}
)


@dataclass(frozen=True, slots=True)
class ToolchainsAdapter:
    name: ClassVar[str] = "toolchains"

    def matches(self, path: str) -> bool:
        return path.rsplit("/", 1)[-1] in _TOOLCHAIN_FILES

    def extract(self, inputs: Sequence[InputContent]) -> AdapterResult:
        issues: list[Issue] = []
        projects: dict[str, Project] = {}
        drafts: dict[str, _Ref] = {}
        edges: list[Edge] = []
        for content in inputs:
            directory = posixpath.dirname(content.path) or "."
            entries = _entries(content.path, content.text, issues)
            if not entries:
                continue
            project = Project(f"{directory}:generic", Ecosystem.GENERIC, directory)
            projects.setdefault(project.id, project)
            for entry in entries:
                ref = _Ref(
                    entry.name,
                    entry.version,
                    SourceKind.VENDORED if content.vendored else SourceKind.URL,
                    f"toolchain:{entry.name}:{entry.version}",
                )
                drafts.setdefault(ref.key, ref)
                edges.append(
                    Edge(
                        from_id=project.id,
                        to_id=ref.instance_id,
                        relation=Relation.DIRECT,
                        scope=Scope.TOOLCHAIN,
                        optional=False,
                        constraint=entry.constraint,
                        resolved_version=entry.version,
                        condition=None,
                        source_kind=ref.source_kind,
                        resolution=entry.resolution,
                        evidence=(Evidence(content.path, entry.locator),),
                    )
                )
        components: dict[str, Component] = {}
        instances: list[Instance] = []
        for ref in drafts.values():
            component = Component(ref.component, Ecosystem.GENERIC, ref.name, ref.name)
            components.setdefault(component.purl, component)
            instances.append(
                Instance(
                    id=ref.instance_id,
                    purl=ref.purl,
                    component=ref.component,
                    version=ref.version,
                    source_kind=ref.source_kind,
                    first_party=ref.first_party,
                )
            )
        return AdapterResult(
            projects=tuple(projects.values()),
            components=tuple(components.values()),
            instances=tuple(instances),
            edges=tuple(edges),
            issues=tuple(issues),
        )


@dataclass(frozen=True, slots=True)
class _Ref:
    name: str
    version: str
    source_kind: SourceKind
    source_key: str
    first_party: bool = False

    @property
    def purl(self) -> str:
        return purl(Ecosystem.GENERIC, self.name, self.version)

    @property
    def component(self) -> str:
        return purl(Ecosystem.GENERIC, self.name)

    @property
    def key(self) -> str:
        return self.source_key

    @property
    def instance_id(self) -> str:
        return f"{self.purl}#"


@dataclass(frozen=True, slots=True)
class _Entry:
    name: str
    version: str
    constraint: str
    resolution: Resolution
    locator: str


def _entries(path: str, text: str, issues: list[Issue]) -> tuple[_Entry, ...]:
    name = path.rsplit("/", 1)[-1]
    if name == "rust-toolchain.toml":
        try:
            document = tomllib.loads(text)
        except tomllib.TOMLDecodeError as error:
            issues.append(Issue(CoverageKind.PARSE_ERROR, f"invalid TOML: {error}", path=path))
            return ()
        toolchain = document.get("toolchain")
        channel = toolchain.get("channel") if isinstance(toolchain, dict) else None
        if not isinstance(channel, str) or not channel:
            return ()
        return (_entry("rust", channel, 1),)
    if name in {".node-version", ".nvmrc"}:
        version = _first_line(text)
        return (_entry("node", version, 1),) if version else ()
    if name == ".python-version":
        return tuple(
            _entry("python", line.strip(), number)
            for number, line in enumerate(text.splitlines(), start=1)
            if line.strip() and not line.strip().startswith("#")
        )
    if name == ".tool-versions":
        entries: list[_Entry] = []
        for number, raw in enumerate(text.splitlines(), start=1):
            line = raw.split("#", 1)[0].strip()
            if not line:
                continue
            fields = line.split()
            if len(fields) < 2 or fields[1] == "system":
                continue
            entries.append(_entry(fields[0], fields[1], number))
        return tuple(entries)
    if name == "package.json":
        return _package_manager(path, text, issues)
    return ()


def _package_manager(path: str, text: str, issues: list[Issue]) -> tuple[_Entry, ...]:
    try:
        document = json.loads(text)
    except json.JSONDecodeError as error:
        issues.append(Issue(CoverageKind.PARSE_ERROR, f"invalid JSON: {error}", path=path))
        return ()
    package_manager = document.get("packageManager") if isinstance(document, dict) else None
    if not isinstance(package_manager, str) or not package_manager:
        return ()
    manager, separator, version = package_manager.partition("@")
    if not separator or not manager or not version:
        issues.append(
            Issue(
                CoverageKind.DYNAMIC_VALUE,
                f"packageManager {package_manager!r} has no version",
                path=path,
                locator="packageManager",
            )
        )
        return ()
    resolved = version.split("+", 1)[0]
    if not resolved:
        return ()
    return (
        _Entry(
            manager,
            resolved,
            package_manager,
            Resolution.RESOLVED if "+" in version else _resolution(resolved),
            "packageManager",
        ),
    )


def _first_line(text: str) -> str:
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if line:
            return line
    return ""


def _entry(name: str, version: str, line: int) -> _Entry:
    return _Entry(name, version, version, _resolution(version), f"L{line}")


def _resolution(version: str) -> Resolution:
    if any(character in version for character in "*") or version in {"latest", "stable", "lts", "nightly"}:
        return Resolution.DECLARED_ONLY
    return Resolution.RESOLVED
