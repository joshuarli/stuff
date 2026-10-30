from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

import repomod.cli as cli
from repomod.cmd import common as cmd_common
from repomod.types import Catalog, RepoId, Repository


def git(*arguments: str, cwd: Path) -> None:
    subprocess.run(["git", *arguments], cwd=cwd, check=True, capture_output=True, text=True)


@pytest.fixture
def fleet(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> tuple[Path, Path]:
    config = tmp_path / "gitconfig"
    config.write_text("[user]\n\tname = Test\n\temail = test@example.invalid\n", encoding="utf-8")
    monkeypatch.setenv("GIT_CONFIG_GLOBAL", str(config))
    monkeypatch.setenv("GIT_CONFIG_NOSYSTEM", "1")
    workspace = tmp_path / "workspace"
    canonical = workspace / "acme" / "tool"
    canonical.mkdir(parents=True)
    git("init", "--initial-branch=main", cwd=canonical)
    git("remote", "add", "origin", "https://github.com/acme/tool.git", cwd=canonical)
    git("commit", "--allow-empty", "-m", "initial", cwd=canonical)
    worktree = tmp_path / "worktree"
    git("worktree", "add", "-b", "fix", str(worktree), cwd=canonical)
    (worktree / "Makefile").write_text("lint:\n\truff check .\n", encoding="utf-8")
    catalog = Catalog(
        tmp_path / "catalog.toml",
        workspace,
        "personal",
        (
            Repository(RepoId("acme", "tool"), ("python",)),
            Repository(RepoId("acme", "other"), ("python",)),
        ),
    )
    monkeypatch.setattr(cmd_common, "_catalog", lambda: catalog)
    return canonical, worktree


def lint_codes(capsys: pytest.CaptureFixture[str], *arguments: str) -> tuple[int, set[str]]:
    code = cli.main(["lint", "--repo", "acme/tool", "--rule", "make-lint-target", "--json", *arguments])
    return code, {record["message"] for record in json.loads(capsys.readouterr().out)}


def test_checkout_override_lints_the_worktree_while_canonical_keeps_its_finding(
    fleet: tuple[Path, Path], capsys: pytest.CaptureFixture[str]
) -> None:
    canonical, worktree = fleet
    canonical_code, canonical_messages = lint_codes(capsys)
    worktree_code, worktree_messages = lint_codes(capsys, "--checkout", str(worktree))

    assert canonical_code == 1
    assert canonical_messages
    assert worktree_code == 0
    assert worktree_messages == set()
    assert not (canonical / "Makefile").exists()


def test_status_override_reports_the_worktree_state(
    fleet: tuple[Path, Path], capsys: pytest.CaptureFixture[str]
) -> None:
    _, worktree = fleet
    cli.main(["status", "--offline", "--json", "--repo", "acme/tool", "--checkout", str(worktree)])
    row = json.loads(capsys.readouterr().out)[0]

    assert row["branch"] == "fix"
    assert row["untracked"] == 1
    assert row["origin_valid"] is True


def test_checkout_requires_exactly_one_repo(
    fleet: tuple[Path, Path], capsys: pytest.CaptureFixture[str]
) -> None:
    _, worktree = fleet

    assert cli.main(["lint", "--checkout", str(worktree)]) == 2
    assert cli.main(["lint", "--repo", "acme/tool", "--repo", "acme/other", "--checkout", str(worktree)]) == 2
    assert "--checkout requires exactly one --repo" in capsys.readouterr().err


def test_checkout_rejects_wrong_origin_non_git_and_symlink(
    fleet: tuple[Path, Path], tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    _, worktree = fleet
    git("remote", "set-url", "origin", "https://github.com/acme/other.git", cwd=worktree)
    plain = tmp_path / "plain"
    plain.mkdir()
    link = tmp_path / "link"
    link.symlink_to(worktree)

    for path, message in (
        (worktree, "origin does not match"),
        (plain, "not a Git work tree"),
        (tmp_path / "missing", "not a directory"),
        (link, "symlink"),
    ):
        assert cli.main(["lint", "--repo", "acme/tool", "--checkout", str(path)]) == 2
        assert message in capsys.readouterr().err
