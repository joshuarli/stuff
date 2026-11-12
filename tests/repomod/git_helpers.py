from __future__ import annotations

import os
import subprocess
from pathlib import Path

from repomod.catalog import Catalog, load_catalog
from repomod.types import RemoteRepository, Repository


def git(*arguments: str, cwd: Path | None = None) -> str:
    result = subprocess.run(
        ["git", *arguments],
        cwd=cwd,
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout.strip()


def make_remote(tmp_path: Path, name: str = "tool") -> tuple[Path, Path, str]:
    bare = tmp_path / f"{name}.git"
    seed = tmp_path / f"{name}-seed"
    git("init", "--bare", "--initial-branch=main", str(bare))
    git("init", "--initial-branch=main", str(seed))
    (seed / "README.md").write_text("first\n", encoding="utf-8")
    git("add", "README.md", cwd=seed)
    git("commit", "-m", "initial", cwd=seed)
    url = f"https://github.com/acme/{name}.git"
    git("remote", "add", "origin", url, cwd=seed)
    global_config = Path(os.environ["GIT_CONFIG_GLOBAL"])
    with global_config.open("a", encoding="utf-8") as config:
        config.write(f'\n[url "file://{bare}"]\n\tinsteadOf = {url}\n')
    git("push", "-u", "origin", "main", cwd=seed)
    return bare, seed, url


def make_catalog(tmp_path: Path) -> tuple[Catalog, Repository]:
    path = tmp_path / "catalog.toml"
    path.write_text(
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "joshuarli"\n\n'
        '[repos."acme"]\ntool = ["tool"]\n',
        encoding="utf-8",
    )
    catalog = load_catalog(
        path,
        {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path / "fleet")},
    )
    return catalog, catalog.repositories[0]


def make_parent(tmp_path: Path) -> tuple[Path, str]:
    bare = tmp_path / "upstream.git"
    seed = tmp_path / "upstream-seed"
    git("init", "--bare", "--initial-branch=main", str(bare))
    git("init", "--initial-branch=main", str(seed))
    (seed / "UPSTREAM.md").write_text("upstream\n", encoding="utf-8")
    git("add", "UPSTREAM.md", cwd=seed)
    git("commit", "-m", "upstream initial", cwd=seed)
    git("push", str(bare), "main", cwd=seed)
    return bare, git("rev-parse", "HEAD", cwd=seed)


def parent_remote(repository: Repository, parent_sha: str, branch: str = "main") -> RemoteRepository:
    return RemoteRepository(
        str(repository.id), "main", "unused", False,
        parent="acme/upstream", parent_branch=branch, parent_sha=parent_sha,
    )


def upstream_refs(checkout: Path) -> list[str]:
    return git("for-each-ref", "--format=%(refname)", "refs/remotes/upstream/", cwd=checkout).splitlines()
