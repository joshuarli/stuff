"""An interrupt during fix or validation leaves a pending row unfinished."""

from pathlib import Path
from typing import Any, cast

import pytest

from repomod.main import run_fix_and_validate
from repomod.status import atomic_write_yaml, load_status
from repomod.types import FixConfig, Repo, RunContext
from repomod.ui import ProgressDisplay
from repomod.utils import ShutdownError, interrupt


class _Fix:
    def __init__(self, *, fail: bool) -> None:
        self._fail = fail

    def accept_repo(self, repo: dict[str, Any], config: dict[str, Any]) -> bool:
        return True

    def run(self, repo: Path, config: dict[str, Any]) -> bool:
        if self._fail:
            raise RuntimeError("fix failed")
        return True


class _Display:
    def __init__(self) -> None:
        self.finished: list[str] = []

    def set_phase(self, repo_name: str, phase: str) -> None:
        return

    def finish(self, repo_name: str, msg: str = "", category: str = "done") -> None:
        self.finished.append(repo_name)


def _pending_context(
    tmp_path: Path, fix: _Fix, display: _Display
) -> tuple[Repo, RunContext, dict[str, str]]:
    repo = Repo("acme/backend", "main", ["rust"])
    status_file = tmp_path / "status.yaml"
    logs_dir = tmp_path / "logs"
    logs_dir.mkdir()
    atomic_write_yaml(
        {
            repo.name: {
                "status": "pending",
                "fix_hash": "reviewed-hash",
                "base_sha": "abc123",
            }
        },
        status_file,
    )
    all_status = load_status(status_file)
    row = dict(all_status[repo.name])
    ctx = RunContext(
        fix=fix,
        config=FixConfig(branch="ci-fixes", pr_title="t", pr_body="p"),
        config_dict={},
        fix_hash="current-hash",
        accepted_hashes=frozenset({"current-hash"}),
        repos_dir=tmp_path / "repos",
        logs_dir=logs_dir,
        patches_dir=tmp_path / "patches",
        status_file=status_file,
        apply=False,
        all_status=all_status,
        pd=cast("ProgressDisplay", display),
        existing_prs={},
    )
    return repo, ctx, row


def _run_while_interrupted(
    repo: Repo, ctx: RunContext, row: dict[str, str], display: _Display
) -> None:
    interrupt.event.clear()
    caught: BaseException | None = None
    try:
        interrupt.event.set()
        try:
            run_fix_and_validate(repo, ctx.repos_dir / repo.slug, ctx)
        except ShutdownError as exc:
            caught = exc
    finally:
        interrupt.event.clear()
    assert isinstance(caught, ShutdownError), (
        f"row={ctx.all_status[repo.name]!r} finish={display.finished!r}"
    )
    assert ctx.all_status[repo.name] == row
    assert display.finished == []


def test_interrupted_fix_keeps_the_pending_row(tmp_path: Path) -> None:
    display = _Display()
    repo, ctx, row = _pending_context(tmp_path, _Fix(fail=True), display)
    _run_while_interrupted(repo, ctx, row, display)


def _validation_crashes(
    _fix: object, _repo: Path, _config: dict[str, Any]
) -> list[str]:
    raise RuntimeError("lint")


def test_interrupted_validation_keeps_the_pending_row(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    display = _Display()
    repo, ctx, row = _pending_context(tmp_path, _Fix(fail=False), display)
    monkeypatch.setattr("repomod.main.stage_worktree", lambda _repo: None)
    monkeypatch.setattr("repomod.main.has_changes", lambda _repo: True)
    monkeypatch.setattr("repomod.main.validate_changes", _validation_crashes)
    _run_while_interrupted(repo, ctx, row, display)
