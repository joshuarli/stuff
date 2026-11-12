from __future__ import annotations

from pathlib import Path

from git_helpers import git, make_remote

from repomod.git import (
    _porcelain_counts,
    expected_origin,
    status,
)
from repomod.types import RepoId


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
