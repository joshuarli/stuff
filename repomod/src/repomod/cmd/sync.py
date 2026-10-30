from __future__ import annotations

import argparse
import sys

from repomod import github
from repomod.sync import sync_repositories, untrack_archived_repositories

from . import common


def _sync(args: argparse.Namespace) -> int:
    catalog = common._catalog()
    repositories = catalog.repositories
    print(
        f"Fetching GitHub metadata for {len(repositories)} repositories...",
        file=sys.stderr,
        flush=True,
    )
    remotes = github.remote_metadata(
        repositories,
        on_batch_complete=lambda completed, total: _sync_progress(
            "GitHub metadata batches", completed, total
        ),
    )
    print(
        f"Syncing {len(repositories)} repositories...",
        file=sys.stderr,
        flush=True,
    )
    results = sync_repositories(
        catalog,
        remotes,
        dry_run=args.dry_run,
        jobs=args.workers,
        on_complete=lambda completed, total, result: _sync_progress(
            "Repository progress", completed, total, f"{result.repository}: {result.action}"
        ),
    )
    failed = False
    for result in results:
        print(f"{result.repository}: {result.action}: {result.message}")
        failed |= result.failed
    for result in untrack_archived_repositories(catalog, remotes, results, dry_run=args.dry_run):
        print(f"{result.repository}: {result.action}: {result.message}")
        failed |= result.failed
    return 1 if failed else 0


def _sync_progress(stage: str, completed: int, total: int, detail: str = "") -> None:
    message = f"{stage}: {completed}/{total} complete"
    if detail:
        message += f" ({detail})"
    if sys.stderr.isatty():
        print(f"\r{message}\033[K", end="", file=sys.stderr, flush=True)
        if completed == total:
            print(file=sys.stderr)
    else:
        print(message, file=sys.stderr, flush=True)
