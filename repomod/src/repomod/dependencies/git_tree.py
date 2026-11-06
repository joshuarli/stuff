from __future__ import annotations

import re
from collections.abc import Iterable
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

from repomod.git import GitError, run_git, run_git_bytes

_NO_LAZY_FETCH = {"GIT_NO_LAZY_FETCH": "1"}
_OBJECT_ID = re.compile(r"[0-9a-f]{40}(?:[0-9a-f]{24})?\Z")

type EntryKind = Literal["blob", "symlink", "gitlink"]


@dataclass(frozen=True, slots=True)
class TreeEntry:
    path: str
    object_id: str
    kind: EntryKind


def default_commit(path: Path) -> str:
    result = run_git(
        ["rev-parse", "--verify", "refs/remotes/origin/HEAD^{commit}"],
        cwd=path,
        check=False,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or f"exit {result.returncode}"
        raise GitError(f"cannot resolve refs/remotes/origin/HEAD: {detail}")
    value = result.stdout.strip()
    if not _OBJECT_ID.fullmatch(value):
        raise GitError("git returned an invalid origin/HEAD object id")
    return value


def tree_entries(path: Path, commit: str) -> tuple[TreeEntry, ...]:
    result = run_git_bytes(
        ["ls-tree", "-r", "-z", "--full-tree", commit],
        cwd=path,
        env=_NO_LAZY_FETCH,
    )
    entries: list[TreeEntry] = []
    for record in result.stdout.split(b"\0"):
        if not record:
            continue
        metadata, separator, raw_path = record.partition(b"\t")
        if not separator:
            raise GitError("git ls-tree returned a record without a path")
        fields = metadata.split(b" ")
        if len(fields) != 3:
            raise GitError("git ls-tree returned invalid metadata")
        raw_mode, raw_kind, raw_object = fields
        try:
            entry_path = raw_path.decode("utf-8")
        except UnicodeDecodeError:
            continue
        mode = raw_mode.decode("ascii", "replace")
        object_id = raw_object.decode("ascii", "replace")
        if not _OBJECT_ID.fullmatch(object_id):
            raise GitError("git ls-tree returned an invalid object id")
        kind = _entry_kind(mode, raw_kind)
        if kind is None:
            continue
        entries.append(TreeEntry(entry_path, object_id, kind))
    return tuple(sorted(entries, key=lambda entry: entry.path))


def _entry_kind(mode: str, raw_kind: bytes) -> EntryKind | None:
    if mode == "160000":
        return "gitlink"
    if mode == "120000":
        return "symlink"
    if raw_kind == b"blob":
        return "blob"
    return None


def read_blobs(path: Path, object_ids: Iterable[str]) -> dict[str, bytes]:
    requested = sorted(set(object_ids))
    if not requested:
        return {}
    for object_id in requested:
        if not _OBJECT_ID.fullmatch(object_id):
            raise GitError(f"invalid Git object id: {object_id!r}")
    request = "".join(f"{object_id}\n" for object_id in requested).encode("ascii")
    result = run_git_bytes(["cat-file", "--batch"], cwd=path, input_bytes=request, env=_NO_LAZY_FETCH)
    contents: dict[str, bytes] = {}
    offset = 0
    for object_id in requested:
        newline = result.stdout.find(b"\n", offset)
        if newline < 0:
            raise GitError("git cat-file returned a truncated header")
        header = result.stdout[offset:newline].decode("ascii", "replace")
        offset = newline + 1
        fields = header.split()
        if len(fields) == 2 and fields[1] == "missing":
            raise GitError(f"missing Git object: {fields[0]}")
        if len(fields) != 3 or fields[0] != object_id or fields[1] != "blob":
            raise GitError(f"git cat-file returned an unexpected header: {header}")
        try:
            size = int(fields[2])
        except ValueError as error:
            raise GitError(f"git cat-file returned an invalid size: {header}") from error
        content = result.stdout[offset : offset + size]
        if len(content) != size:
            raise GitError(f"git cat-file returned a truncated object: {object_id}")
        offset += size
        if result.stdout[offset : offset + 1] != b"\n":
            raise GitError(f"git cat-file returned a malformed object: {object_id}")
        offset += 1
        contents[object_id] = content
    return contents
