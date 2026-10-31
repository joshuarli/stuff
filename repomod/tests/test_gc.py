from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

import repomod.cli as cli_module
import repomod.cmd.gc as gc_cmd
from repomod.catalog import load_catalog
from repomod.clean import CleanResult, gc_one, gc_repositories, unreferenced_paths
from repomod.cmd import common as cmd_common
from repomod.types import Catalog


def git(*arguments: str, cwd: Path | None = None) -> str:
    result = subprocess.run(
        ["git", *arguments],
        cwd=cwd,
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout.strip()


def make_catalog(tmp_path: Path) -> Catalog:
    path = tmp_path / "catalog.toml"
    path.write_text(
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "joshuarli"\n\n'
        '[repos."acme"]\ntool = ["tool"]\n[repos."joshuarli"]\nmine = ["tool"]\n',
        encoding="utf-8",
    )
    return load_catalog(
        path,
        {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path / "fleet")},
    )


def make_bare(tmp_path: Path, name: str) -> Path:
    bare = tmp_path / f"{name}.git"
    seed = tmp_path / f"{name}-seed"
    git("init", "--bare", "--initial-branch=main", str(bare))
    git("init", "--initial-branch=main", str(seed))
    (seed / "README.md").write_text("first\n", encoding="utf-8")
    git("add", "README.md", cwd=seed)
    git("commit", "-m", "initial", cwd=seed)
    git("push", str(bare), "main:main", cwd=seed)
    return bare


def clone_into(
    bare: Path, destination: Path, *, origin: str | None = None
) -> Path:
    destination.parent.mkdir(parents=True, exist_ok=True)
    git("clone", "--branch", "main", str(bare), str(destination))
    if origin is not None:
        git("remote", "set-url", "origin", origin, cwd=destination)
    return destination


def by_label(results: list[CleanResult]) -> dict[str, CleanResult]:
    return {result.repository: result for result in results}


def test_gc_removes_clean_unreferenced_checkout_and_keeps_catalogued(
    tmp_path: Path, git_environment: None
) -> None:
    catalog = make_catalog(tmp_path)
    bare = make_bare(tmp_path, "seed")
    kept = clone_into(
        bare,
        catalog.root / "acme" / "tool",
        origin="https://github.com/acme/tool.git",
    )
    stale = clone_into(
        bare,
        catalog.root / "stale",
        origin="https://github.com/acme/stale.git",
    )

    results = gc_repositories(catalog, dry_run=False, force=False)

    assert by_label(results)["stale"].action == "removed"
    assert not stale.exists()
    assert kept.is_dir()
    assert (catalog.root / "acme" / "tool").is_dir()


def test_gc_dry_run_plans_without_removing(
    tmp_path: Path, git_environment: None
) -> None:
    catalog = make_catalog(tmp_path)
    bare = make_bare(tmp_path, "seed")
    stale = clone_into(
        bare,
        catalog.root / "stale",
        origin="https://github.com/acme/stale.git",
    )
    empty_dir = catalog.root / "emptydir"
    empty_dir.mkdir(parents=True)

    results = gc_repositories(catalog, dry_run=True, force=False)

    assert by_label(results)["stale"].action == "planned"
    assert by_label(results)["emptydir"].action == "planned"
    assert stale.is_dir()
    assert empty_dir.is_dir()


def test_gc_skips_dirty_checkout_without_force(
    tmp_path: Path, git_environment: None
) -> None:
    catalog = make_catalog(tmp_path)
    bare = make_bare(tmp_path, "seed")
    stale = clone_into(
        bare,
        catalog.root / "stale",
        origin="https://github.com/acme/stale.git",
    )
    (stale / "README.md").write_text("local edit\n", encoding="utf-8")

    skipped = gc_repositories(catalog, dry_run=False, force=False)

    assert "uncommitted changes" in by_label(skipped)["stale"].message
    assert stale.is_dir()

    removed = gc_repositories(catalog, dry_run=False, force=True)

    assert by_label(removed)["stale"].action == "removed"
    assert not stale.exists()


def test_gc_skips_detached_ahead_and_stashed_checkouts(
    tmp_path: Path, git_environment: None
) -> None:
    catalog = make_catalog(tmp_path)
    bare = make_bare(tmp_path, "seed")
    detached = clone_into(bare, catalog.root / "detached")
    git("checkout", "--detach", "HEAD", cwd=detached)
    ahead = clone_into(bare, catalog.root / "ahead")
    (ahead / "ahead.txt").write_text("ahead\n", encoding="utf-8")
    git("add", "ahead.txt", cwd=ahead)
    git("commit", "-m", "ahead", cwd=ahead)
    stashed = clone_into(bare, catalog.root / "stashed")
    (stashed / "README.md").write_text("stash me\n", encoding="utf-8")
    git("stash", "push", "-m", "test", cwd=stashed)

    results = gc_repositories(catalog, dry_run=False, force=False)

    assert by_label(results)["detached"].action == "skipped"
    assert "detached" in by_label(results)["detached"].message
    assert by_label(results)["ahead"].action == "skipped"
    assert "ahead" in by_label(results)["ahead"].message
    assert by_label(results)["stashed"].action == "skipped"
    assert "stash" in by_label(results)["stashed"].message
    assert detached.is_dir()
    assert ahead.is_dir()
    assert stashed.is_dir()


def test_gc_never_removes_checkout_matching_a_catalogued_origin(
    tmp_path: Path, git_environment: None
) -> None:
    catalog = make_catalog(tmp_path)
    bare = make_bare(tmp_path, "seed")
    misplaced = clone_into(
        bare,
        catalog.root / "misplaced",
        origin="https://github.com/acme/tool.git",
    )

    for force in (False, True):
        results = gc_repositories(catalog, dry_run=False, force=force)

        assert by_label(results)["misplaced"].action == "skipped"
        assert "acme/tool" in by_label(results)["misplaced"].message
        assert misplaced.is_dir()


def test_gc_handles_plain_dirs_files_and_symlinks(
    tmp_path: Path, git_environment: None
) -> None:
    catalog = make_catalog(tmp_path)
    bare = make_bare(tmp_path, "seed")
    nested = clone_into(
        bare,
        catalog.root / "oldowner" / "stale",
        origin="https://github.com/oldowner/stale.git",
    )
    stray = catalog.root / "notes.txt"
    stray.write_text("not a checkout\n", encoding="utf-8")
    target = catalog.root / "target.txt"
    target.write_text("target\n", encoding="utf-8")
    link = catalog.root / "link"
    link.symlink_to(target)

    planned = gc_repositories(catalog, dry_run=True, force=False)

    assert by_label(planned)["notes.txt"].action == "skipped"
    assert by_label(planned)["link"].action == "skipped"

    removed = gc_repositories(catalog, dry_run=False, force=True)

    assert by_label(removed)["oldowner/stale"].action == "removed"
    assert by_label(removed)["oldowner"].action == "removed"
    assert not nested.exists()
    assert not (catalog.root / "oldowner").exists()
    assert not stray.exists()
    assert by_label(removed)["link"].action == "skipped"
    assert link.is_symlink()


def test_gc_ignores_repomod_state_and_missing_root(tmp_path: Path) -> None:
    catalog = make_catalog(tmp_path)
    locks = catalog.root / ".repomod" / "locks"
    locks.mkdir(parents=True)

    results = gc_repositories(catalog, dry_run=False, force=False)

    assert results == []
    assert locks.is_dir()


def test_gc_one_rejects_paths_outside_the_workspace_root(
    tmp_path: Path, git_environment: None
) -> None:
    catalog = make_catalog(tmp_path)

    result = gc_one(catalog, tmp_path / "elsewhere", dry_run=False, force=True)

    assert result.action == "failed"
    assert "escapes" in result.message


def test_gc_rejects_invalid_job_counts(tmp_path: Path) -> None:
    catalog = make_catalog(tmp_path)

    with pytest.raises(ValueError, match="jobs must be at least one"):
        gc_repositories(catalog, dry_run=True, force=False, jobs=0)


def test_unreferenced_paths_skips_catalogued_checkouts_and_owner_dirs(
    tmp_path: Path, git_environment: None
) -> None:
    catalog = make_catalog(tmp_path)
    bare = make_bare(tmp_path, "seed")
    clone_into(
        bare, catalog.root / "acme" / "tool", origin="https://github.com/acme/tool.git"
    )
    clone_into(
        bare, catalog.root / "mine", origin="https://github.com/joshuarli/mine.git"
    )
    clone_into(bare, catalog.root / "acme" / "stale")

    found = unreferenced_paths(catalog)

    assert [path.as_posix() for path in found.leaves] == [
        (catalog.root / "acme" / "stale").as_posix()
    ]
    assert found.directories == ()


def test_gc_is_registered_in_cli_parser(
    tmp_path: Path,
    git_environment: None,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    args = cli_module._parser().parse_args(["gc", "--dry-run", "--force"])
    assert args.command == "gc"
    assert args.dry_run is True
    assert args.force is True

    catalog = make_catalog(tmp_path)
    bare = make_bare(tmp_path, "seed")
    clone_into(
        bare,
        catalog.root / "stale",
        origin="https://github.com/acme/stale.git",
    )
    monkeypatch.setattr(cmd_common, "_catalog", lambda: catalog)
    assert gc_cmd._gc(args) == 0
    out = capsys.readouterr().out
    assert "stale: planned:" in out
    assert (catalog.root / "stale").is_dir()
