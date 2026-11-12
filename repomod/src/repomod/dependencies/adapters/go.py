from __future__ import annotations

import posixpath
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

type DraftKey = tuple[str, str]


@dataclass(frozen=True, slots=True)
class GoAdapter:
    name: ClassVar[str] = "go"

    def matches(self, path: str) -> bool:
        return path.rsplit("/", 1)[-1] in {"go.mod", "go.sum", "go.work"}

    def extract(self, inputs: Sequence[InputContent]) -> AdapterResult:
        issues: list[Issue] = []
        projects: list[Project] = []
        drafts: dict[DraftKey, _Ref] = {}
        edges: list[Edge] = []
        for content in inputs:
            name = content.path.rsplit("/", 1)[-1]
            if name in {"go.sum", "go.work"}:
                continue
            mod = _parse_mod(content.path, content.text, content.vendored, issues)
            if mod is None:
                continue
            projects.append(Project(f"{mod.directory}:golang", Ecosystem.GOLANG, mod.directory))
            for ref, edge in _records(mod, issues):
                drafts.setdefault(ref.key, ref)
                edges.append(edge)
        components: dict[str, Component] = {}
        instances: list[Instance] = []
        for ref in drafts.values():
            component = Component(ref.component, ref.ecosystem, ref.name, ref.name)
            components.setdefault(component.purl, component)
            instances.append(
                Instance(
                    id=ref.instance_id,
                    purl=ref.purl,
                    component=ref.component,
                    version=ref.version,
                    source_kind=ref.source_kind,
                    registry_url=None,
                    first_party=ref.first_party,
                )
            )
        unique_projects: dict[str, Project] = {}
        for project in projects:
            unique_projects.setdefault(project.id, project)
        return AdapterResult(
            projects=tuple(unique_projects.values()),
            components=tuple(components.values()),
            instances=tuple(instances),
            edges=tuple(edges),
            issues=tuple(issues),
        )


@dataclass(frozen=True, slots=True)
class _Ref:
    name: str
    version: str | None
    ecosystem: Ecosystem
    source_kind: SourceKind
    source_key: str
    first_party: bool = False

    @property
    def purl(self) -> str:
        return purl(self.ecosystem, self.name, self.version)

    @property
    def component(self) -> str:
        return purl(self.ecosystem, self.name)

    @property
    def key(self) -> DraftKey:
        return (self.purl, self.source_key)

    @property
    def instance_id(self) -> str:
        return f"{self.purl}#"


@dataclass(frozen=True, slots=True)
class _Mod:
    path: str
    directory: str
    name: str | None
    requirements: tuple[_Require, ...]
    replacements: tuple[_Replace, ...]
    tools: tuple[_Tool, ...]
    toolchain: tuple[str, int] | None
    vendored: bool


@dataclass(frozen=True, slots=True)
class _Require:
    module: str
    version: str
    indirect: bool
    line: int


@dataclass(frozen=True, slots=True)
class _Replace:
    old: str
    old_version: str | None
    new: str | None
    new_version: str | None
    path: str | None
    line: int


@dataclass(frozen=True, slots=True)
class _Tool:
    module: str
    line: int


def _parse_mod(
    path: str, text: str, vendored: bool, issues: list[Issue]
) -> _Mod | None:
    directory = posixpath.dirname(path) or "."
    module: str | None = None
    toolchain: tuple[str, int] | None = None
    requirements: list[_Require] = []
    replacements: list[_Replace] = []
    tools: list[_Tool] = []
    block: str | None = None
    for number, raw in enumerate(text.splitlines(), 1):
        code, _, comment = raw.partition("//")
        code = code.strip()
        if not code:
            continue
        if block is not None:
            if code == ")":
                block = None
                continue
            _entry(block, code, comment, number, requirements, replacements, tools, issues, path)
            continue
        if code in {"require (", "replace (", "exclude (", "tool (", "retract ("}:
            block = code.split()[0]
            continue
        keyword, _, rest = code.partition(" ")
        rest = rest.strip()
        if keyword == "module" and rest:
            module = rest.strip('"')
        elif keyword == "toolchain" and rest:
            value = rest.strip('"')
            version = value.removeprefix("go")
            if version not in {"", "default", "local"}:
                toolchain = (version, number)
        elif keyword in {"require", "replace", "exclude", "tool"} and rest:
            _entry(keyword, rest, comment, number, requirements, replacements, tools, issues, path)
        elif keyword in {"go", "retract", "godebug", "ignore"}:
            continue
        else:
            issues.append(
                Issue(CoverageKind.PARSE_ERROR, f"unknown go.mod directive: {keyword}", path=path)
            )
    if module is None:
        issues.append(Issue(CoverageKind.PARSE_ERROR, "go.mod has no module line", path=path))
    return _Mod(
        path=path,
        directory=directory,
        name=module,
        requirements=tuple(requirements),
        replacements=tuple(replacements),
        tools=tuple(tools),
        toolchain=toolchain,
        vendored=vendored,
    )


def _entry(
    kind: str,
    code: str,
    comment: str,
    line: int,
    requirements: list[_Require],
    replacements: list[_Replace],
    tools: list[_Tool],
    issues: list[Issue],
    path: str,
) -> None:
    if kind == "require":
        fields = code.split()
        if len(fields) != 2:
            issues.append(Issue(CoverageKind.PARSE_ERROR, f"malformed require: {code}", path=path))
            return
        requirements.append(
            _Require(fields[0], fields[1], "indirect" in comment.split(), line)
        )
        return
    if kind == "tool":
        fields = code.split()
        if len(fields) != 1:
            issues.append(Issue(CoverageKind.PARSE_ERROR, f"malformed tool: {code}", path=path))
            return
        tools.append(_Tool(fields[0], line))
        return
    if kind == "exclude":
        return
    if kind != "replace":
        return
    left, separator, right = code.partition("=>")
    if not separator:
        issues.append(Issue(CoverageKind.PARSE_ERROR, f"malformed replace: {code}", path=path))
        return
    left_fields = left.split()
    right_fields = right.split()
    if not left_fields or not right_fields or len(left_fields) > 2 or len(right_fields) > 2:
        issues.append(Issue(CoverageKind.PARSE_ERROR, f"malformed replace: {code}", path=path))
        return
    old = left_fields[0]
    old_version = left_fields[1] if len(left_fields) == 2 else None
    target = right_fields[0]
    target_version = right_fields[1] if len(right_fields) == 2 else None
    if target.startswith(("./", "../", "/")) or target in {".", ".."}:
        replacements.append(_Replace(old, old_version, None, None, target, line))
        return
    if target_version is None:
        issues.append(
            Issue(
                CoverageKind.DYNAMIC_VALUE,
                f"replacement for {old} has no version or local path",
                path=path,
            )
        )
    replacements.append(_Replace(old, old_version, target, target_version, None, line))


def _records(mod: _Mod, issues: list[Issue]) -> list[tuple[_Ref, Edge]]:
    project_id = f"{mod.directory}:golang"
    records: list[tuple[_Ref, Edge]] = []
    found: dict[str, _Ref] = {}
    for requirement in mod.requirements:
        ref = _resolve(mod, requirement, issues)
        found[requirement.module] = ref
        resolution = Resolution.RESOLVED
        if not requirement.version.startswith("v"):
            issues.append(
                Issue(
                    CoverageKind.DYNAMIC_VALUE,
                    f"{requirement.module} has a non-version requirement: {requirement.version}",
                    path=mod.path,
                    locator=f"L{requirement.line}",
                )
            )
            resolution = Resolution.UNKNOWN
        records.append(
            (
                ref,
                _edge(
                    project_id,
                    ref,
                    Relation.TRANSITIVE if requirement.indirect else Relation.DIRECT,
                    Scope.RUNTIME,
                    requirement.version,
                    resolution,
                    mod.path,
                    requirement.line,
                ),
            )
        )
    for tool in mod.tools:
        module = _tool_module(tool.module, mod.requirements)
        candidate = next(
            (item for item in mod.requirements if item.module == module), None
        )
        ref = found.get(module or "")
        if candidate is not None:
            ref = _resolve(mod, candidate, issues)
        if ref is None:
            issues.append(
                Issue(
                    CoverageKind.DYNAMIC_VALUE,
                    f"tool {tool.module} is not required by this module",
                    path=mod.path,
                    locator=f"L{tool.line}",
                )
            )
            ref = _Ref(tool.module, None, Ecosystem.GOLANG, SourceKind.REGISTRY, f"golang:{tool.module}")
        records.append(
            (
                ref,
                _edge(
                    project_id,
                    ref,
                    Relation.DIRECT,
                    Scope.TOOLCHAIN,
                    None,
                    Resolution.RESOLVED if ref.version is not None else Resolution.DECLARED_ONLY,
                    mod.path,
                    tool.line,
                ),
            )
        )
    if mod.toolchain is not None:
        version, line = mod.toolchain
        ref = _Ref(
            "go",
            version,
            Ecosystem.GENERIC,
            SourceKind.URL,
            f"toolchain:go:{version}",
        )
        records.append(
            (
                ref,
                _edge(
                    project_id,
                    ref,
                    Relation.DIRECT,
                    Scope.TOOLCHAIN,
                    version,
                    Resolution.RESOLVED,
                    mod.path,
                    line,
                ),
            )
        )
    return records


def _tool_module(tool: str, requirements: Sequence[_Require]) -> str | None:
    matches = [
        requirement.module
        for requirement in requirements
        if tool == requirement.module or tool.startswith(requirement.module + "/")
    ]
    return max(matches, key=len) if matches else None


def _resolve(mod: _Mod, requirement: _Require, issues: list[Issue]) -> _Ref:
    replacement = _replacement(mod, requirement.module, requirement.version)
    if replacement is not None:
        if replacement.path is not None:
            target = (
                posixpath.normpath(posixpath.join(mod.directory, replacement.path))
                if replacement.path.startswith(".")
                else replacement.path
            )
            return _Ref(
                requirement.module,
                None,
                Ecosystem.GENERIC,
                SourceKind.PATH,
                f"path:{target}",
                first_party=_inside(target),
            )
        if replacement.new is not None:
            return _Ref(
                replacement.new,
                replacement.new_version,
                Ecosystem.GOLANG,
                SourceKind.REGISTRY,
                f"golang:{replacement.new}",
            )
    return _Ref(
        requirement.module,
        requirement.version,
        Ecosystem.GOLANG,
        SourceKind.REGISTRY,
        f"golang:{requirement.module}",
    )


def _replacement(mod: _Mod, module: str, version: str) -> _Replace | None:
    exact = next(
        (
            item
            for item in mod.replacements
            if item.old == module and item.old_version == version
        ),
        None,
    )
    if exact is not None:
        return exact
    return next(
        (item for item in mod.replacements if item.old == module and item.old_version is None),
        None,
    )


def _inside(directory: str) -> bool:
    return directory == "." or not directory.startswith(("../", "/"))


def _edge(
    source: str,
    ref: _Ref,
    relation: Relation,
    scope: Scope,
    constraint: str | None,
    resolution: Resolution,
    path: str,
    line: int,
) -> Edge:
    return Edge(
        from_id=source,
        to_id=ref.instance_id,
        relation=relation,
        scope=scope,
        optional=False,
        constraint=constraint,
        resolved_version=ref.version,
        condition=None,
        source_kind=ref.source_kind,
        resolution=resolution,
        evidence=(Evidence(path, f"L{line}"),),
    )
