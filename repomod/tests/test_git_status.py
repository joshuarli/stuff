from __future__ import annotations

from pathlib import Path

import pytest
from git_helpers import git, make_remote

from repomod.cmd.status import _local_color, _local_label, _stale_local_work
from repomod.git import (
    _porcelain_counts,
    branch_checked_out_time,
    expected_origin,
    oldest_unpushed_commit_time,
    status,
)
from repomod.types import GitStatus, RepoId


def test_github_origins_support_https_ssh_and_git_suffix() -> None:
    repo = RepoId("acme", "tool")

    assert expected_origin(repo, "https://github.com/acme/tool")
    assert expected_origin(repo, "https://github.com/ACME/TOOL.git")
    assert expected_origin(repo, "git@github.com:acme/tool.git")
    assert expected_origin(repo, "ssh://git@github.com/acme/tool")
    assert not expected_origin(repo, "https://github.com/acme/other.git")
    assert not expected_origin(repo, "https://github.com.evil.invalid/acme/tool")
    assert not expected_origin(repo, "https://github.com/acme/tool?fork=1")


def test_porcelain_v2_counts_header_and_rename_records() -> None:
    output = (
        "# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0"
        "# branch.ab +2 -3\0"
        "2 .M N... 100644 100644 100644 abc def R100 new name\0old name\0"
        "1 A. N... 000000 100644 100644 . abc staged\0"
        "? untracked\0"
    )

    assert _porcelain_counts(output) == (1, 1, 1)


def test_status_reads_remote_tracking_counts_from_nul_headers(
    tmp_path: Path,
    git_environment: None,
) -> None:
    _, seed, url = make_remote(tmp_path)
    checkout = tmp_path / "checkout"
    git("clone", "--branch", "main", url, str(checkout))
    (checkout / "ahead.txt").write_text("ahead\n", encoding="utf-8")
    git("add", "ahead.txt", cwd=checkout)
    git("commit", "-m", "ahead", cwd=checkout)
    (seed / "behind.txt").write_text("behind\n", encoding="utf-8")
    git("add", "behind.txt", cwd=seed)
    git("commit", "-m", "behind", cwd=seed)
    git("push", "origin", "main", cwd=seed)
    git("fetch", "origin", cwd=checkout)

    state = status(checkout, RepoId("acme", "tool"))

    assert state.origin_valid
    assert state.branch == "main"
    assert state.upstream == "origin/main"
    assert state.ahead == 1
    assert state.behind == 1

def test_oldest_unpushed_commit_time_uses_commit_dates(
    tmp_path: Path,
    git_environment: None,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _, _, url = make_remote(tmp_path)
    checkout = tmp_path / "checkout"
    git("clone", "--branch", "main", url, str(checkout))
    assert oldest_unpushed_commit_time(checkout) is None
    for name, stamp in (("old", "1700000000"), ("new", "1700500000")):
        (checkout / f"{name}.txt").write_text(name, encoding="utf-8")
        git("add", f"{name}.txt", cwd=checkout)
        monkeypatch.setenv("GIT_COMMITTER_DATE", f"{stamp} +0000")
        git("commit", "-m", name, cwd=checkout)

    assert oldest_unpushed_commit_time(checkout) == 1700000000


def test_branch_checked_out_time_reads_latest_switch_onto_branch(
    tmp_path: Path,
    git_environment: None,
) -> None:
    _, _, url = make_remote(tmp_path)
    checkout = tmp_path / "checkout"
    git("clone", "--branch", "main", url, str(checkout))
    assert branch_checked_out_time(checkout, "main") is None
    git("checkout", "-b", "repomod/fix/example", cwd=checkout)
    reflog = checkout / ".git" / "logs" / "HEAD"
    lines = reflog.read_text(encoding="utf-8").splitlines()
    identity, _, rest = lines[-1].partition("> ")
    _, _, tail = rest.partition(" ")
    lines[-1] = f"{identity}> 1700000000 {tail}"
    reflog.write_text("\n".join(lines) + "\n", encoding="utf-8")

    assert branch_checked_out_time(checkout, "repomod/fix/example") == 1700000000
    assert branch_checked_out_time(checkout, "other") is None


def test_local_label_shows_ages_and_flags_stale_work(tmp_path: Path) -> None:
    value = GitStatus(
        "acme/tool", tmp_path, True, True, "repomod/fix/example", False, 0, 0, 0, 2, 0,
        "origin/repomod/fix/example", True,
    )

    label = _local_label(value, "main", branch_days=9, unpushed_days=3)

    assert label == "ahead 2 (last fetch), oldest unpushed 3d on repomod/fix/example for 9d"
    assert _stale_local_work(9, 3)
    assert not _stale_local_work(6, None)
    assert _local_color(label, stale=True) == "31"
