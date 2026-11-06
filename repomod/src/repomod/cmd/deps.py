from __future__ import annotations

import argparse
import sys
from collections.abc import Sequence
from pathlib import Path

from repomod.cmd import common
from repomod.dependencies.model import RepositoryData, RepositoryState


def _root() -> Path:
    return Path(__file__).resolve().parents[3]


def _inventory() -> Path:
    return _root() / "data" / "dependencies" / "v1"


def _display(path: Path) -> str:
    try:
        return path.relative_to(_root()).as_posix()
    except ValueError:
        return str(path)


def _deps(args: argparse.Namespace) -> int:
    from repomod.dependencies import snapshot as snapshot_module

    catalog = common.default_catalog()
    repositories = common.select_repositories(catalog, args.repo, args.tag)
    filtered = bool(args.repo or args.tag)
    inventory = _inventory()
    if args.operation == "snapshot":
        result = snapshot_module.snapshot(
            catalog,
            repositories,
            inventory=inventory,
            filtered=filtered,
            workers=args.workers,
        )
        _print_states(result.repositories)
        if result.written:
            print(f"wrote {len(result.written)} files")
        else:
            print("inventory is current")
        if result.deleted:
            print(f"deleted {len(result.deleted)} stale files")
        return _strict(result.repositories, args.strict)
    result = snapshot_module.check(
        catalog,
        repositories,
        inventory=inventory,
        filtered=filtered,
    )
    for path in result.drifted:
        print(_display(path))
    if result.drifted:
        return 1
    return _strict(result.repositories, args.strict)


def _print_states(repositories: Sequence[RepositoryData]) -> None:
    counts = dict.fromkeys(RepositoryState, 0)
    for item in repositories:
        counts[item.state] += 1
    summary = ", ".join(f"{state.value} {count}" for state, count in counts.items())
    print(f"{len(repositories)} repositories: {summary}")


def _strict(repositories: Sequence[RepositoryData], strict: bool) -> int:
    if not strict:
        return 0
    failures = [
        item.repository
        for item in repositories
        if item.state in {RepositoryState.FAILED, RepositoryState.UNAVAILABLE}
    ]
    if not failures:
        return 0
    print(
        f"strict: {len(failures)} repositories are failed or unavailable",
        file=sys.stderr,
    )
    return 1
