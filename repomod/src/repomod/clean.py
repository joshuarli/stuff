from __future__ import annotations

import shutil
from collections.abc import Sequence
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

from repomod.catalog import CatalogError, repository_path
from repomod.git import GitError, repository_lock, stash_count, status
from repomod.types import Catalog, GitStatus, Repository

type CleanAction = Literal["absent", "planned", "removed", "skipped", "failed"]


@dataclass(frozen=True, slots=True)
class CleanResult:
    repository: str
    path: Path | None
    action: CleanAction
    message: str

    @property
    def failed(self) -> bool:
        return self.action in {"skipped", "failed"}


def _blocker(current: GitStatus, stashes: int) -> str | None:
    if current.dirty:
        return "checkout has uncommitted changes"
    if current.detached:
        return "HEAD is detached"
    if current.branch is None:
        return "no branch is checked out"
    if current.upstream is None:
        return f"branch {current.branch} has no upstream"
    if current.ahead is None:
        return f"branch {current.branch} has an unknown number of unpushed commits"
    if current.ahead:
        return f"branch {current.branch} is {current.ahead} commits ahead of {current.upstream}"
    if stashes:
        noun = "entry" if stashes == 1 else "entries"
        return f"repository has {stashes} stash {noun}"
    return None


def clean_one(
    catalog: Catalog,
    repo: Repository,
    *,
    dry_run: bool,
    force: bool,
) -> CleanResult:
    name = str(repo.id)
    try:
        path = repository_path(catalog, repo)
    except (CatalogError, OSError) as error:
        return CleanResult(name, None, "failed", str(error))
    try:
        with repository_lock(catalog.root, repo.id):
            current = status(path, repo.id)
            if not current.present:
                return CleanResult(name, path, "absent", "no checkout")
            if not current.valid:
                return CleanResult(
                    name,
                    path,
                    "skipped",
                    f"{current.error}; refusing to remove an unverified path",
                )
            if not current.origin_valid:
                return CleanResult(
                    name,
                    path,
                    "skipped",
                    f"{current.error}; refusing to remove an unverified checkout",
                )
            blocker = _blocker(current, stash_count(path))
            if blocker is not None and not force:
                return CleanResult(name, path, "skipped", f"{blocker}; pass --force to remove")
            summary = blocker or "clean checkout"
            if dry_run:
                return CleanResult(name, path, "planned", summary)
            shutil.rmtree(path)
            return CleanResult(name, path, "removed", summary)
    except (CatalogError, GitError, OSError) as error:
        return CleanResult(name, path, "failed", str(error))


def clean_repositories(
    catalog: Catalog,
    repositories: Sequence[Repository],
    *,
    dry_run: bool,
    force: bool,
    jobs: int = 4,
) -> list[CleanResult]:
    if jobs < 1:
        raise ValueError("jobs must be at least one")
    if not repositories:
        return []
    workers = min(jobs, 32, len(repositories))
    results: list[CleanResult] = []
    with ThreadPoolExecutor(max_workers=workers, thread_name_prefix="repomod-clean") as pool:
        futures = {
            pool.submit(clean_one, catalog, repo, dry_run=dry_run, force=force): repo
            for repo in repositories
        }
        for future in as_completed(futures):
            repo = futures[future]
            try:
                results.append(future.result())
            except Exception as error:
                results.append(CleanResult(str(repo.id), None, "failed", str(error)))
    return sorted(results, key=lambda item: item.repository.casefold())
