from __future__ import annotations

import posixpath
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, replace
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
    redact_urls,
)


@dataclass(frozen=True, slots=True)
class ZigAdapter:
    name: ClassVar[str] = "zig"

    def matches(self, path: str) -> bool:
        return path.rsplit("/", 1)[-1] == "build.zig.zon"

    def extract(self, inputs: Sequence[InputContent]) -> AdapterResult:
        issues: list[Issue] = []
        projects: dict[str, Project] = {}
        drafts: dict[str, _Ref] = {}
        edges: list[Edge] = []
        for content in inputs:
            path = content.path
            try:
                root = _Parser(content.text).parse()
            except ValueError as error:
                issues.append(Issue(CoverageKind.PARSE_ERROR, f"invalid ZON: {error}", path=path))
                continue
            if not isinstance(root.value, dict):
                issues.append(Issue(CoverageKind.PARSE_ERROR, "ZON root is not a struct", path=path))
                continue
            directory = posixpath.dirname(path) or "."
            project = Project(f"{directory}:generic", Ecosystem.GENERIC, directory)
            projects.setdefault(project.id, project)
            dependencies = root.value.get("dependencies")
            if dependencies is None:
                continue
            if not isinstance(dependencies.value, dict):
                issues.append(
                    Issue(CoverageKind.DYNAMIC_VALUE, "dependencies is not a struct", path=path)
                )
                continue
            for name, spec in dependencies.value.items():
                resolved = _dependency(
                    name, spec, directory, path, content.vendored, issues
                )
                if resolved is None:
                    continue
                ref, edge = resolved
                drafts.setdefault(ref.key, ref)
                edges.append(edge)
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
                    version=None,
                    source_kind=ref.source_kind,
                    first_party=ref.first_party,
                )
            )
        return AdapterResult(
            projects=tuple(projects.values()),
            components=tuple(components.values()),
            instances=tuple(instances),
            edges=tuple(edges),
            issues=tuple(replace(item, message=redact_urls(item.message)) for item in issues),
        )


@dataclass(frozen=True, slots=True)
class _Ref:
    name: str
    source_kind: SourceKind
    source_key: str
    first_party: bool = False
    qualifiers: tuple[tuple[str, str], ...] = ()

    @property
    def purl(self) -> str:
        return purl(Ecosystem.GENERIC, self.name, qualifiers=dict(self.qualifiers))

    @property
    def component(self) -> str:
        return self.purl

    @property
    def key(self) -> str:
        return self.source_key

    @property
    def instance_id(self) -> str:
        return f"{self.purl}#"


@dataclass(frozen=True, slots=True)
class _Value:
    value: str | bool | Mapping[str, _Value] | tuple[_Value, ...] | None
    line: int


def _dependency(
    name: str,
    spec: _Value,
    directory: str,
    path: str,
    vendored: bool,
    issues: list[Issue],
) -> tuple[_Ref, Edge] | None:
    if not isinstance(spec.value, dict):
        issues.append(
            Issue(
                CoverageKind.DYNAMIC_VALUE,
                f"dependency {name} is not a struct",
                path=path,
                locator=f"L{spec.line}",
            )
        )
        return None
    url = _text_field(spec, "url")
    local = _text_field(spec, "path")
    checksum = _text_field(spec, "hash")
    lazy = _bool_field(spec, "lazy")
    if url is None and local is None:
        issues.append(
            Issue(
                CoverageKind.DYNAMIC_VALUE,
                f"dependency {name} has no literal url or path",
                path=path,
                locator=f"L{spec.line}",
            )
        )
        return None
    condition = "lazy" if lazy else None
    if url is not None:
        target = redact_url(url, drop_query_and_fragment=True)
        ref = _Ref(
            name,
            SourceKind.VENDORED if vendored else SourceKind.URL,
            f"url:{target}",
            qualifiers=(("download_url", target),),
        )
        constraint = checksum or target
        if checksum is None:
            resolution = Resolution.DECLARED_ONLY
        else:
            resolution = Resolution.CONDITIONAL if lazy else Resolution.RESOLVED
    else:
        target = (
            posixpath.normpath(posixpath.join(directory, local))
            if local is not None
            else directory
        )
        ref = _Ref(
            name,
            SourceKind.VENDORED if vendored else SourceKind.PATH,
            f"path:{target}",
            first_party=_inside(target) and not vendored,
        )
        constraint = local
        resolution = Resolution.CONDITIONAL if lazy else Resolution.RESOLVED
    edge = Edge(
        from_id=f"{directory}:generic",
        to_id=ref.instance_id,
        relation=Relation.DIRECT,
        scope=Scope.RUNTIME,
        optional=lazy,
        constraint=constraint,
        resolved_version=None,
        condition=condition,
        source_kind=ref.source_kind,
        resolution=resolution,
        evidence=(Evidence(path, f"L{spec.line}"),),
    )
    return ref, edge


def _text_field(value: _Value, name: str) -> str | None:
    entry = value.value.get(name) if isinstance(value.value, dict) else None
    if entry is None or not isinstance(entry.value, str):
        return None
    return entry.value


def _bool_field(value: _Value, name: str) -> bool:
    entry = value.value.get(name) if isinstance(value.value, dict) else None
    return entry is not None and entry.value is True


def _inside(directory: str) -> bool:
    return directory == "." or not directory.startswith(("../", "/"))


class _Parser:
    def __init__(self, text: str) -> None:
        self.text = text
        self.index = 0
        self.line = 1

    def parse(self) -> _Value:
        self._skip()
        value = self._expr()
        self._skip()
        if self.index != len(self.text):
            self._fail("trailing input")
        return value

    def _fail(self, message: str) -> None:
        raise ValueError(f"{message} at line {self.line}")

    def _advance(self) -> str:
        character = self.text[self.index]
        self.index += 1
        if character == "\n":
            self.line += 1
        return character

    def _skip(self) -> None:
        while self.index < len(self.text):
            if self.text[self.index].isspace():
                self._advance()
                continue
            if self.text.startswith("//", self.index):
                while self.index < len(self.text) and self.text[self.index] != "\n":
                    self.index += 1
                continue
            if self.text.startswith("/*", self.index):
                end = self.text.find("*/", self.index + 2)
                if end < 0:
                    self._fail("unterminated block comment")
                while self.index < end + 2:
                    self._advance()
                continue
            return

    def _expr(self) -> _Value:
        self._skip()
        line = self.line
        if self.index >= len(self.text):
            self._fail("unexpected end of input")
        if self.text.startswith(".{", self.index):
            return self._struct_or_list(line)
        character = self.text[self.index]
        if character == ".":
            self._advance()
            return _Value(self._identifier(), line)
        if character == '"':
            return _Value(self._string(), line)
        if character.isdigit() or character == "-":
            return _Value(self._number(), line)
        if character == "[":
            return _Value(self._opaque(), line)
        name = self._identifier()
        if name == "true":
            return _Value(True, line)
        if name == "false":
            return _Value(False, line)
        if name == "null":
            return _Value(None, line)
        self._fail(f"unsupported expression: {name}")
        raise AssertionError("unreachable")

    def _struct_or_list(self, line: int) -> _Value:
        self.index += 2
        self._skip()
        if self.index < len(self.text) and self.text[self.index] == "}":
            self._advance()
            return _Value({}, line)
        if self._struct_ahead():
            entries: dict[str, _Value] = {}
            while True:
                self._skip()
                if self.index >= len(self.text):
                    self._fail("unterminated struct")
                if self.text[self.index] == "}":
                    self._advance()
                    return _Value(entries, line)
                if self.text[self.index] != ".":
                    self._fail("expected a field name")
                self._advance()
                key = self._identifier()
                self._skip()
                if self.index >= len(self.text) or self.text[self.index] != "=":
                    self._fail("expected '='")
                self._advance()
                entries[key] = self._expr()
                self._skip()
                if self.index < len(self.text) and self.text[self.index] == ",":
                    self._advance()
        items: list[_Value] = []
        while True:
            self._skip()
            if self.index >= len(self.text):
                self._fail("unterminated list")
            if self.text[self.index] == "}":
                self._advance()
                return _Value(tuple(items), line)
            items.append(self._expr())
            self._skip()
            if self.index < len(self.text) and self.text[self.index] == ",":
                self._advance()

    def _struct_ahead(self) -> bool:
        saved = self.index
        saved_line = self.line
        try:
            self._skip()
            if self.index >= len(self.text) or self.text[self.index] != ".":
                return False
            self._advance()
            if self.index < len(self.text) and self.text[self.index] == "@":
                self._advance()
                self._string()
            else:
                self._identifier()
            self._skip()
            return self.index < len(self.text) and self.text[self.index] == "="
        except ValueError:
            return False
        finally:
            self.index = saved
            self.line = saved_line

    def _identifier(self) -> str:
        if self.index < len(self.text) and self.text[self.index] == "@":
            self._advance()
            return self._string()
        start = self.index
        while self.index < len(self.text):
            character = self.text[self.index]
            if character.isalnum() or character == "_":
                self.index += 1
                continue
            break
        if start == self.index:
            self._fail("expected an identifier")
        return self.text[start:self.index]

    def _string(self) -> str:
        if self.index >= len(self.text) or self.text[self.index] != '"':
            self._fail("expected a string")
        self._advance()
        characters: list[str] = []
        while self.index < len(self.text):
            character = self._advance()
            if character == '"':
                return "".join(characters)
            if character != "\\":
                characters.append(character)
                continue
            if self.index >= len(self.text):
                break
            escape = self._advance()
            if escape == "n":
                characters.append("\n")
            elif escape == "t":
                characters.append("\t")
            elif escape == "r":
                characters.append("\r")
            elif escape == "\\":
                characters.append("\\")
            elif escape == '"':
                characters.append('"')
            elif escape == "x":
                digits = self.text[self.index : self.index + 2]
                if len(digits) != 2:
                    self._fail("invalid hex escape")
                characters.append(chr(int(digits, 16)))
                self.index += 2
            elif escape == "u":
                end = self.text.find("}", self.index)
                if end < 0:
                    self._fail("invalid unicode escape")
                characters.append(chr(int(self.text[self.index + 1 : end], 16)))
                self.index = end + 1
            else:
                characters.append(escape)
        self._fail("unterminated string")
        raise AssertionError("unreachable")

    def _number(self) -> str:
        start = self.index
        while self.index < len(self.text):
            character = self.text[self.index]
            if character.isalnum() or character in "_.":
                self.index += 1
                continue
            if character == "-" and self.index == start:
                self.index += 1
                continue
            break
        return self.text[start:self.index]

    def _opaque(self) -> str:
        depth = 0
        start = self.index
        while self.index < len(self.text):
            self._skip()
            if self.index >= len(self.text):
                break
            character = self.text[self.index]
            if character == '"':
                self._string()
                continue
            if character in "{[(":
                depth += 1
            elif character in "}])":
                if depth == 0:
                    break
                depth -= 1
            elif character == "," and depth == 0:
                break
            self._advance()
        return self.text[start:self.index]
