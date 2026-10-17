"""A fix branch is a ref, never a git option."""

import json
import subprocess
from pathlib import Path
from typing import Any
from unittest.mock import patch

import pytest
from gh_harness import FakeGh, branch_pushed
from test_apply_gh_calls import _setup
from test_main import REPO_A, _mock_sync

from repomod import main as run_fix


def _record_git(commands: list[list[str]]) -> Any:
    original = subprocess.run

    def side_effect(cmd: Any, **kwargs: Any) -> subprocess.CompletedProcess[Any]:
        argv = [str(part) for part in cmd]
        if argv and argv[0] == "git":
            commands.append(argv)
        return original(cmd, **kwargs)

    return patch("repomod.git.subprocess.run", side_effect=side_effect)


def _option_branch(tmp_path: Path) -> tuple[Path, Path, str]:
    marker = tmp_path / "marker"
    wrapper = tmp_path / "upload-pack"
    wrapper.write_text(f"#!/bin/sh\nprintf ran > {json.dumps(str(marker))}\n")
    wrapper.chmod(0o755)
    return marker, wrapper, f"--upload-pack={wrapper}"


def test_option_shaped_fix_branch_is_rejected_before_push(
    tmp_path: Path, fake_gh: FakeGh, capsys: pytest.CaptureFixture[str]
) -> None:
    """`--upload-pack=<wrapper>` must not run, and the run fails before push."""
    marker, _wrapper, branch = _option_branch(tmp_path)
    fix_dir, origins = _setup(tmp_path)
    config = fix_dir / "config.yaml"
    config.write_text(
        config.read_text().replace(
            "branch: test-branch\n", f"branch: {json.dumps(branch)}\n", 1
        )
    )
    commands: list[list[str]] = []

    with _mock_sync(repos=[REPO_A]), _record_git(commands):
        code = run_fix.main(
            [str(fix_dir), "--apply", "--workers", "1", "--repos", REPO_A.name]
        )

    captured = capsys.readouterr()
    assert code != 0
    assert branch in captured.err
    assert not marker.exists()
    listed = subprocess.run(
        ["git", "branch", "--list", "--", branch],
        cwd=origins[REPO_A.name],
        capture_output=True,
        text=True,
        check=False,
    )
    assert listed.returncode == 0
    assert not listed.stdout.strip()
    assert fake_gh.count("pr", "create") == 0
    for cmd in commands:
        if branch not in cmd:
            continue
        assert cmd[:4] == ["git", "check-ref-format", "--branch", branch]


def test_normal_branch_is_fetched_checked_out_and_pushed(
    tmp_path: Path, fake_gh: FakeGh
) -> None:
    """`test-branch` is still fetched, checked out, and force-pushed."""
    fix_dir, origins = _setup(tmp_path)
    commands: list[list[str]] = []

    with _mock_sync(repos=[REPO_A]), _record_git(commands):
        assert (
            run_fix.main(
                [str(fix_dir), "--apply", "--workers", "1", "--repos", REPO_A.name]
            )
            == 0
        )

    assert ["git", "fetch", "--depth=1", "origin", "--", "main"] in commands
    assert ["git", "switch", "--discard-changes", "--", "main"] in commands
    assert ["git", "switch", "--", "test-branch"] in commands
    assert [
        "git",
        "push",
        "--set-upstream",
        "--force",
        "--quiet",
        "origin",
        "--",
        "test-branch",
    ] in commands
    bare = origins[REPO_A.name]
    assert branch_pushed(bare, "test-branch")
    pushed = subprocess.run(
        ["git", "show", "test-branch:README.md"],
        cwd=bare,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    assert pushed == "changed"
    assert fake_gh.count("pr", "create", repo=REPO_A.name) == 1
