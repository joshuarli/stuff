from __future__ import annotations

import shutil
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from pathlib import Path

from repomod.catalog import CatalogError, remove_repository, repository_path
from repomod.clean import removal_blocker
from repomod.git import (
    GitError,
    clone_repository,
    clone_url,
    complete_history,
    ensure_upstream_remote,
    fetch_default,
    fetch_upstream,
    head,
    repository_lock,
    safe_fast_forward,
    shallow,
    stash_count,
    status,
    upstream_url,
)
from repomod.types import Catalog, RemoteRepository, RepoId, Repository


@dataclass(frozen=True, slots=True)
class SyncResult:
    repository: str
    action: str
    message: str
    failed: bool = False


def _upstream_plan(remote: RemoteRepository | None) -> str:
    if remote is None or remote.parent is None or remote.parent_branch is None:
        return ""
    return f"; would ensure the upstream {remote.parent} remote and fetch {remote.parent_branch}"


def _sync_upstream(path: Path, remote: RemoteRepository) -> str | None:
    if remote.parent is None or remote.parent_branch is None:
        return None
    try:
        state = ensure_upstream_remote(path, upstream_url(remote.parent))
        fetch_upstream(path, remote.parent_branch, 1 if shallow(path) else None)
    except (GitError, OSError) as error:
        return f"upstream {remote.parent} unavailable: {error}"
    if state == "added":
        return f"upstream {remote.parent} added and fetched"
    if state == "updated":
        return f"upstream {remote.parent} repointed and fetched"
    return f"upstream {remote.parent} fetched"


def _remove_archived_checkout(catalog: Catalog, repo: Repository, *, dry_run: bool) -> SyncResult:
    name = str(repo.id)
    try:
        path = repository_path(catalog, repo)
    except (CatalogError, OSError) as error:
        return SyncResult(name, "failed", str(error), True)
    try:
        if dry_run:
            current = status(path, repo.id)
            if not current.present:
                return SyncResult(name, "skipped", "repository is archived; no checkout to remove")
            if not current.valid:
                return SyncResult(name, "failed", current.error or "invalid checkout", True)
            if not current.origin_valid:
                return SyncResult(name, "failed", current.error or "invalid origin", True)
            blocker = removal_blocker(current, stash_count(path))
            if blocker is not None:
                return SyncResult(
                    name, "skipped", f"{blocker}; refusing to remove archived checkout", True
                )
            return SyncResult(name, "planned", f"would remove archived checkout at {path}")
        with repository_lock(catalog.root, repo.id):
            path = repository_path(catalog, repo)
            current = status(path, repo.id)
            if not current.present:
                return SyncResult(name, "skipped", "repository is archived; no checkout to remove")
            if not current.valid:
                return SyncResult(
                    name,
                    "skipped",
                    f"{current.error or 'invalid checkout'}; refusing to remove an unverified path",
                    True,
                )
            if not current.origin_valid:
                return SyncResult(
                    name,
                    "skipped",
                    f"{current.error or 'invalid origin'}; refusing to remove an unverified checkout",
                    True,
                )
            blocker = removal_blocker(current, stash_count(path))
            if blocker is not None:
                return SyncResult(
                    name, "skipped", f"{blocker}; refusing to remove archived checkout", True
                )
            shutil.rmtree(path)
            return SyncResult(name, "removed", "removed archived checkout")
    except (CatalogError, GitError, OSError) as error:
        return SyncResult(name, "failed", str(error), True)


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
    except (CatalogError, OSError) as error:
        return SyncResult(name, "failed", str(error), True)
    if dry_run:
        if remote is not None and remote.archived is True:
            return _remove_archived_checkout(catalog, repo, dry_run=True)
        try:
            current = status(path, repo.id)
        except (GitError, OSError) as error:
            return SyncResult(name, "failed", str(error), True)
        if not current.present:
            return SyncResult(name, "planned", f"would clone into {path}{_upstream_plan(remote)}")
        if not current.valid:
            return SyncResult(name, "failed", current.error or "invalid checkout", True)
        if not current.origin_valid:
            return SyncResult(name, "failed", current.error or "invalid origin", True)
        return SyncResult(
            name,
            "planned",
            "would fetch remote default branch and fast-forward only a clean checkout on it"
            f"{_upstream_plan(remote)}",
        )
    if remote is None:
        return SyncResult(name, "failed", "GitHub repository is unavailable", True)
    if remote.archived is True:
        return _remove_archived_checkout(catalog, repo, dry_run=False)
    if remote.error is not None:
        return SyncResult(name, "failed", remote.error, True)
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
                note = _sync_upstream(path, remote)
                message = f"cloned {remote.default_branch}"
                if note is not None:
                    message += f"; {note}"
                return SyncResult(name, "cloned", message)
            if not current.valid:
                return SyncResult(name, "failed", current.error or "invalid checkout", True)
            if not current.origin_valid:
                return SyncResult(name, "failed", current.error or "invalid checkout origin", True)
            notes: list[str] = []
            if shallow(path):
                try:
                    complete_history(path, remote.default_branch)
                except (GitError, OSError) as error:
                    return SyncResult(name, "failed", f"could not complete history from shallow clone: {error}", True)
                notes.append("completed full history")
            upstream_note = _sync_upstream(path, remote)
            if upstream_note is not None:
                notes.append(upstream_note)
            trailer = "".join(f"; {note}" for note in notes)
            starting_head = head(path)
            if (
                not current.dirty
                and not current.detached
                and current.branch == remote.default_branch
                and remote.sha is not None
                and starting_head.casefold() == remote.sha.casefold()
            ):
                return SyncResult(name, "current", f"already at GitHub's reported default branch tip{trailer}")
            fetch_default(path, remote.default_branch, 1 if shallow(path) else None)
            if current.dirty:
                return SyncResult(name, "fetched", f"worktree is dirty; checked out files were not changed{trailer}")
            if current.detached:
                return SyncResult(name, "fetched", f"HEAD is detached; checked out files were not changed{trailer}")
            if current.branch != remote.default_branch:
                return SyncResult(
                    name,
                    "fetched",
                    f"on branch {current.branch}; default is {remote.default_branch}{trailer}",
                )
            result = safe_fast_forward(path, remote.default_branch, starting_head)
            return SyncResult(name, "updated" if result == "fast-forwarded" else "fetched", f"{result}{trailer}")
    except CatalogError as error:
        return SyncResult(name, "failed", str(error), True)
    except (GitError, OSError) as error:
        return SyncResult(name, "failed", str(error), True)


def untrack_archived_repositories(
    catalog: Catalog,
    remotes: dict[str, RemoteRepository],
    results: list[SyncResult],
    *,
    dry_run: bool = False,
) -> list[SyncResult]:
    untracked: list[SyncResult] = []
    for result in sorted(results, key=lambda item: item.repository.casefold()):
        if result.failed:
            continue
        remote = remotes.get(result.repository)
        if remote is None or remote.archived is not True:
            continue
        try:
            repo_id = RepoId.parse(result.repository)
        except ValueError as error:
            untracked.append(SyncResult(result.repository, "failed", str(error), True))
            continue
        if dry_run:
            untracked.append(
                SyncResult(
                    result.repository,
                    "planned",
                    "would untrack archived repository from catalog",
                )
            )
            continue
        try:
            changed = remove_repository(catalog, repo_id.owner, repo_id.name)
        except (CatalogError, OSError) as error:
            untracked.append(SyncResult(result.repository, "failed", str(error), True))
            continue
        if changed:
            untracked.append(
                SyncResult(
                    result.repository,
                    "untracked",
                    "commented out of catalog.toml (archived on GitHub)",
                )
            )
        else:
            untracked.append(
                SyncResult(result.repository, "skipped", "already untracked from catalog")
            )
    return untracked


def sync_repositories(
    catalog: Catalog,
    remotes: dict[str, RemoteRepository],
    *,
    dry_run: bool = False,
    jobs: int = 4,
    on_complete: Callable[[int, int, SyncResult], None] | None = None,
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
                result = future.result()
            except Exception as error:
                result = SyncResult(str(repo.id), "failed", str(error), True)
            results.append(result)
            if on_complete is not None:
                on_complete(len(results), len(repos), result)
    return sorted(results, key=lambda item: item.repository.casefold())
