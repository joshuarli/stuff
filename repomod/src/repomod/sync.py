from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass

from repomod.catalog import CatalogError, repository_path
from repomod.git import (
    GitError,
    clone_repository,
    clone_url,
    fetch_default,
    head,
    repository_lock,
    safe_fast_forward,
    shallow,
    status,
)
from repomod.types import Catalog, RemoteRepository, Repository


@dataclass(frozen=True, slots=True)
class SyncResult:
    repository: str
    action: str
    message: str
    failed: bool = False


def sync_one(
    catalog: Catalog,
    repo: Repository,
    remote: RemoteRepository | None,
    *,
    dry_run: bool = False,
) -> SyncResult:
    name = str(repo.id)
    try:
        path = repository_path(catalog, repo)
        current = status(path, repo.id)
    except (CatalogError, GitError, OSError) as error:
        return SyncResult(name, "failed", str(error), True)
    if dry_run:
        if not current.present:
            return SyncResult(name, "planned", f"would clone into {path}")
        if not current.valid:
            return SyncResult(name, "failed", current.error or "invalid checkout", True)
        if not current.origin_valid:
            return SyncResult(name, "failed", current.error or "invalid origin", True)
        return SyncResult(
            name,
            "planned",
            "would fetch remote default branch and fast-forward only a clean checkout on it",
        )
    if remote is None:
        return SyncResult(name, "failed", "GitHub repository is unavailable", True)
    if remote.error is not None:
        return SyncResult(name, "failed", remote.error, True)
    if remote.archived is True:
        return SyncResult(name, "skipped", "repository is archived")
    if remote.archived is not False:
        return SyncResult(name, "failed", "GitHub did not confirm repository is active", True)
    if remote.default_branch is None:
        return SyncResult(name, "failed", "GitHub did not report a default branch", True)
    try:
        with repository_lock(catalog.root, repo.id):
            path = repository_path(catalog, repo)
            current = status(path, repo.id)
            if not current.present:
                clone_repository(path, repo.id, clone_url(repo.id), remote.default_branch)
                return SyncResult(name, "cloned", f"cloned {remote.default_branch}")
            if not current.valid:
                return SyncResult(name, "failed", current.error or "invalid checkout", True)
            if not current.origin_valid:
                return SyncResult(name, "failed", current.error or "invalid checkout origin", True)
            starting_head = head(path)
            fetch_default(path, remote.default_branch, 1 if shallow(path) else None)
            if current.dirty:
                return SyncResult(name, "fetched", "worktree is dirty; checked out files were not changed")
            if current.detached:
                return SyncResult(name, "fetched", "HEAD is detached; checked out files were not changed")
            if current.branch != remote.default_branch:
                return SyncResult(
                    name,
                    "fetched",
                    f"on branch {current.branch}; default is {remote.default_branch}",
                )
            result = safe_fast_forward(path, remote.default_branch, starting_head)
            return SyncResult(name, "updated" if result == "fast-forwarded" else "fetched", result)
    except CatalogError as error:
        return SyncResult(name, "failed", str(error), True)
    except (GitError, OSError) as error:
        return SyncResult(name, "failed", str(error), True)


def sync_repositories(
    catalog: Catalog,
    remotes: dict[str, RemoteRepository],
    *,
    dry_run: bool = False,
    jobs: int = 4,
) -> list[SyncResult]:
    if jobs < 1:
        raise ValueError("jobs must be at least one")
    repos = catalog.repositories
    if not repos:
        return []
    workers = min(jobs, 32, len(repos))
    results: list[SyncResult] = []
    with ThreadPoolExecutor(max_workers=workers, thread_name_prefix="repomod-sync") as pool:
        futures = {
            pool.submit(sync_one, catalog, repo, remotes.get(str(repo.id)), dry_run=dry_run): repo
            for repo in repos
        }
        for future in as_completed(futures):
            repo = futures[future]
            try:
                results.append(future.result())
            except Exception as error:
                results.append(SyncResult(str(repo.id), "failed", str(error), True))
    return sorted(results, key=lambda item: item.repository.casefold())
