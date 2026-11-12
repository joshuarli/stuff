from __future__ import annotations

import os
import re
from pathlib import Path, PurePosixPath

from repomod.git import GitError, tracked_paths
from repomod.rules.workspace import is_workspace_excluded, workspace_excluded
from repomod.types import Finding, Repository, Rule, TagPredicate, operational, violation

_RULE_ID = "rust-collections"

_BANNED = re.compile(r"\b(BTreeMap|BTreeSet)\b")
_MESSAGES = {
    "BTreeMap": (
        "std::collections::BTreeMap is banned: use rustc-hash 2.x for small integer-heavy maps, "
        "rapidhash::fast for general-purpose performance, or foldhash::fast with hashbrown"
    ),
    "BTreeSet": (
        "std::collections::BTreeSet is banned: use rustc-hash 2.x for small integer-heavy sets, "
        "rapidhash::fast for general-purpose performance, or foldhash::fast with hashbrown"
    ),
}
_SKIPPED_PARTS = frozenset({".git", ".work", "node_modules", "target", "third_party", "vendor"})
_IDENTIFIER = re.compile(r"\w")


def _identifier_char(char: str) -> bool:
    return _IDENTIFIER.match(char) is not None


def _block_comment_end(source: str, start: int) -> int:
    depth = 1
    cursor = start + 2
    while cursor < len(source) and depth:
        if source.startswith("/*", cursor):
            depth += 1
            cursor += 2
        elif source.startswith("*/", cursor):
            depth -= 1
            cursor += 2
        else:
            cursor += 1
    return cursor


def _string_end(source: str, start: int) -> int:
    cursor = start + 1
    while cursor < len(source):
        if source[cursor] == "\\":
            cursor += 2
        elif source[cursor] == '"':
            return cursor + 1
        else:
            cursor += 1
    return len(source)


def _raw_quote(source: str, start: int) -> tuple[int, int] | None:
    if start and _identifier_char(source[start - 1]):
        return None
    if source.startswith("br", start):
        cursor = start + 2
    elif source[start] == "r":
        cursor = start + 1
    else:
        return None
    hashes = 0
    while cursor < len(source) and source[cursor] == "#":
        hashes += 1
        cursor += 1
    if cursor < len(source) and source[cursor] == '"':
        return cursor, hashes
    return None


def _raw_string_end(source: str, start: int) -> int | None:
    quote = _raw_quote(source, start)
    if quote is None:
        return None
    position, hashes = quote
    closing = '"' + "#" * hashes
    end = source.find(closing, position + 1)
    return len(source) if end == -1 else end + len(closing)


def _char_literal_end(source: str, start: int) -> int | None:
    if start + 1 >= len(source):
        return None
    if source[start + 1] == "\\":
        end = source.find("'", start + 2, min(len(source), start + 14))
        return None if end == -1 else end + 1
    if start + 2 < len(source) and source[start + 2] == "'":
        return start + 3
    return None


def _mask_noncode(source: str) -> str:
    masked = list(source)
    length = len(source)
    index = 0
    while index < length:
        end: int | None = None
        if source.startswith("//", index):
            newline = source.find("\n", index)
            end = length if newline == -1 else newline
        elif source.startswith("/*", index):
            end = _block_comment_end(source, index)
        elif source[index] == '"':
            end = _string_end(source, index)
        elif source[index] in {"b", "r"}:
            end = _raw_string_end(source, index)
        elif source[index] == "'":
            end = _char_literal_end(source, index)
        if end is None:
            index += 1
            continue
        for position in range(index, end):
            if masked[position] != "\n":
                masked[position] = " "
        index = end
    return "".join(masked)


def _position(source: str, offset: int) -> tuple[int, int]:
    return source.count("\n", 0, offset) + 1, offset - source.rfind("\n", 0, offset)


def _read_source(root: Path, relative: PurePosixPath) -> str:
    current = root
    for part in relative.parts:
        current = current / part
        if current.is_symlink():
            raise OSError("symbolic links are not followed")
    descriptor = os.open(current, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(descriptor, encoding="utf-8") as source:
        return source.read()


def _source_findings(root: Path, repository: Repository, relative: str) -> list[Finding]:
    path = PurePosixPath(relative)
    if any(part.casefold() in _SKIPPED_PARTS for part in path.parts):
        return []
    try:
        source = _read_source(root, path)
    except (OSError, UnicodeError) as error:
        return [
            operational(
                repository,
                _RULE_ID,
                relative,
                f"cannot read Rust source: {error}",
            )
        ]
    masked = _mask_noncode(source)
    findings = []
    for match in _BANNED.finditer(masked):
        name = match.group(1)
        line, column = _position(masked, match.start())
        findings.append(
            violation(
                repository,
                _RULE_ID,
                relative,
                _MESSAGES[name],
                line=line,
                column=column,
            )
        )
    return findings


def _check(root: Path, repository: Repository) -> list[Finding]:
    if "fork" in repository.tags:
        return []
    try:
        paths = tracked_paths(root, (":(glob)**/*.rs",))
    except GitError as error:
        return [
            operational(repository, _RULE_ID, None, f"cannot list Rust sources: {error}")
        ]
    excluded = workspace_excluded(root)
    findings = []
    for relative in paths:
        if is_workspace_excluded(excluded, PurePosixPath(relative)):
            continue
        findings.extend(_source_findings(root, repository, relative))
    return findings


def rust_collection_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            _RULE_ID,
            "Ban std::collections BTreeMap and BTreeSet in Rust source",
            TagPredicate(),
            _check,
            None,
            None,
            (),
        ),
    )
