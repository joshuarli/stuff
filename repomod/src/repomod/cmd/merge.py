from __future__ import annotations

import argparse
import sys
from concurrent.futures import ThreadPoolExecutor, as_completed

from repomod import github
from repomod.types import RepomodPullRequest, Repository

from . import common

_MERGE_WORKERS_LIMIT = 16

type MergeOutcome = tuple[RepomodPullRequest, str, str | None]


def _merge_repository(
    repository: Repository,
    requests: list[RepomodPullRequest],
    *,
    dry_run: bool,
) -> list[MergeOutcome]:
    outcomes: list[MergeOutcome] = []
    for request in requests:
        if request.draft:
            outcomes.append((request, "skipped", "pull request is a draft"))
            continue
        if dry_run:
            outcomes.append((request, "would-merge", None))
            continue
        try:
            sha = github.merge_repomod_pull_request(
                repository.id, request.number, request.head_sha
            )
        except github.GitHubError as error:
            outcomes.append((request, "failed", str(error)))
        else:
            outcomes.append((request, "merged", sha))
    return outcomes


def _merge(args: argparse.Namespace) -> int:
    if args.workers < 1:
        raise ValueError("--workers must be at least one")
    catalog = common._catalog()
    repositories = common._select(catalog, args.repo, args.tag)
    listed = github.repomod_pull_requests(repositories, jobs=args.workers)
    failed = False
    pending: list[tuple[Repository, list[RepomodPullRequest]]] = []
    for repository in repositories:
        name = str(repository.id)
        requests, error = listed.get(name, ([], "GitHub did not return a pull request list"))
        if error is not None:
            print(f"{name}: failed: {error}", file=sys.stderr)
            failed = True
        elif requests:
            pending.append((repository, requests))
    results: list[tuple[Repository, list[MergeOutcome]]] = []
    if pending:
        with ThreadPoolExecutor(
            max_workers=min(args.workers, _MERGE_WORKERS_LIMIT, len(pending)),
            thread_name_prefix="repomod-merge",
        ) as pool:
            futures = {
                pool.submit(
                    _merge_repository, repository, requests, dry_run=args.dry_run
                ): repository
                for repository, requests in pending
            }
            for future in as_completed(futures):
                repository = futures[future]
                try:
                    outcomes = future.result()
                except Exception as error:
                    print(f"{repository.id}: failed: {error}", file=sys.stderr)
                    failed = True
                    continue
                results.append((repository, outcomes))
    for repository, outcomes in sorted(
        results, key=lambda item: str(item[0].id).casefold()
    ):
        name = str(repository.id)
        for request, state, detail in outcomes:
            if state == "merged":
                assert detail is not None
                print(f"{name}: #{request.number} merged {detail[:12]} {request.url}")
            elif state == "would-merge":
                print(f"{name}: #{request.number} would merge {request.branch} {request.url}")
            else:
                print(
                    f"{name}: #{request.number} {state}: {detail} {request.url}",
                    file=sys.stderr,
                )
                failed |= state == "failed"
    return 1 if failed else 0
