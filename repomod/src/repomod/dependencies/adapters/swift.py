from __future__ import annotations

import json
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
    redact_url,
)

type DraftKey = tuple[str, str]

_SELECTORS = ("from", "exact", "branch", "revision")


@dataclass(frozen=True, slots=True)
class SwiftAdapter:
    name: ClassVar[str] = "swift"

    def matches(self, path: str) -> bool:
        return path.rsplit("/", 1)[-1] in {"Package.swift", "Package.resolved"}

    def extract(self, inputs: Sequence[InputContent]) -> AdapterResult:
        issues: list[Issue] = []
        manifests: list[_Manifest] = []
        resolved: list[_Resolved] = []
        for content in inputs:
            name = content.path.rsplit("/", 1)[-1]
            if name == "Package.resolved":
                parsed = _parse_resolved(content.path, content.text, content.vendored, issues)
                if parsed is not None:
                    resolved.append(parsed)
            else:
                manifests.append(
                    _parse_manifest(content.path, content.text, content.vendored, issues)
                )
        projects: dict[str, Project] = {}
        drafts: dict[DraftKey, _Ref] = {}
        edges: list[Edge] = []
        for manifest in manifests:
            project = Project(f"{manifest.directory}:swift", Ecosystem.SWIFT, manifest.directory)
            projects.setdefault(project.id, project)
            pins = next(
                (item for item in resolved if item.directory == manifest.directory), None
            )
            used: set[str] = set()
            for declaration in manifest.declarations:
                ref = _resolve_declaration(declaration, pins)
                if ref.vcs_url is not None:
                    used.add(_normalize(ref.vcs_url))
                elif ref.source_key.startswith("path:"):
                    used.add(_normalize(ref.source_key.removeprefix("path:")))
                drafts.setdefault(ref.key, ref)
                edges.append(
                    _edge(
                        project.id,
                        ref,
                        Relation.DIRECT,
                        declaration.constraint,
                        ref.resolution,
                        manifest.path,
                        declaration.locator,
                    )
                )
            if pins is not None:
                for pin in pins.pins:
                    key = pin.url if pin.url is not None else pin.path or ""
                    if _normalize(key) in used:
                        continue
                    ref = _pin_ref(pin, pins.vendored)
                    drafts.setdefault(ref.key, ref)
                    edges.append(
                        _edge(
                            project.id,
                            ref,
                            Relation.TRANSITIVE,
                            None,
                            Resolution.RESOLVED,
                            pins.path,
                            pin.locator,
                        )
                    )
        for pins in resolved:
            if any(item.directory == pins.directory for item in manifests):
                continue
            project = Project(f"{pins.directory}:swift", Ecosystem.SWIFT, pins.directory)
            projects.setdefault(project.id, project)
            for pin in pins.pins:
                ref = _pin_ref(pin, pins.vendored)
                drafts.setdefault(ref.key, ref)
                edges.append(
                    _edge(
                        project.id,
                        ref,
                        Relation.TRANSITIVE,
                        None,
                        Resolution.RESOLVED,
                        pins.path,
                        pin.locator,
                    )
                )
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
                    vcs_url=ref.vcs_url,
                    commit=ref.commit,
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
    version: str | None
    ecosystem: Ecosystem
    source_kind: SourceKind
    source_key: str
    resolution: Resolution
    vcs_url: str | None = None
    commit: str | None = None
    first_party: bool = False
    qualifiers: tuple[tuple[str, str], ...] = ()

    @property
    def purl(self) -> str:
        return purl(self.ecosystem, self.name, self.version, qualifiers=dict(self.qualifiers))

    @property
    def component(self) -> str:
        return purl(self.ecosystem, self.name, qualifiers=dict(self.qualifiers))

    @property
    def key(self) -> DraftKey:
        return (self.purl, self.source_key)

    @property
    def instance_id(self) -> str:
        return f"{self.purl}#"


@dataclass(frozen=True, slots=True)
class _Pin:
    name: str
    url: str | None
    path: str | None
    version: str | None
    commit: str | None
    locator: str


@dataclass(frozen=True, slots=True)
class _Resolved:
    path: str
    directory: str
    pins: tuple[_Pin, ...]
    vendored: bool


@dataclass(frozen=True, slots=True)
class _Declaration:
    name: str
    url: str | None
    path: str | None
    constraint: str | None
    locator: str
    vendored: bool


@dataclass(frozen=True, slots=True)
class _Manifest:
    path: str
    directory: str
    declarations: tuple[_Declaration, ...]
    vendored: bool


def _parse_resolved(
    path: str, text: str, vendored: bool, issues: list[Issue]
) -> _Resolved | None:
    try:
        document = json.loads(text)
    except json.JSONDecodeError as error:
        issues.append(Issue(CoverageKind.PARSE_ERROR, f"invalid JSON: {error}", path=path))
        return None
    if not isinstance(document, dict):
        issues.append(Issue(CoverageKind.PARSE_ERROR, "Package.resolved is not an object", path=path))
        return None
    version = document.get("version")
    if version == 1:
        container = document.get("object")
        raw_pins = container.get("pins") if isinstance(container, dict) else None
    elif version in {2, 3}:
        raw_pins = document.get("pins")
    else:
        issues.append(
            Issue(
                CoverageKind.UNSUPPORTED_FORMAT,
                f"unsupported Package.resolved version {version!r}",
                path=path,
            )
        )
        return None
    if not isinstance(raw_pins, list):
        issues.append(Issue(CoverageKind.PARSE_ERROR, "Package.resolved has no pins", path=path))
        return None
    pins: list[_Pin] = []
    for index, raw in enumerate(raw_pins):
        locator = (
            f"object.pins[{index}]" if version == 1 else f"pins[{index}]"
        )
        if not isinstance(raw, dict):
            issues.append(Issue(CoverageKind.PARSE_ERROR, "pin is not an object", path=path))
            continue
        state = raw.get("state")
        state = state if isinstance(state, dict) else {}
        name = raw.get("identity") if version in {2, 3} else raw.get("package")
        location = raw.get("location") if version in {2, 3} else raw.get("repositoryURL")
        name = name if isinstance(name, str) and name else None
        location = location if isinstance(location, str) and location else None
        if name is None and location is not None:
            name = _repo_name(location)
        if name is None or location is None:
            issues.append(Issue(CoverageKind.PARSE_ERROR, "pin has no name or location", path=path))
            continue
        version_value = state.get("version")
        version_value = str(version_value) if version_value is not None else None
        revision = state.get("revision")
        revision = revision if isinstance(revision, str) and revision else None
        if location.startswith(("http://", "https://", "ssh://", "git@")):
            pins.append(_Pin(name, location, None, version_value, revision, locator))
        else:
            pins.append(
                _Pin(
                    name,
                    None,
                    _normalize_path(posixpath.dirname(path) or ".", location),
                    version_value,
                    revision,
                    locator,
                )
            )
    return _Resolved(path, posixpath.dirname(path) or ".", tuple(pins), vendored)


def _parse_manifest(
    path: str, text: str, vendored: bool, issues: list[Issue]
) -> _Manifest:
    declarations: list[_Declaration] = []
    for line, arguments in _package_calls(text):
        declaration = _declaration(path, line, arguments, vendored, issues)
        if declaration is not None:
            declarations.append(declaration)
    return _Manifest(path, posixpath.dirname(path) or ".", tuple(declarations), vendored)


def _declaration(
    path: str,
    line: int,
    arguments: str,
    vendored: bool,
    issues: list[Issue],
) -> _Declaration | None:
    values: dict[str, str] = {}
    dynamic = False
    for argument in _split_arguments(arguments):
        stripped = argument.strip()
        if not stripped:
            continue
        if stripped.startswith((".upToNextMajor(", ".upToNextMinor(")):
            name, _, inner = stripped.partition("(")
            inner = inner.rsplit(")", 1)[0]
            literal = _literal(inner.strip().removeprefix("from:").strip())
            if literal is None:
                dynamic = True
            else:
                values.setdefault("from", literal)
            continue
        label, separator, value = stripped.partition(":")
        if separator == "":
            dynamic = True
            continue
        label = label.strip()
        literal = _literal(value.strip())
        if literal is None:
            if label in {"url", "path", "name", *_SELECTORS}:
                dynamic = True
            continue
        values.setdefault(label, literal)
    if dynamic:
        issues.append(
            Issue(
                CoverageKind.DYNAMIC_VALUE,
                "Swift package declaration is not made of string literals",
                path=path,
                locator=f"L{line}",
            )
        )
        return None
    url = values.get("url")
    package_path = values.get("path")
    if url is None and package_path is None:
        issues.append(
            Issue(
                CoverageKind.DYNAMIC_VALUE,
                "Swift package declaration has no literal url or path",
                path=path,
                locator=f"L{line}",
            )
        )
        return None
    constraint = next((values.get(key) for key in _SELECTORS if values.get(key) is not None), None)
    name = values.get("name") or (_repo_name(url) if url is not None else None) or (
        posixpath.basename(package_path.rstrip("/")) if package_path is not None else ""
    )
    if not name:
        issues.append(
            Issue(
                CoverageKind.DYNAMIC_VALUE,
                "Swift package declaration has no package name",
                path=path,
                locator=f"L{line}",
            )
        )
        return None
    return _Declaration(
        name=name,
        url=url,
        path=(
            _normalize_path(posixpath.dirname(path) or ".", package_path)
            if package_path is not None
            else None
        ),
        constraint=constraint,
        locator=f"L{line}",
        vendored=vendored,
    )


def _resolve_declaration(declaration: _Declaration, pins: _Resolved | None) -> _Ref:
    if declaration.path is not None:
        target = None
        if pins is not None:
            target = next(
                (
                    pin
                    for pin in pins.pins
                    if pin.path is not None
                    and _normalize(pin.path) == _normalize(declaration.path)
                ),
                None,
            )
        if target is not None:
            return _pin_ref(target, declaration.vendored)
        return _Ref(
            declaration.name,
            None,
            Ecosystem.GENERIC,
            SourceKind.VENDORED if declaration.vendored else SourceKind.PATH,
            f"path:{declaration.path}",
            Resolution.RESOLVED,
            first_party=_inside(declaration.path),
        )
    url = declaration.url
    target = None
    if pins is not None and url is not None:
        target = next(
            (pin for pin in pins.pins if pin.url is not None and _normalize(pin.url) == _normalize(url)),
            None,
        )
        if target is None:
            target = next(
                (pin for pin in pins.pins if pin.name.casefold() == declaration.name.casefold()),
                None,
            )
    if target is not None:
        ref = _pin_ref(target, declaration.vendored)
        return _Ref(
            ref.name,
            ref.version,
            ref.ecosystem,
            SourceKind.VENDORED if declaration.vendored else ref.source_kind,
            ref.source_key,
            Resolution.RESOLVED,
            vcs_url=ref.vcs_url,
            commit=ref.commit,
            first_party=ref.first_party,
            qualifiers=ref.qualifiers,
        )
    if url is None:
        return _Ref(
            declaration.name,
            None,
            Ecosystem.SWIFT,
            SourceKind.VENDORED if declaration.vendored else SourceKind.GIT,
            f"swift:{declaration.name}",
            Resolution.DECLARED_ONLY,
        )
    vcs_url = redact_url(url, drop_query_and_fragment=True)
    commit = declaration.constraint if _is_sha(declaration.constraint or "") else None
    resolution = Resolution.RESOLVED if commit is not None else Resolution.DECLARED_ONLY
    return _Ref(
        declaration.name,
        None,
        Ecosystem.SWIFT,
        SourceKind.VENDORED if declaration.vendored else SourceKind.GIT,
        f"swift:{vcs_url}",
        resolution,
        vcs_url=vcs_url,
        commit=commit,
        qualifiers=(("vcs_url", vcs_url),),
    )


def _pin_ref(pin: _Pin, vendored: bool) -> _Ref:
    if pin.path is not None:
        return _Ref(
            pin.name,
            pin.version,
            Ecosystem.GENERIC,
            SourceKind.VENDORED if vendored else SourceKind.PATH,
            f"path:{pin.path}",
            Resolution.RESOLVED,
            first_party=_inside(pin.path),
        )
    vcs_url = redact_url(pin.url or "", drop_query_and_fragment=True)
    return _Ref(
        pin.name,
        pin.version,
        Ecosystem.SWIFT,
        SourceKind.VENDORED if vendored else SourceKind.GIT,
        f"swift:{vcs_url}",
        Resolution.RESOLVED,
        vcs_url=vcs_url,
        commit=pin.commit,
        qualifiers=(("vcs_url", vcs_url),),
    )


def _edge(
    source: str,
    ref: _Ref,
    relation: Relation,
    constraint: str | None,
    resolution: Resolution,
    path: str,
    locator: str,
) -> Edge:
    return Edge(
        from_id=source,
        to_id=ref.instance_id,
        relation=relation,
        scope=Scope.RUNTIME,
        optional=False,
        constraint=constraint,
        resolved_version=ref.version,
        condition=None,
        source_kind=ref.source_kind,
        resolution=resolution,
        evidence=(Evidence(path, locator),),
    )


def _package_calls(text: str) -> list[tuple[int, str]]:
    calls: list[tuple[int, str]] = []
    index = 0
    while index < len(text):
        if text.startswith("//", index):
            index = text.find("\n", index)
            if index < 0:
                break
            continue
        if text.startswith("/*", index):
            end = text.find("*/", index + 2)
            index = len(text) if end < 0 else end + 2
            continue
        if text.startswith('"""', index):
            end = text.find('"""', index + 3)
            index = len(text) if end < 0 else end + 3
            continue
        character = text[index]
        if character == '"':
            index = _skip_string(text, index)
            continue
        if text.startswith(".package(", index):
            line = text.count("\n", 0, index) + 1
            start = index + len(".package(")
            end = _matching(text, start)
            if end is None:
                break
            calls.append((line, text[start:end]))
            index = end + 1
            continue
        index += 1
    return calls


def _matching(text: str, start: int) -> int | None:
    depth = 1
    index = start
    while index < len(text):
        if text.startswith("//", index):
            newline = text.find("\n", index)
            if newline < 0:
                return None
            index = newline
            continue
        if text.startswith("/*", index):
            end = text.find("*/", index + 2)
            if end < 0:
                return None
            index = end + 2
            continue
        if text.startswith('"""', index):
            end = text.find('"""', index + 3)
            if end < 0:
                return None
            index = end + 3
            continue
        character = text[index]
        if character == '"':
            index = _skip_string(text, index)
            continue
        if character == "(":
            depth += 1
        elif character == ")":
            depth -= 1
            if depth == 0:
                return index
        index += 1
    return None


def _skip_string(text: str, start: int) -> int:
    index = start + 1
    while index < len(text):
        if text[index] == "\\":
            index += 2
            continue
        if text[index] == '"':
            return index + 1
        index += 1
    return len(text)


def _split_arguments(text: str) -> list[str]:
    arguments: list[str] = []
    depth = 0
    start = 0
    index = 0
    while index < len(text):
        if text.startswith("//", index):
            newline = text.find("\n", index)
            index = len(text) if newline < 0 else newline
            continue
        if text.startswith("/*", index):
            end = text.find("*/", index + 2)
            index = len(text) if end < 0 else end + 2
            continue
        character = text[index]
        if character == '"':
            index = _skip_string(text, index)
            continue
        if character in "([{":
            depth += 1
        elif character in ")]}":
            depth -= 1
        elif character == "," and depth == 0:
            arguments.append(text[start:index])
            start = index + 1
        index += 1
    arguments.append(text[start:])
    return arguments


def _literal(value: str) -> str | None:
    if len(value) >= 2 and value.startswith('"') and value.endswith('"'):
        inner = value[1:-1]
        return (
            inner.replace('\\"', '"')
            .replace("\\n", "\n")
            .replace("\\t", "\t")
            .replace("\\\\", "\\")
        )
    return None


def _repo_name(url: str) -> str:
    tail = url.rstrip("/").rsplit("/", 1)[-1]
    return tail.removesuffix(".git")


def _normalize(value: str) -> str:
    return value.rstrip("/").removesuffix(".git").casefold()


def _normalize_path(base: str, value: str) -> str:
    return posixpath.normpath(posixpath.join(base, value))


def _inside(directory: str) -> bool:
    return directory == "." or not directory.startswith(("../", "/"))


def _is_sha(value: str) -> bool:
    return len(value) == 40 and all(character in "0123456789abcdef" for character in value)
