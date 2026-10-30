from __future__ import annotations

import shutil
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

from repomod.catalog import CatalogError, repository_path
from repomod.git import GitError, expected_origin, origin_url, repository_lock, stash_count, status
from repomod.types import Catalog, GitStatus, RepoId

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


def removal_blocker(current: GitStatus, stashes: int) -> str | None:
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


@dataclass(frozen=True, slots=True)
class GarbagePaths:
    leaves: tuple[Path, ...]
    directories: tuple[Path, ...]


def _lock_id(catalog: Catalog, relative: Path) -> RepoId:
    parts = relative.parts
    if len(parts) >= 2:
        return RepoId(parts[0], parts[1])
    return RepoId(catalog.personal_owner, parts[0])


def unreferenced_paths(catalog: Catalog) -> GarbagePaths:
    checkouts: set[tuple[str, ...]] = set()
    for repo in catalog.repositories:
        try:
            relative = repository_path(catalog, repo).relative_to(catalog.root)
        except (CatalogError, OSError):
            continue
        checkouts.add(relative.parts)
    leaves: list[Path] = []
    directories: list[Path] = []

    def scan(directory: Path, prefix: tuple[str, ...]) -> None:
        try:
            entries = sorted(directory.iterdir(), key=lambda entry: entry.name.casefold())
        except OSError:
            return
        for entry in entries:
            if not prefix and entry.name == ".repomod":
                continue
            relative = (*prefix, entry.name)
            if relative in checkouts:
                continue
            if any(checkout[: len(relative)] == relative for checkout in checkouts):
                if entry.is_dir() and not entry.is_symlink():
                    scan(entry, relative)
                continue
            if entry.is_symlink() or not entry.is_dir() or (entry / ".git").exists():
                leaves.append(entry)
            else:
                scan(entry, relative)
                directories.append(entry)

    scan(catalog.root, ())
    ordered_leaves = sorted(leaves, key=lambda path: path.as_posix().casefold())
    ordered_directories = sorted(
        directories, key=lambda path: (-len(path.parts), path.as_posix().casefold())
    )
    return GarbagePaths(tuple(ordered_leaves), tuple(ordered_directories))


def gc_one(
    catalog: Catalog,
    path: Path,
    *,
    dry_run: bool,
    force: bool,
) -> CleanResult:
    try:
        relative = path.relative_to(catalog.root)
    except ValueError:
        return CleanResult(str(path), None, "failed", "path escapes the workspace root")
    label = relative.as_posix()
    try:
        with repository_lock(catalog.root, _lock_id(catalog, relative)):
            if path.is_symlink():
                return CleanResult(label, path, "skipped", "path is a symlink; refusing to remove")
            if not path.exists():
                return CleanResult(label, path, "absent", "already removed")
            if not path.resolve(strict=False).is_relative_to(catalog.root):
                return CleanResult(label, path, "failed", "path escapes the workspace root")
            if not path.is_dir():
                if not force:
                    return CleanResult(
                        label, path, "skipped", "not a directory; pass --force to remove"
                    )
                if dry_run:
                    return CleanResult(label, path, "planned", "stray file")
                path.unlink()
                return CleanResult(label, path, "removed", "stray file")
            current = status(path, _lock_id(catalog, relative))
            if not current.present:
                return CleanResult(label, path, "absent", "already removed")
            if not current.valid:
                return CleanResult(
                    label,
                    path,
                    "skipped",
                    f"{current.error}; refusing to remove an unverified path",
                )
            remote = origin_url(path)
            if remote is not None:
                for repo in catalog.repositories:
                    if expected_origin(repo.id, remote):
                        return CleanResult(
                            label,
                            path,
                            "skipped",
                            f"origin matches catalogued {repo.id}; move the checkout instead",
                        )
            blocker = removal_blocker(current, stash_count(path))
            if blocker is not None and not force:
                return CleanResult(label, path, "skipped", f"{blocker}; pass --force to remove")
            summary = blocker or "clean checkout"
            if dry_run:
                return CleanResult(label, path, "planned", summary)
            shutil.rmtree(path)
            return CleanResult(label, path, "removed", summary)
    except (CatalogError, GitError, OSError) as error:
        return CleanResult(label, path, "failed", str(error))


def gc_repositories(
    catalog: Catalog,
    *,
    dry_run: bool,
    force: bool,
    jobs: int = 4,
) -> list[CleanResult]:
    if jobs < 1:
        raise ValueError("jobs must be at least one")
    found = unreferenced_paths(catalog)
    results: list[CleanResult] = []
    if found.leaves:
        workers = min(jobs, 32, len(found.leaves))
        with ThreadPoolExecutor(max_workers=workers, thread_name_prefix="repomod-gc") as pool:
            futures = {
                pool.submit(gc_one, catalog, path, dry_run=dry_run, force=force): path
                for path in found.leaves
            }
            for future in as_completed(futures):
                path = futures[future]
                try:
                    results.append(future.result())
                except Exception as error:
                    results.append(CleanResult(str(path), path, "failed", str(error)))
    for directory in found.directories:
        label = directory.relative_to(catalog.root).as_posix()
        try:
            if any(directory.iterdir()):
                continue
        except FileNotFoundError:
            continue
        except OSError as error:
            results.append(CleanResult(label, directory, "failed", str(error)))
            continue
        if dry_run:
            results.append(CleanResult(label, directory, "planned", "empty directory"))
            continue
        try:
            directory.rmdir()
        except OSError as error:
            results.append(CleanResult(label, directory, "failed", str(error)))
        else:
            results.append(CleanResult(label, directory, "removed", "empty directory"))
    return sorted(results, key=lambda item: item.repository.casefold())
