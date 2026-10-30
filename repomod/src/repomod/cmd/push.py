from __future__ import annotations

import argparse
import sys
from concurrent.futures import ThreadPoolExecutor, as_completed

from repomod import catalog as catalog_module
from repomod import fix as fix_engine
from repomod import git
from repomod.types import Repository

from . import common


def _push(args: argparse.Namespace) -> int:
    if args.workers < 1:
        raise ValueError("--workers must be at least one")
    catalog = common._catalog()
    repositories = common._select(catalog, args.repo, args.tag)

    def push_one(repository: Repository) -> tuple[str, str, str | None]:
        try:
            with git.repository_lock(catalog.root, repository.id):
                path = catalog_module.repository_path(catalog, repository)
                state, message = fix_engine.push_repository(repository, path)
                return str(repository.id), state, message
        except (catalog_module.CatalogError, git.GitError, OSError) as error:
            return str(repository.id), "failed", str(error)

    results: list[tuple[str, str, str | None]] = []
    with ThreadPoolExecutor(
        max_workers=min(args.workers, 32, max(1, len(repositories))),
        thread_name_prefix="repomod-push",
    ) as pool:
        futures = {pool.submit(push_one, repository): repository for repository in repositories}
        for future in as_completed(futures):
            try:
                results.append(future.result())
            except Exception as error:
                repository = futures[future]
                results.append((str(repository.id), "failed", str(error)))
    failed = False
    for name, state, message in sorted(results, key=lambda item: item[0].casefold()):
        line = f"{name}: {state}" + (f": {message}" if message is not None else "")
        if state == "failed":
            print(line, file=sys.stderr)
            failed = True
        else:
            print(line)
    return 1 if failed else 0
