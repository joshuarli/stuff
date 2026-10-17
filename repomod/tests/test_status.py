"""Tests for YAML serialization contracts."""

import os
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any

import pytest
import yaml

from repomod.status import (
    atomic_write_yaml,
    load_status,
    patch_stat,
    print_rollout,
    print_summary,
    set_status,
)
from repomod.types import FixConfig, RolloutPR


def test_dump_then_load_repos_yaml(tmp_path: Path) -> None:
    repos = [
        {
            "name": "acme/backend",
            "default_branch": "main",
            "topics": ["rust"],
        },
        {
            "name": "acme/platform",
            "default_branch": "master",
            "topics": [],
        },
    ]
    p = tmp_path / "repos.yaml"
    atomic_write_yaml(repos, p)
    with open(p) as f:
        loaded = yaml.safe_load(f)
    assert loaded == repos


def test_dump_yaml_is_valid_yaml(tmp_path: Path) -> None:
    repos = [
        {
            "name": "acme/frontend",
            "default_branch": "main",
            "topics": ["rust"],
        }
    ]
    p = tmp_path / "repos.yaml"
    atomic_write_yaml(repos, p)
    with open(p) as f:
        parsed = yaml.safe_load(f)
    assert parsed[0]["name"] == "acme/frontend"
    assert parsed[0]["topics"] == ["rust"]


def test_load_config_yaml(tmp_path: Path) -> None:
    p = tmp_path / "config.yaml"
    p.write_text(
        "branch: pin-gha-actions\n"
        'pr_title: "chore: pin actions"\n'
        "pr_body: |\n"
        "  ## Summary\n"
        "\n"
        "  - Pin all actions.\n"
        "reviewers:\n"
        "  - reviewer-one\n"
        "  - reviewer-two\n"
        "sparse_paths:\n"
        "  - .github\n"
        "exclude:\n"
        "  - acme/internal-tool\n"
    )
    config = FixConfig.from_yaml(p)

    assert config.branch == "pin-gha-actions"
    assert config.pr_title == "chore: pin actions"
    assert "## Summary" in config.pr_body
    assert "- Pin all actions." in config.pr_body
    assert config.reviewers == ["reviewer-one", "reviewer-two"]
    assert config.sparse_paths == [".github"]
    assert config.extra["exclude"] == ["acme/internal-tool"]


def test_atomic_write_yaml_does_not_follow_a_tmp_symlink(tmp_path: Path) -> None:
    """A planted temp-name symlink must not receive the new document."""
    fix_dir = tmp_path / "ubuntu-24"
    fix_dir.mkdir()
    outside = tmp_path / "outside" / "token.yaml"
    outside.parent.mkdir()
    outside.write_text("original\n")
    dest = fix_dir / "status.yaml"
    # with_suffix(".tmp") is status.tmp; the predictable alias is status.yaml.tmp.
    for planted in (dest.with_suffix(".tmp"), fix_dir / "status.yaml.tmp"):
        planted.symlink_to(outside)
    document = {"acme/backend": {"status": "pending", "fix_hash": "abc123"}}

    atomic_write_yaml(document, dest)

    assert outside.read_text() == "original\n"
    assert not dest.is_symlink()
    assert yaml.safe_load(dest.read_text()) == document


def test_atomic_write_yaml_fsyncs_the_parent_directory_after_replace(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A crash after rename must not lose the new directory entry."""
    dest = tmp_path / "repos.yaml"
    document = {"acme/backend": {"status": "pending", "fix_hash": "abc123"}}
    events: list[tuple[str, Path | None]] = []
    fd_paths: dict[int, Path] = {}
    real_open = os.open
    real_fsync = os.fsync
    real_replace = os.replace

    def tracking_open(
        path: str | os.PathLike[str], flags: int, *args: Any, **kwargs: Any
    ) -> int:
        fd = real_open(path, flags, *args, **kwargs)
        fd_paths[fd] = Path(path)
        return fd

    def tracking_fsync(fd: int) -> None:
        events.append(("fsync", fd_paths.get(fd)))
        real_fsync(fd)

    def tracking_replace(
        src: str | os.PathLike[str], dst: str | os.PathLike[str]
    ) -> None:
        events.append(("replace", Path(dst)))
        real_replace(src, dst)

    monkeypatch.setattr(os, "open", tracking_open)
    monkeypatch.setattr(os, "fsync", tracking_fsync)
    monkeypatch.setattr(os, "replace", tracking_replace)

    atomic_write_yaml(document, dest)

    parent = dest.parent.resolve()
    replace_at = next(i for i, event in enumerate(events) if event[0] == "replace")
    parent_fsyncs = [
        i
        for i, event in enumerate(events)
        if event[0] == "fsync" and event[1] is not None and event[1].resolve() == parent
    ]
    assert parent_fsyncs, events
    assert parent_fsyncs[0] > replace_at
    assert yaml.safe_load(dest.read_text()) == document


def test_set_status_keeps_a_disk_row_missing_from_memory(tmp_path: Path) -> None:
    """A stale in-memory dict must not publish over another process's row."""
    fix_dir = tmp_path / "ubuntu-24"
    fix_dir.mkdir()
    status_file = fix_dir / "status.yaml"
    frontend = {"status": "pending", "fix_hash": "abc123"}
    atomic_write_yaml({"acme/frontend": frontend}, status_file)
    memory: dict[str, dict[str, str]] = {}

    set_status(
        memory,
        status_file,
        "acme/backend",
        status="skipped",
        reason="no changes",
    )

    loaded = load_status(status_file)
    assert loaded["acme/frontend"] == frontend
    assert loaded["acme/backend"] == {"status": "skipped", "reason": "no changes"}
    assert memory == loaded


_LOCK_HOLDER = """
import fcntl
import os
import sys
import time
from pathlib import Path

lock_path, ready, release = sys.argv[1:]
fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o644)
try:
    fcntl.flock(fd, fcntl.LOCK_EX)
    Path(ready).write_text("held")
    deadline = time.monotonic() + 15
    while not Path(release).exists():
        if time.monotonic() > deadline:
            raise SystemExit("timed out waiting to release the generation lock")
        time.sleep(0.01)
finally:
    fcntl.flock(fd, fcntl.LOCK_UN)
    os.close(fd)
"""


def test_set_status_waits_for_another_process_generation_lock(tmp_path: Path) -> None:
    """set_status blocks while another process holds the generations lock."""
    fix_dir = tmp_path / "ubuntu-24"
    fix_dir.mkdir()
    status_file = fix_dir / "status.yaml"
    lock_path = fix_dir / "generations" / ".lock"
    lock_path.parent.mkdir()
    ready = tmp_path / "held"
    release = tmp_path / "release"
    holder = subprocess.Popen(
        [sys.executable, "-c", _LOCK_HOLDER, str(lock_path), str(ready), str(release)]
    )
    errors: list[BaseException] = []
    memory: dict[str, dict[str, str]] = {}

    def write() -> None:
        try:
            set_status(
                memory,
                status_file,
                "acme/backend",
                status="skipped",
                reason="no changes",
            )
        except BaseException as exc:
            errors.append(exc)

    try:
        deadline = time.monotonic() + 5
        while not ready.exists():
            if holder.poll() is not None:
                raise AssertionError(f"lock holder exited {holder.returncode}")
            if time.monotonic() > deadline:
                raise AssertionError("lock holder did not acquire the lock")
            time.sleep(0.01)
        worker = threading.Thread(target=write)
        worker.start()
        worker.join(0.4)
        assert worker.is_alive(), errors
        release.write_text("go")
        worker.join(5)
    finally:
        release.write_text("go")
        try:
            holder.wait(timeout=5)
        except subprocess.TimeoutExpired:
            holder.terminate()
            holder.wait(timeout=5)

    assert errors == []
    assert not worker.is_alive()
    assert holder.returncode == 0
    assert memory["acme/backend"] == {"status": "skipped", "reason": "no changes"}
    assert load_status(status_file)["acme/backend"] == memory["acme/backend"]


def test_load_status_missing_file(tmp_path: Path) -> None:
    assert load_status(tmp_path / "nope.yaml") == {}


def test_status_roundtrip(tmp_path: Path) -> None:
    original = {
        "acme/backend": {
            "status": "pr_created",
            "pr_url": "https://github.com/acme/backend/pull/456",
        },
        "acme/frontend": {
            "status": "failed",
            "reason": "clone failed",
        },
        "acme/worker": {
            "status": "skipped",
            "reason": "no changes",
        },
    }
    p = tmp_path / "status.yaml"
    atomic_write_yaml(original, p)
    loaded = load_status(p)
    assert loaded == original


def test_status_preserves_insertion_order(tmp_path: Path) -> None:
    original = {
        "acme/frontend": {"status": "pending"},
        "acme/backend": {"status": "skipped", "reason": "no changes"},
        "acme/worker": {
            "status": "pr_created",
            "pr_url": "https://github.com/acme/worker/pull/1",
        },
    }
    p = tmp_path / "status.yaml"
    atomic_write_yaml(original, p)
    loaded = load_status(p)
    assert list(loaded.keys()) == [
        "acme/frontend",
        "acme/backend",
        "acme/worker",
    ]


def test_status_multiline_error_roundtrip(tmp_path: Path) -> None:
    tb = (
        "Traceback (most recent call last):\n"
        '  File "main.py", line 10\n'
        "KeyError: 'branch'"
    )
    original = {"acme/cli-tools": {"status": "failed", "error": tb}}
    p = tmp_path / "status.yaml"
    atomic_write_yaml(original, p)
    loaded = load_status(p)
    assert loaded["acme/cli-tools"]["error"] == tb


def test_print_rollout_lists_open_and_closed_but_only_counts_merged(
    capsys: Any,
) -> None:
    prs = [
        RolloutPR(
            "acme/backend", "https://github.com/acme/backend/pull/1", "OPEN", True
        ),
        RolloutPR(
            "acme/frontend",
            "https://github.com/acme/frontend/pull/2",
            "MERGED",
            False,
        ),
        RolloutPR(
            "acme/worker",
            "https://github.com/acme/worker/pull/3",
            "CLOSED",
            False,
        ),
    ]

    print_rollout("macos-15-runners", prs)

    out = capsys.readouterr().out
    assert "1 merged" in out
    assert "1 open" in out
    assert "1 closed" in out
    assert "acme/backend" in out
    assert "(draft)" in out
    assert "acme/worker" in out
    assert "acme/frontend" not in out


def test_pr_spec_requests_no_reviewers_when_the_fix_names_none() -> None:
    config = FixConfig(branch="b", pr_title="t", pr_body="p")
    assert config.pr_spec("acme/backend").reviewers == []


def test_pr_spec_requests_exactly_the_fixes_reviewers() -> None:
    config = FixConfig(
        branch="b", pr_title="t", pr_body="p", reviewers=["reviewer-one"]
    )
    assert config.pr_spec("acme/backend").reviewers == ["reviewer-one"]


# The single space on the blank context line is significant, so spell it out.
REALISTIC_PATCH = "\n".join(
    [
        "diff --git a/.github/workflows/ci.yml b/.github/workflows/ci.yml",
        "index 6fd4f4e..468f5a3 100644",
        "--- a/.github/workflows/ci.yml",
        "+++ b/.github/workflows/ci.yml",
        "@@ -23,7 +23,7 @@ jobs:",
        "     strategy:",
        "       matrix:",
        "-        os: [ubuntu-22.04, ubuntu-24.04]",
        "+        os: [ubuntu-24.04]",
        " ",
        "-    runs-on: ubuntu-22.04",
        "+    runs-on: ubuntu-24.04",
        "diff --git a/.github/workflows/release.yml b/.github/workflows/release.yml",
        "--- a/.github/workflows/release.yml",
        "+++ b/.github/workflows/release.yml",
        "@@ -1 +1 @@",
        "-    runs-on: ubuntu-20.04",
        "+    runs-on: ubuntu-24.04",
        "",
    ]
)


def test_patch_stat_counts_files_and_changed_lines_not_headers() -> None:
    assert patch_stat(REALISTIC_PATCH) == (2, 3, 3)


def test_print_summary_lists_each_pending_repo_with_its_diffstat(
    tmp_path: Path, capsys: Any
) -> None:
    fix_dir = tmp_path / "ubuntu-24"
    patches = fix_dir / "patches-abc123"
    patches.mkdir(parents=True)
    (patches / "acme-backend.patch").write_text(REALISTIC_PATCH)
    all_status = {
        "acme/backend": {"status": "pending", "fix_hash": "abc123"},
        "acme/legacy-tool": {"status": "pending", "fix_hash": "old999"},
        "acme/website": {"status": "skipped", "fix_hash": "abc123"},
    }

    print_summary(all_status, fix_dir=fix_dir)

    out = capsys.readouterr().out
    assert "2 pending" in out
    assert "acme/backend  2 files" in out
    assert "+3" in out
    assert "-3" in out
    # A pending repo whose patch predates patch storage is flagged, not hidden.
    assert "acme/legacy-tool  (no stored patch)" in out
    assert "acme/website" not in out
    assert "repomod ubuntu-24 --diffs" in out
