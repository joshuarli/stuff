from __future__ import annotations

import json
import os
import re
import stat
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from repomod.rules.languages import _cargo_candidates
from repomod.types import Finding, Repository, Rule, TagPredicate
from repomod.types import operational as _operational
from repomod.types import violation as _violation

_PATH = "rust-toolchain.toml"
_CHANNEL = "nightly-2026-09-15"
_REQUIRED_COMPONENTS = ("rust-src", "clippy")
_ASSIGNMENT = re.compile(r"^[ \t]*(?P<key>channel|components|'channel'|'components'|\"channel\"|\"components\")[ \t]*=")
_TABLE = re.compile(r"\[\s*(.*?)\s*\](?:[ \t]*#.*)?\Z")


@dataclass(frozen=True, slots=True)
class _ValueSpan:
    start: int
    end: int


@dataclass(frozen=True, slots=True)
class _Layout:
    table_start: int | None
    table_end: int
    fields: dict[str, tuple[_ValueSpan, ...]]


@dataclass(frozen=True, slots=True)
class _Config:
    source: str | None
    document: dict[str, Any] | None
    problem: str | None
    operational: bool = False


def _string_end(source: str, start: int) -> int:
    quote = source[start]
    delimiter = quote * 3 if source.startswith(quote * 3, start) else quote
    multiline = len(delimiter) == 3
    index = start + len(delimiter)
    while index < len(source):
        character = source[index]
        if quote == '"' and character == "\\":
            index += 2
            continue
        if multiline:
            if character == quote:
                end = index
                while end < len(source) and source[end] == quote:
                    end += 1
                if end - index >= 3:
                    return end
        elif character == quote:
            return index + 1
        index += 1
    raise ValueError("unterminated TOML string")


def _value_end(source: str, start: int) -> int:
    depth = 0
    last_value_end = start
    index = start
    while index < len(source):
        character = source[index]
        if character in {'"', "'"}:
            index = _string_end(source, index)
            last_value_end = index
            continue
        if character == "#":
            newline = source.find("\n", index)
            index = len(source) if newline < 0 else newline
        elif character in "[{":
            depth += 1
            index += 1
            last_value_end = index
        elif character in "]}":
            depth -= 1
            index += 1
            last_value_end = index
        elif character == "\n":
            if depth == 0:
                return last_value_end
            index += 1
        else:
            if not character.isspace():
                last_value_end = index + 1
            index += 1
    return last_value_end


def _table_header(line: str) -> tuple[bool, bool]:
    stripped = line.strip()
    if stripped.startswith("[["):
        return True, False
    match = _TABLE.fullmatch(stripped)
    if match is None:
        return False, False
    name = match.group(1).strip()
    return True, name in {"toolchain", '"toolchain"', "'toolchain'"}


def _layout(source: str) -> _Layout:
    section_is_toolchain = False
    table_start = None
    table_end = len(source)
    fields: dict[str, list[_ValueSpan]] = {"channel": [], "components": []}
    index = 0
    line_start = 0
    depth = 0
    while index < len(source):
        if index == line_start and depth == 0:
            newline = source.find("\n", line_start)
            line_end = len(source) if newline < 0 else newline
            line = source[line_start:line_end].rstrip("\r")
            is_header, is_toolchain = _table_header(line)
            if is_header:
                if section_is_toolchain:
                    table_end = line_start
                section_is_toolchain = is_toolchain
                if is_toolchain:
                    table_start = line_start
                index = len(source) if newline < 0 else newline + 1
                line_start = index
                continue
            if section_is_toolchain:
                assignment = _ASSIGNMENT.match(line)
                if assignment is not None:
                    key = assignment.group("key").strip("'\"")
                    value_start = line_start + assignment.end()
                    while value_start < line_end and source[value_start] in " \t":
                        value_start += 1
                    fields[key].append(_ValueSpan(value_start, _value_end(source, value_start)))

        character = source[index]
        if character in {'"', "'"}:
            end = _string_end(source, index)
            last_newline = source.rfind("\n", index, end)
            if last_newline >= 0:
                line_start = last_newline + 1
            index = end
        elif character == "#":
            newline = source.find("\n", index)
            if newline < 0:
                index = len(source)
            else:
                index = newline
        elif character in "[{":
            depth += 1
            index += 1
        elif character in "]}":
            depth -= 1
            index += 1
        elif character == "\n":
            line_start = index + 1
            index += 1
        else:
            index += 1
    return _Layout(table_start, table_end, {key: tuple(values) for key, values in fields.items()})


def _read_config(root: Path) -> _Config:
    path = root / _PATH
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return _Config(None, None, None)
    except OSError as error:
        return _Config(None, None, f"cannot inspect {_PATH}: {error}", operational=True)
    if stat.S_ISLNK(metadata.st_mode):
        return _Config(None, None, f"{_PATH} is a symlink")
    if not stat.S_ISREG(metadata.st_mode):
        return _Config(None, None, f"{_PATH} is not a regular file")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_NONBLOCK", 0)
    try:
        descriptor = os.open(path, flags)
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            os.close(descriptor)
            return _Config(None, None, f"{_PATH} is not a regular file", operational=True)
        with os.fdopen(descriptor, encoding="utf-8", newline="") as source_file:
            source = source_file.read()
    except (OSError, UnicodeError) as error:
        return _Config(None, None, f"cannot read {_PATH}: {error}", operational=True)
    try:
        document = tomllib.loads(source)
    except tomllib.TOMLDecodeError as error:
        return _Config(source, None, f"{_PATH} is invalid TOML: {error}")
    return _Config(source, document, None)


def _is_rust_repository(root: Path, repository: Repository) -> bool:
    return "rust" in repository.tags or bool(_cargo_candidates(root))


def _components(toolchain: dict[str, Any]) -> tuple[list[str] | None, str | None]:
    value = toolchain.get("components", [])
    if not isinstance(value, list):
        return None, "toolchain.components must be an array of strings"
    components = []
    for component in value:
        if not isinstance(component, str):
            return None, "toolchain.components must be an array of strings"
        components.append(component)
    return components, None


def _analysis(config: _Config) -> tuple[str | None, bool]:
    if config.problem is not None:
        return config.problem, False
    if config.source is None:
        return f"declare {_PATH} with toolchain.channel = {_CHANNEL!r} and required components", True
    assert config.document is not None
    toolchain = config.document.get("toolchain")
    if toolchain is None:
        return f"declare [toolchain] with channel = {_CHANNEL!r} and required components", True
    if not isinstance(toolchain, dict):
        return "toolchain must be a table", False
    components, error = _components(toolchain)
    if error is not None:
        return error, False
    assert components is not None
    missing = [component for component in _REQUIRED_COMPONENTS if component not in components]
    problems = []
    if toolchain.get("channel") != _CHANNEL:
        problems.append(f"toolchain.channel must be {_CHANNEL!r}")
    if missing:
        problems.append(f"toolchain.components must include {', '.join(missing)}")
    if not problems:
        return None, False
    layout = _layout(config.source)
    if layout.table_start is None:
        return "; ".join(problems), False
    for key in ("channel", "components"):
        if key in toolchain and len(layout.fields[key]) != 1:
            return "; ".join(problems), False
    return "; ".join(problems), True


def _finding(repository: Repository, message: str, *, fixable: bool, operational: bool = False) -> Finding:
    if operational:
        return _operational(repository, "rust-toolchain", _PATH, message)
    return _violation(repository, "rust-toolchain", _PATH, message, fixable=fixable)


def _findings(root: Path, repository: Repository) -> list[Finding]:
    if not _is_rust_repository(root, repository):
        return []
    config = _read_config(root)
    message, fixable = _analysis(config)
    if message is None:
        return []
    return [_finding(repository, message, fixable=fixable, operational=config.operational)]


def _serialized_components(components: list[str]) -> str:
    values = list(components)
    for required in _REQUIRED_COMPONENTS:
        if required not in values:
            values.append(required)
    return "[" + ", ".join(json.dumps(value, ensure_ascii=False) for value in values) + "]"


def _insert_fields(source: str, end: int, values: list[str]) -> str:
    newline = "\r\n" if "\r\n" in source else "\n"
    prefix = source[:end]
    if prefix and not prefix.endswith(("\n", "\r")):
        prefix += newline
    suffix = source[end:]
    return prefix + "".join(value + newline for value in values) + suffix


def _updated_source(config: _Config) -> str:
    if config.problem is not None:
        raise ValueError(f"cannot fix {_PATH}: {config.problem}")
    if config.source is None:
        components = _serialized_components([])
        return f'[toolchain]\nchannel = "{_CHANNEL}"\ncomponents = {components}\n'
    if config.document is None:
        raise ValueError(f"cannot fix {_PATH}: {config.problem}")
    toolchain = config.document.get("toolchain")
    if toolchain is None:
        source = config.source
        separator = "\r\n" if "\r\n" in source else "\n"
        prefix = source
        if prefix and not prefix.endswith(("\n", "\r")):
            prefix += separator
        if prefix and not prefix.endswith(separator * 2):
            prefix += separator
        components = _serialized_components([])
        return prefix + f'[toolchain]{separator}channel = "{_CHANNEL}"{separator}components = {components}{separator}'
    if not isinstance(toolchain, dict):
        raise ValueError("cannot fix rust-toolchain.toml because toolchain is not a table")
    components, error = _components(toolchain)
    if error is not None:
        raise ValueError(f"cannot fix rust-toolchain.toml: {error}")
    assert components is not None
    layout = _layout(config.source)
    if layout.table_start is None:
        raise ValueError("cannot safely edit toolchain settings outside a [toolchain] table")
    replacements: list[tuple[int, int, str]] = []
    additions = []
    channel_value = toolchain.get("channel")
    if channel_value != _CHANNEL:
        spans = layout.fields["channel"]
        if spans:
            replacements.append((spans[0].start, spans[0].end, json.dumps(_CHANNEL)))
        else:
            additions.append(f'channel = "{_CHANNEL}"')
    required_components = _serialized_components(components)
    if any(value not in components for value in _REQUIRED_COMPONENTS):
        spans = layout.fields["components"]
        if spans:
            replacements.append((spans[0].start, spans[0].end, required_components))
        else:
            additions.append(f"components = {required_components}")
    result = config.source
    if additions:
        result = _insert_fields(result, layout.table_end, additions)
    for start, end, value in sorted(replacements, reverse=True):
        result = result[:start] + value + result[end:]
    return result


def _fix(root: Path, repository: Repository) -> dict[str, str]:
    if not _is_rust_repository(root, repository):
        return {}
    config = _read_config(root)
    return {_PATH: _updated_source(config)}


def rust_toolchain_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            "rust-toolchain",
            "Require the pinned nightly Rust toolchain and rust-src and clippy components",
            TagPredicate(),
            _findings,
            _fix,
            "chore(repomod): enforce rust toolchain policy",
            (_PATH,),
        ),
    )
