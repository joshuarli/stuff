from __future__ import annotations

from pathlib import PurePosixPath

FORK_WORKFLOW_NAMES = frozenset({"lint", "prerelease", "test"})


def is_fork_workflow(path: str | PurePosixPath) -> bool:
    workflow_path = str(path).split("@", 1)[0]
    return PurePosixPath(workflow_path).stem.casefold() in FORK_WORKFLOW_NAMES
