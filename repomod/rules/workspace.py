from __future__ import annotations

import tomllib
from pathlib import Path, PurePosixPath


def workspace_excluded(root: Path) -> frozenset[PurePosixPath]:
    try:
        manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError):
        return frozenset[PurePosixPath]()
    workspace = manifest.get("workspace")
    if not isinstance(workspace, dict):
        return frozenset[PurePosixPath]()
    excluded = workspace.get("exclude", [])
    if not isinstance(excluded, list):
        return frozenset[PurePosixPath]()
    return frozenset(
        prefix
        for entry in excluded
        if isinstance(entry, str)
        for prefix in (PurePosixPath(entry),)
        if prefix.parts
    )


def is_workspace_excluded(prefixes: frozenset[PurePosixPath], path: PurePosixPath) -> bool:
    return any(path.parts[: len(prefix.parts)] == prefix.parts for prefix in prefixes)
