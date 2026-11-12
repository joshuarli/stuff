from __future__ import annotations

import os
import subprocess
from pathlib import Path

from repomod.catalog import Catalog, load_catalog
from repomod.types import Finding, Repository, Rule, TagPredicate


def git(
    *arguments: str, cwd: Path | None = None, check: bool = True
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *arguments],
        cwd=cwd,
        capture_output=True,
        text=True,
        check=check,
    )


def initialize_repository(
    tmp_path: Path, *, push: bool = True
) -> tuple[Catalog, Repository, Path, str]:
    root = tmp_path / "fleet"
    root.mkdir()
    path = root / "demo"
    git("init", "--initial-branch=main", str(path))
    (path / "README.md").write_text("base\n", encoding="utf-8")
    git("add", "README.md", cwd=path)
    git("commit", "-m", "base", cwd=path)
    base = git("rev-parse", "HEAD", cwd=path).stdout.strip()
    catalog_path = tmp_path / "catalog.toml"
    catalog_path.write_text(
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n\n'
        '[repos."acme"]\ndemo = ["tool"]\n',
        encoding="utf-8",
    )
    catalog = load_catalog(
        catalog_path, {"HOME": str(tmp_path), "REPOMOD_ROOT": str(root)}
    )
    repository = catalog.repositories[0]
    remote_url = "https://github.com/acme/demo.git"
    if push:
        bare = tmp_path / "remote.git"
        git("init", "--bare", "--initial-branch=main", str(bare))
        global_config = Path(os.environ["GIT_CONFIG_GLOBAL"])
        with global_config.open("a", encoding="utf-8") as config:
            config.write(f'\n[url "file://{bare}"]\n\tinsteadOf = {remote_url}\n')
        git("remote", "add", "origin", remote_url, cwd=path)
        git("push", "-u", "origin", "main", cwd=path)
    else:
        git("remote", "add", "origin", remote_url, cwd=path)
    return catalog, repository, path, base


def file_rule(
    rule_id: str, filename: str, value: str, *, validate=None, on_fix=None
) -> Rule:
    def check(root: Path, repository: Repository) -> list[Finding]:
        path = root / filename
        if path.is_file() and path.read_text(encoding="utf-8") == value:
            return []
        return [
            Finding(
                str(repository.id),
                rule_id,
                filename,
                "file needs its policy content",
                fixable=True,
            )
        ]

    def fix(root: Path, repository: Repository) -> dict[str, str]:
        if on_fix is not None:
            on_fix(root, repository)
        return {filename: value}

    return Rule(
        rule_id,
        f"Repair {filename}",
        TagPredicate(),
        check,
        fix,
        f"chore(repomod): apply {rule_id} policy",
        (filename,),
        validate,
    )


def subjects(path: Path, base: str, target: str = "HEAD") -> list[str]:
    output = git("log", "--format=%s", f"{base}..{target}", cwd=path).stdout
    return output.splitlines()
