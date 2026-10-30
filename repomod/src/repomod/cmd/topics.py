from __future__ import annotations

import argparse
import sys

from repomod.github import topics_many

from . import common


def _topics(args: argparse.Namespace) -> int:
    catalog = common._catalog()
    repositories = common._select(catalog, args.repo, args.tag)
    results = topics_many(repositories, apply=args.operation == "apply", jobs=args.workers)
    failed = False
    for name, changed, additions, removals, error in results:
        if error is not None:
            print(f"{name}: failed: {error}", file=sys.stderr)
            failed = True
            continue
        if not additions and not removals:
            print(f"{name}: already compliant")
            continue
        changes = []
        if additions:
            changes.append("add " + ", ".join(additions))
        if removals:
            changes.append("remove " + ", ".join(removals))
        print(f"{name}: {'applied' if changed else 'diff'}: {'; '.join(changes)}")
    return 1 if failed else 0
