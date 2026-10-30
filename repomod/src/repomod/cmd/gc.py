from __future__ import annotations

import argparse

from repomod.clean import gc_repositories

from . import common


def _gc(args: argparse.Namespace) -> int:
    catalog = common._catalog()
    results = gc_repositories(
        catalog, dry_run=args.dry_run, force=args.force, jobs=args.workers
    )
    failed = False
    for result in results:
        print(f"{result.repository}: {result.action}: {result.message}")
        failed |= result.failed
    if not results:
        print("no unreferenced checkouts")
    return 1 if failed else 0
