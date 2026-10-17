"""Integration tests for repomod.main pipeline."""

import os
import signal
import subprocess
import tempfile
import threading
import time
from collections.abc import Callable, Generator
from contextlib import ExitStack, contextmanager
from pathlib import Path
from types import FrameType
from typing import Any, cast
from unittest.mock import patch

import pytest
import yaml
from gh_harness import FakeGh

from repomod import git
from repomod import main as run_fix
from repomod.main import compute_fix_hash, filter_work
from repomod.status import atomic_write_yaml, load_status, stored_patch
from repomod.types import (
    FixConfig,
    FixModule,
    LintFinding,
    PRInfo,
    Repo,
    RolloutPR,
    SyncResult,
    WorkItem,
)
from repomod.ui import ProgressDisplay
from repomod.utils import ShutdownError, interrupt


def _call_handler(handler: object) -> None:
    cast("Callable[[int, FrameType | None], object]", handler)(signal.SIGINT, None)


REPO_TEST = Repo(
    name="acme/test-repo",
    default_branch="main",
    topics=["rust"],
)
REPO_A = Repo(name="acme/repo-alpha", default_branch="main", topics=["rust"])
REPO_B = Repo(name="acme/repo-beta", default_branch="main", topics=["rust"])
REPO_C = Repo(name="acme/repo-gamma", default_branch="main", topics=["rust"])

# Default: single repo for simple tests. Multi-repo tests pass explicit lists.
DEFAULT_REPOS: list[Repo] = [REPO_TEST]


class _TestFix:
    def __init__(self, accept_repo: Any) -> None:
        self._accept_repo = accept_repo

    def accept_repo(self, repo: dict[str, Any], config: dict[str, Any]) -> bool:
        return bool(self._accept_repo(repo, config))

    def run(self, repo: Path, config: dict[str, Any]) -> bool:
        msg = "test helper _TestFix.run() should not be called"
        raise AssertionError(msg)


def _make_fix_module(accept_repo: Any) -> FixModule:
    return _TestFix(accept_repo)


CONFIG_YAML = """\
branch: test-branch
pr_title: "test: a fix"
pr_body: |
  Test body.
reviewers:
  - testuser
"""

FIX_MAIN = """\
from pathlib import Path

def accept_repo(repo: dict, config: dict) -> bool:
    return {accept}

def run(repo: Path, config: dict) -> bool:
{body}
"""


@contextmanager
def _mock_sync(repos: list[Repo] | None = None) -> Generator[None]:
    """Mock sync_repos and redirect REPOS_FILE to a temp path."""
    with tempfile.TemporaryDirectory() as td:
        tmp_repos = Path(td) / "repos.yaml"
        with (
            patch.object(
                run_fix,
                "sync_repos",
                return_value=SyncResult(repos or DEFAULT_REPOS, []),
            ),
            patch.object(run_fix, "REPOS_FILE", tmp_repos),
        ):
            yield


def _write_fix(
    fix_dir: Path,
    accept: str = "True",
    body: str = "    (repo / 'README.md').write_text('changed')\n    return True",
) -> None:
    """Write a minimal fix module and config into fix_dir."""
    fix_dir.mkdir(parents=True, exist_ok=True)
    (fix_dir / "config.yaml").write_text(CONFIG_YAML)
    (fix_dir / "main.py").write_text(FIX_MAIN.format(accept=accept, body=body))


def _make_git_repo(dest: Path) -> None:
    """Create a real local git repo to act as a 'clone'."""
    dest.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["git", "init", "--initial-branch=main"],
        cwd=dest,
        capture_output=True,
        check=True,
    )
    subprocess.run(
        ["git", "config", "user.email", "test@test.com"],
        cwd=dest,
        capture_output=True,
        check=True,
    )
    subprocess.run(
        ["git", "config", "user.name", "Test"],
        cwd=dest,
        capture_output=True,
        check=True,
    )
    (dest / "README.md").write_text("init\n")
    subprocess.run(["git", "add", "."], cwd=dest, capture_output=True, check=True)
    subprocess.run(
        ["git", "commit", "-m", "init", "--quiet"],
        cwd=dest,
        capture_output=True,
        check=True,
    )


def _mock_clone_git() -> Any:
    """Stub the clone reset. A real `git clean -fd` would delete fixture files."""
    original_run = subprocess.run

    def side_effect(cmd: Any, **kwargs: Any) -> subprocess.CompletedProcess[Any]:
        cmd_str = " ".join(str(c) for c in cmd)
        if any(
            x in cmd_str
            for x in (
                "git clone",
                "git fetch",
                "git reset --hard",
                "git branch --list --format",
                "git checkout --force",
                "git remote set-url",
                "git clean",
            )
        ):
            return subprocess.CompletedProcess(cmd, 0, stdout="main\n", stderr="")
        return original_run(cmd, **kwargs)

    return patch("repomod.git.subprocess.run", side_effect=side_effect)


def _write_status(fix_dir: Path, data: dict[str, Any]) -> None:
    atomic_write_yaml(data, fix_dir / "status.yaml")


def test_load_fix_success(tmp_path: Path) -> None:
    fix_dir = tmp_path / "myfix"
    _write_fix(fix_dir)
    mod, config, fix_hash = run_fix.load_fix(fix_dir)

    assert config.branch == "test-branch"
    assert config.pr_title == "test: a fix"
    assert isinstance(fix_hash, str)
    assert len(fix_hash) == 12
    assert hasattr(mod, "accept_repo")
    assert hasattr(mod, "run")


def test_load_fix_missing_config(tmp_path: Path) -> None:
    fix_dir = tmp_path / "noconfig"
    fix_dir.mkdir()
    (fix_dir / "main.py").write_text("def accept_repo(r, c): ...\ndef run(r, c): ...\n")

    with pytest.raises(SystemExit):
        run_fix.load_fix(fix_dir)


def test_load_fix_missing_main(tmp_path: Path) -> None:
    fix_dir = tmp_path / "nomain"
    fix_dir.mkdir()
    (fix_dir / "config.yaml").write_text("branch: x\npr_title: x\npr_body: |\n  x\n")

    with pytest.raises(SystemExit):
        run_fix.load_fix(fix_dir)


def test_load_fix_missing_required_func(tmp_path: Path) -> None:
    fix_dir = tmp_path / "badfuncs"
    fix_dir.mkdir()
    (fix_dir / "config.yaml").write_text("branch: x\npr_title: x\npr_body: |\n  x\n")
    (fix_dir / "main.py").write_text("def accept_repo(r, c): ...\n")

    with pytest.raises(SystemExit):
        run_fix.load_fix(fix_dir)


def test_fix_hash_deterministic(tmp_path: Path) -> None:
    _write_fix(tmp_path / "fix")
    h1 = compute_fix_hash(tmp_path / "fix")
    h2 = compute_fix_hash(tmp_path / "fix")
    assert h1 == h2
    assert len(h1) == 12


def test_fix_hash_changes_on_edit(tmp_path: Path) -> None:
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir)
    h1 = compute_fix_hash(fix_dir)
    (fix_dir / "main.py").write_text("def accept_repo(r, c): ...\ndef run(r, c): ...\n")
    h2 = compute_fix_hash(fix_dir)
    assert h1 != h2


def test_filter_excludes_skip_list() -> None:
    config = FixConfig(
        branch="b",
        pr_title="t",
        pr_body="p",
        skip=["acme/repo-beta"],
    )
    mod = _make_fix_module(lambda r, c: True)
    work, _already_done = filter_work(
        [REPO_A, REPO_B, REPO_C], {}, config, mod, "hash", None
    )
    names = [w.repo.name for w in work]
    assert "acme/repo-beta" not in names
    assert "acme/repo-alpha" in names
    assert "acme/repo-gamma" in names


def test_filter_excludes_up_to_date() -> None:
    config = FixConfig(branch="b", pr_title="t", pr_body="p")
    mod = _make_fix_module(lambda r, c: True)
    status = {
        "acme/repo-alpha": {"status": "pr_created", "fix_hash": "current"},
    }
    work, already_done = filter_work(
        [REPO_A, REPO_B], status, config, mod, "current", None
    )
    names = [w.repo.name for w in work]
    assert "acme/repo-alpha" not in names
    assert "acme/repo-beta" in names
    assert already_done == 1


def test_filter_skips_pending_and_skipped_with_matching_hash() -> None:
    """pending and skipped repos are also skipped when fix hash matches."""
    config = FixConfig(branch="b", pr_title="t", pr_body="p")
    mod = _make_fix_module(lambda r, c: True)
    status = {
        "acme/repo-alpha": {"status": "pending", "fix_hash": "current"},
        "acme/repo-beta": {"status": "skipped", "fix_hash": "current"},
        "acme/repo-gamma": {"status": "failed", "fix_hash": "current"},
    }
    work, already_done = filter_work(
        [REPO_A, REPO_B, REPO_C],
        status,
        config,
        mod,
        "current",
        None,
    )
    names = [w.repo.name for w in work]
    # pending and skipped are done, failed is retried
    assert "acme/repo-alpha" not in names
    assert "acme/repo-beta" not in names
    assert "acme/repo-gamma" in names
    assert already_done == 2


def test_filter_pending_reprocessed_in_apply_mode() -> None:
    """In --apply mode, pending repos (dry-run saw changes) must be re-processed."""
    config = FixConfig(branch="b", pr_title="t", pr_body="p")
    mod = _make_fix_module(lambda r, c: True)
    status = {
        "acme/repo-alpha": {"status": "pending", "fix_hash": "current"},
        "acme/repo-beta": {"status": "skipped", "fix_hash": "current"},
        "acme/repo-gamma": {"status": "pr_created", "fix_hash": "current"},
    }
    work, already_done = filter_work(
        [REPO_A, REPO_B, REPO_C],
        status,
        config,
        mod,
        "current",
        None,
        apply=True,
    )
    names = [w.repo.name for w in work]
    # pending should be re-processed (needs PR), skipped and pr_created are done
    assert "acme/repo-alpha" in names
    assert "acme/repo-beta" not in names
    assert "acme/repo-gamma" not in names
    assert already_done == 2


def test_filter_includes_stale_hash() -> None:
    config = FixConfig(branch="b", pr_title="t", pr_body="p")
    mod = _make_fix_module(lambda r, c: True)
    status = {
        "acme/repo-alpha": {"status": "pr_created", "fix_hash": "old"},
    }
    work, _already_done = filter_work([REPO_A], status, config, mod, "new", None)
    assert len(work) == 1
    assert work[0].stale_reason == "fix changed"


def test_filter_respects_only_repos() -> None:
    config = FixConfig(branch="b", pr_title="t", pr_body="p")
    mod = _make_fix_module(lambda r, c: True)
    work, _already_done = filter_work(
        [REPO_A, REPO_B, REPO_C],
        {},
        config,
        mod,
        "hash",
        only_repos={"acme/repo-beta"},
    )
    assert len(work) == 1
    assert work[0].repo.name == "acme/repo-beta"


def test_filter_excludes_rejected_by_accept_repo() -> None:
    config = FixConfig(branch="b", pr_title="t", pr_body="p")
    mod = _make_fix_module(lambda r, c: r["name"] != "acme/repo-beta")
    work, _already_done = filter_work(
        [REPO_A, REPO_B, REPO_C], {}, config, mod, "hash", None
    )
    names = [w.repo.name for w in work]
    assert "acme/repo-beta" not in names


def test_e2e_dry_run_shows_diff(tmp_path: Path, capsys: Any) -> None:
    """Dry-run shows diff box, doesn't write status."""
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir)
    _make_git_repo(fix_dir / "repos" / "acme-test-repo")

    with _mock_sync(), _mock_clone_git():
        run_fix.main([str(fix_dir)])

    captured = capsys.readouterr()
    assert "acme/test-repo" in captured.out
    assert "README.md" in captured.out
    assert "+changed" in captured.out
    # Dry-run writes status=pending so ^C knows what completed
    status = load_status(fix_dir / "status.yaml")
    assert status["acme/test-repo"]["status"] == "pending"
    assert "interrupted" not in str(status)


def test_e2e_dry_run_no_changes(tmp_path: Path, capsys: Any) -> None:
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir, body="    return False")
    _make_git_repo(fix_dir / "repos" / "acme-test-repo")

    with _mock_sync(), _mock_clone_git():
        run_fix.main([str(fix_dir)])

    captured = capsys.readouterr()
    assert "no changes" in captured.out
    if (fix_dir / "status.yaml").exists():
        content = (fix_dir / "status.yaml").read_text()
        assert "interrupted" not in content, (
            "completed repos must not be marked interrupted"
        )


def test_e2e_dry_run_multiple_repos(tmp_path: Path, capsys: Any) -> None:
    """Multiple repos in dry-run: each gets processed, none marked interrupted."""
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir, body="    return False")
    for repo in [REPO_A, REPO_B, REPO_C]:
        _make_git_repo(fix_dir / "repos" / repo.slug)

    with _mock_sync(repos=[REPO_A, REPO_B, REPO_C]), _mock_clone_git():
        run_fix.main([str(fix_dir)])

    captured = capsys.readouterr()
    assert "no changes" in captured.out
    if (fix_dir / "status.yaml").exists():
        status = load_status(fix_dir / "status.yaml")
        for name in ["acme/repo-alpha", "acme/repo-beta", "acme/repo-gamma"]:
            assert status.get(name, {}).get("status") != "interrupted"


def test_e2e_skip_filtered_repo(tmp_path: Path, capsys: Any) -> None:
    """Repos rejected by accept_repo are silently excluded."""
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir, accept="False")

    with _mock_sync():
        run_fix.main([str(fix_dir)])

    captured = capsys.readouterr()
    assert "No repos to process" in captured.out


def test_e2e_skip_list(tmp_path: Path, capsys: Any) -> None:
    """Repos in the skip list are silently excluded."""
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir)
    (fix_dir / "config.yaml").write_text(CONFIG_YAML + "skip:\n  - acme/test-repo\n")

    with _mock_sync():
        run_fix.main([str(fix_dir)])

    captured = capsys.readouterr()
    assert "No repos to process" in captured.out


def test_e2e_resume_skips_pr_created(tmp_path: Path, capsys: Any) -> None:
    """Repos with status=pr_created and matching hash are skipped."""
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir)
    fix_hash = compute_fix_hash(fix_dir)

    _write_status(
        fix_dir,
        {
            "acme/test-repo": {
                "status": "pr_created",
                "pr_url": "https://github.com/x/1",
                "fix_hash": fix_hash,
            }
        },
    )

    with _mock_sync():
        run_fix.main([str(fix_dir)])

    captured = capsys.readouterr()
    assert "test-repo" not in captured.out or "No repos" in captured.out

    status = load_status(fix_dir / "status.yaml")
    assert status["acme/test-repo"]["status"] == "pr_created"


def test_e2e_stale_hash_reruns(tmp_path: Path, capsys: Any) -> None:
    """Repos with pr_created but stale fix_hash get re-processed."""
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir)
    _make_git_repo(fix_dir / "repos" / "acme-test-repo")

    _write_status(
        fix_dir,
        {
            "acme/test-repo": {
                "status": "pr_created",
                "pr_url": "https://github.com/x/1",
                "fix_hash": "old_stale_hash",
            }
        },
    )

    with _mock_sync(), _mock_clone_git():
        run_fix.main([str(fix_dir)])

    captured = capsys.readouterr()
    assert "acme/test-repo" in captured.out
    # Dry-run updates status with new hash (so ^C tracking works)
    status = load_status(fix_dir / "status.yaml")
    assert status["acme/test-repo"]["status"] == "pending"
    assert status["acme/test-repo"]["fix_hash"] == compute_fix_hash(fix_dir)


def test_e2e_fix_failure_writes_status_and_log(tmp_path: Path) -> None:
    """Fix exception: status=failed with reason, traceback in log file."""
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir, body="    raise RuntimeError('boom')")
    _make_git_repo(fix_dir / "repos" / "acme-test-repo")

    with _mock_sync(), _mock_clone_git():
        run_fix.main([str(fix_dir)])

    status = load_status(fix_dir / "status.yaml")
    assert status["acme/test-repo"]["status"] == "failed"
    assert status["acme/test-repo"]["reason"] == "fix failed"

    fix_hash = compute_fix_hash(fix_dir)
    log_path = fix_dir / f"logs-{fix_hash}" / "acme-test-repo.log"
    assert log_path.exists()
    log_content = log_path.read_text()
    assert "RuntimeError" in log_content
    assert "boom" in log_content


def test_e2e_failure_does_not_taint_other_repos(tmp_path: Path) -> None:
    """One repo failing doesn't affect other repos' status."""
    fix_dir = tmp_path / "fix"
    # repo-a succeeds (no changes), repo-b fails
    _write_fix(
        fix_dir,
        body="""\
    if (repo / '.fail').exists():
        raise RuntimeError('fail')
    return False
""",
    )
    _make_git_repo(fix_dir / "repos" / REPO_A.slug)
    _make_git_repo(fix_dir / "repos" / REPO_B.slug)
    (fix_dir / "repos" / REPO_B.slug / ".fail").write_text("")

    with _mock_sync(repos=[REPO_A, REPO_B]), _mock_clone_git():
        run_fix.main([str(fix_dir)])

    status = load_status(fix_dir / "status.yaml")
    # repo-b failed
    assert status["acme/repo-beta"]["status"] == "failed"
    # repo-a must NOT be failed or interrupted
    assert status.get("acme/repo-alpha", {}).get("status") != "failed"
    assert status.get("acme/repo-alpha", {}).get("status") != "interrupted"


def test_sync_command_writes_repos_yaml_without_running_a_fix(
    capsys: Any,
) -> None:
    """`repomod sync` refreshes repos.yaml and never touches a fix."""
    with tempfile.TemporaryDirectory() as td:
        tmp_repos = Path(td) / "repos.yaml"
        with (
            patch.object(
                run_fix,
                "sync_repos",
                return_value=SyncResult([REPO_A, REPO_B], ["acme/legacy-tool"]),
            ),
            patch.object(run_fix, "REPOS_FILE", tmp_repos),
            patch.object(run_fix, "load_fix") as load_fix,
        ):
            assert run_fix.main(["sync"]) == 0

        saved = [r["name"] for r in yaml.safe_load(tmp_repos.read_text())]
        excluded = yaml.safe_load((Path(td) / "repos-excluded.yaml").read_text())

    assert saved == ["acme/repo-alpha", "acme/repo-beta"]
    assert excluded == ["acme/legacy-tool"]
    load_fix.assert_not_called()
    out = capsys.readouterr().out
    assert "Synced 2 repos" in out
    assert "1 repos excluded as inactive" in out


def test_e2e_new_workflow_errors_fail_the_repo_before_diff(
    tmp_path: Path, capsys: Any
) -> None:
    """A fix that introduces workflow errors is failed, never shown or pushed."""
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir)
    _make_git_repo(fix_dir / "repos" / "acme-test-repo")
    finding = LintFinding(".github/workflows/ci.yml", 'undefined variable "foo"')

    with (
        _mock_sync(),
        _mock_clone_git(),
        patch("repomod.lint.new_workflow_findings", return_value={finding}),
    ):
        run_fix.main([str(fix_dir)])

    status = load_status(fix_dir / "status.yaml")
    assert status["acme/test-repo"]["status"] == "failed"
    assert status["acme/test-repo"]["reason"] == "validation: 1 error(s)"

    log_path = fix_dir / f"logs-{compute_fix_hash(fix_dir)}" / "acme-test-repo.log"
    assert 'ci.yml: undefined variable "foo"' in log_path.read_text()
    assert "README.md" not in capsys.readouterr().out


def test_e2e_dry_run_stores_patch_and_base_sha(tmp_path: Path) -> None:
    """The proposed diff is saved per fix hash, with the commit it applies to."""
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir)
    repo_dir = fix_dir / "repos" / "acme-test-repo"
    _make_git_repo(repo_dir)
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=repo_dir,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()

    with _mock_sync(), _mock_clone_git():
        run_fix.main([str(fix_dir)])

    patch_path = (
        fix_dir / f"patches-{compute_fix_hash(fix_dir)}" / "acme-test-repo.patch"
    )
    assert "+changed" in patch_path.read_text()
    status = load_status(fix_dir / "status.yaml")
    assert status["acme/test-repo"]["base_sha"] == head


def test_e2e_no_changes_stores_no_patch(tmp_path: Path) -> None:
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir, body="    return False")
    _make_git_repo(fix_dir / "repos" / "acme-test-repo")

    with _mock_sync(), _mock_clone_git():
        run_fix.main([str(fix_dir)])

    patches_dir = fix_dir / f"patches-{compute_fix_hash(fix_dir)}"
    assert list(patches_dir.iterdir()) == []


def test_e2e_apply_records_already_merged_pr_without_calling_it_created(
    tmp_path: Path,
) -> None:
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir)
    _make_git_repo(fix_dir / "repos" / "acme-test-repo")
    url = "https://github.com/acme/test-repo/pull/7"

    with (
        _mock_sync(),
        _mock_clone_git(),
        patch.object(run_fix, "commit_and_push", return_value=PRInfo(url, "MERGED")),
    ):
        run_fix.main([str(fix_dir), "--apply"])

    entry = load_status(fix_dir / "status.yaml")["acme/test-repo"]
    assert entry["status"] == "pr_merged"
    assert entry["pr_url"] == url


def test_filter_merged_and_closed_prs_are_done() -> None:
    config = FixConfig(branch="b", pr_title="t", pr_body="p")
    mod = _make_fix_module(lambda r, c: True)
    status = {
        "acme/repo-alpha": {"status": "pr_merged", "fix_hash": "h1"},
        "acme/repo-beta": {"status": "pr_closed", "fix_hash": "h1"},
    }
    work, done = filter_work(
        [REPO_A, REPO_B], status, config, mod, "h1", None, apply=True
    )
    assert work == []
    assert done == 2


def test_prs_flag_reports_rollout_without_running_the_fix(
    tmp_path: Path, capsys: Any
) -> None:
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir)
    prs = [
        RolloutPR(
            "acme/repo-alpha",
            "https://github.com/acme/repo-alpha/pull/4",
            "OPEN",
            False,
        )
    ]

    with (
        patch.object(run_fix, "search_prs", return_value=prs) as search,
        patch.object(run_fix, "load_fix") as load_fix,
    ):
        assert run_fix.main([str(fix_dir), "--prs"]) == 0

    search.assert_called_once_with("test-branch")
    load_fix.assert_not_called()
    assert "acme/repo-alpha" in capsys.readouterr().out


FIX_WITH_VALIDATE = (
    FIX_MAIN
    + """
def validate(repo: Path, config: dict) -> list[str]:
{validate_body}
"""
)


def _write_fix_with_validate(fix_dir: Path, validate_body: str) -> None:
    fix_dir.mkdir(parents=True, exist_ok=True)
    (fix_dir / "config.yaml").write_text(CONFIG_YAML)
    (fix_dir / "main.py").write_text(
        FIX_WITH_VALIDATE.format(
            accept="True",
            body="    (repo / 'README.md').write_text('changed')\n    return True",
            validate_body=validate_body,
        )
    )


def test_e2e_fix_defined_validation_errors_fail_the_repo(tmp_path: Path) -> None:
    fix_dir = tmp_path / "fix"
    _write_fix_with_validate(fix_dir, "    return ['README.md: must not say changed']")
    _make_git_repo(fix_dir / "repos" / "acme-test-repo")

    with _mock_sync(), _mock_clone_git():
        run_fix.main([str(fix_dir)])

    status = load_status(fix_dir / "status.yaml")
    assert status["acme/test-repo"]["status"] == "failed"
    assert status["acme/test-repo"]["reason"] == "validation: 1 error(s)"
    log_path = fix_dir / f"logs-{compute_fix_hash(fix_dir)}" / "acme-test-repo.log"
    assert "README.md: must not say changed" in log_path.read_text()


def test_e2e_passing_fix_validation_lets_the_diff_through(tmp_path: Path) -> None:
    fix_dir = tmp_path / "fix"
    _write_fix_with_validate(fix_dir, "    return []")
    _make_git_repo(fix_dir / "repos" / "acme-test-repo")

    with _mock_sync(), _mock_clone_git():
        run_fix.main([str(fix_dir)])

    status = load_status(fix_dir / "status.yaml")
    assert status["acme/test-repo"]["status"] == "pending"


def test_e2e_crashing_validate_fails_the_repo_with_a_traceback(tmp_path: Path) -> None:
    fix_dir = tmp_path / "fix"
    _write_fix_with_validate(fix_dir, "    raise RuntimeError('validator bug')")
    _make_git_repo(fix_dir / "repos" / "acme-test-repo")

    with _mock_sync(), _mock_clone_git():
        run_fix.main([str(fix_dir)])

    status = load_status(fix_dir / "status.yaml")
    assert status["acme/test-repo"]["reason"] == "validation crashed"
    log_path = fix_dir / f"logs-{compute_fix_hash(fix_dir)}" / "acme-test-repo.log"
    assert "validator bug" in log_path.read_text()


def _dry_run_two_repos(tmp_path: Path) -> Path:
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir)
    _make_git_repo(fix_dir / "repos" / REPO_A.slug)
    _make_git_repo(fix_dir / "repos" / REPO_B.slug)
    with _mock_sync(repos=[REPO_B, REPO_A]), _mock_clone_git():
        run_fix.main([str(fix_dir), "--workers", "1"])
    return fix_dir


def test_dry_run_ends_with_each_pending_repos_diffstat(
    tmp_path: Path, capsys: Any
) -> None:
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir)
    _make_git_repo(fix_dir / "repos" / REPO_A.slug)

    with _mock_sync(repos=[REPO_A]), _mock_clone_git():
        run_fix.main([str(fix_dir)])

    out = capsys.readouterr().out
    summary = out[out.index("Summary:") :]
    assert "1 pending" in summary
    assert "acme/repo-alpha  1 file" in summary
    assert "--diffs" in summary


def test_diffs_pretty_prints_every_pending_repo_in_order_without_rerunning(
    tmp_path: Path, capsys: Any
) -> None:
    fix_dir = _dry_run_two_repos(tmp_path)
    capsys.readouterr()

    with (
        patch.object(run_fix, "sync_repos") as sync,
        patch.object(run_fix, "load_fix") as load_fix,
    ):
        assert run_fix.main([str(fix_dir), "--diffs"]) == 0

    out = capsys.readouterr().out
    assert out.index("acme/repo-alpha") < out.index("acme/repo-beta")
    assert out.count("-init") == 2
    assert out.count("+changed") == 2
    assert "2 repos would change" in out
    sync.assert_not_called()
    load_fix.assert_not_called()


def test_diffs_can_be_narrowed_to_chosen_repos_and_a_limit(
    tmp_path: Path, capsys: Any
) -> None:
    fix_dir = _dry_run_two_repos(tmp_path)
    capsys.readouterr()

    run_fix.main([str(fix_dir), "--diffs", "--repos", "acme/repo-beta"])
    only_beta = capsys.readouterr().out
    assert "acme/repo-beta" in only_beta
    assert "acme/repo-alpha" not in only_beta

    run_fix.main([str(fix_dir), "--diffs", "--limit", "1"])
    limited = capsys.readouterr().out
    assert "1 repos would change" in limited


def test_diffs_with_nothing_pending_says_so(tmp_path: Path, capsys: Any) -> None:
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir)

    assert run_fix.main([str(fix_dir), "--diffs"]) == 0

    assert "No pending dry-run changes" in capsys.readouterr().out


def _repos_file_aged(path: Path, hours: float) -> None:
    old = time.time() - hours * 3600
    os.utime(path, (old, old))


def test_a_day_old_repos_file_is_used_as_the_cache_and_says_how_old_it_is(
    tmp_path: Path, capsys: Any
) -> None:
    repos_file = tmp_path / "repos.yaml"
    atomic_write_yaml([REPO_A.to_record()], repos_file)
    _repos_file_aged(repos_file, hours=20)

    with (
        patch.object(run_fix, "REPOS_FILE", repos_file),
        patch.object(run_fix, "sync_repos") as sync,
    ):
        repos = run_fix.sync_if_needed(force=False)

    assert repos == [REPO_A]
    sync.assert_not_called()
    out = capsys.readouterr().out
    assert "cached repos.yaml (1 repos, 20h old" in out
    assert "--refresh" in out


def test_a_stale_file_triggers_a_sync_that_inherits_its_activity_memo(
    tmp_path: Path,
) -> None:
    """Too old to use as a cache, but its memo spares re-fetching history."""
    repos_file = tmp_path / "repos.yaml"
    topics = ["rust"]
    remembered = Repo("acme/repo-alpha", "main", topics, "2026-09-01T00:00:00Z")
    atomic_write_yaml([remembered.to_record()], repos_file)
    _repos_file_aged(repos_file, hours=30)
    fresh = Repo("acme/repo-alpha", "main", topics, "2026-09-30T00:00:00Z")

    with (
        patch.object(run_fix, "REPOS_FILE", repos_file),
        patch.object(
            run_fix, "sync_repos", return_value=SyncResult([fresh], [])
        ) as sync,
    ):
        repos = run_fix.sync_if_needed(force=False)

    (known,) = sync.call_args.args
    assert known == {"acme/repo-alpha": remembered}
    assert repos == [fresh]
    written = yaml.safe_load(repos_file.read_text())
    assert written[0]["last_human_commit"] == "2026-09-30T00:00:00Z"


def test_fixes_still_see_only_the_stable_repo_facts_not_the_sync_memo() -> None:
    repo = Repo("acme/backend", "main", ["rust"], "2026-09-01T00:00:00Z")

    assert repo.to_dict() == {
        "name": "acme/backend",
        "default_branch": "main",
        "topics": ["rust"],
    }
    assert Repo.from_dict(repo.to_record()) == repo


def test_sigint_handler_returns_while_progress_lock_is_held(tmp_path: Path) -> None:
    """The installed handler must return while the caller holds the progress lock.

    One worker runs the fix on the signal thread, and print holds that
    non-reentrant lock. A handler that prints never returns, so the handler
    captured at startup is never restored and a second ^C does nothing.
    """
    repo = Repo("acme/backend", "main", ["rust"])
    _fix_dir, ctx = _no_review_context(tmp_path, apply=False)
    prints: list[str] = []
    original_print = ProgressDisplay.print

    def spy(self: ProgressDisplay, text: str) -> None:
        prints.append(text)
        original_print(self, text)

    class _Probe:
        def __init__(self) -> None:
            self.started = False
            self.blocked = True
            self.event = False
            self.graceful = 1
            self.joined = False
            self.installed = False

        def accept_repo(self, repo: dict[str, Any], config: dict[str, Any]) -> bool:
            return True

        def run(self, repo: Path, config: dict[str, Any]) -> bool:
            handler = signal.getsignal(signal.SIGINT)
            self.installed = callable(handler)
            if not callable(handler):
                raise ShutdownError
            finished = threading.Event()
            started = threading.Event()

            def invoke() -> None:
                started.set()
                try:
                    _call_handler(handler)
                finally:
                    finished.set()

            worker = threading.Thread(target=invoke, daemon=True)
            ctx.pd._lock.acquire()
            try:
                worker.start()
                self.started = started.wait(1)
                if self.started:
                    self.blocked = not finished.wait(0.3)
                    self.graceful = sum(
                        "Gracefully exiting" in text for text in tuple(prints)
                    )
                    self.event = interrupt.is_set
            finally:
                ctx.pd._lock.release()
                worker.join(timeout=1)
                self.joined = not worker.is_alive()
            raise ShutdownError

    probe = _Probe()
    ctx.fix = probe
    previous = signal.getsignal(signal.SIGINT)
    interrupt.event.clear()
    try:
        with (
            patch.object(run_fix, "clone"),
            patch.object(ProgressDisplay, "print", spy),
            patch("os.kill") as kill,
        ):
            assert run_fix.run_work(ctx, [WorkItem(repo)], 1) is True
            assert not interrupt.is_set
            assert signal.getsignal(signal.SIGINT) is previous
            assert kill.call_count == 0
    finally:
        signal.signal(signal.SIGINT, previous)
        interrupt.event.clear()

    assert probe.installed
    assert probe.started
    assert probe.joined
    assert probe.event
    assert probe.graceful == 0
    assert not probe.blocked


def test_workers_one_interrupt_prints_the_graceful_line_from_the_loop(
    tmp_path: Path, capsys: Any
) -> None:
    """The shutdown line is printed when the loop notices, not by the handler.

    The fix sets the event itself and raises. Nothing delivers SIGINT, so the
    line is not from the handler. One repo means the loop notices only after
    that fix returns.
    """
    repo = Repo("acme/backend", "main", ["rust"])
    fix_dir, ctx = _no_review_context(tmp_path, apply=False)

    class _Stop:
        def accept_repo(self, repo: dict[str, Any], config: dict[str, Any]) -> bool:
            return True

        def run(self, repo: Path, config: dict[str, Any]) -> bool:
            interrupt.event.set()
            raise ShutdownError

    ctx.fix = _Stop()
    previous = signal.getsignal(signal.SIGINT)
    interrupt.event.clear()
    try:
        with patch.object(run_fix, "clone"):
            assert run_fix.run_work(ctx, [WorkItem(repo)], 1) is True
        assert not interrupt.is_set
        assert signal.getsignal(signal.SIGINT) is previous
    finally:
        signal.signal(signal.SIGINT, previous)
        interrupt.event.clear()

    assert load_status(fix_dir / "status.yaml")["acme/backend"] == {
        "status": "interrupted",
    }
    assert capsys.readouterr().out.count("Gracefully exiting, please wait...") == 1


def test_ctrl_c_during_apply_keeps_earlier_results_instead_of_marking_interrupted(
    tmp_path: Path, capsys: Any
) -> None:
    """A dry run's saved diff (and an existing PR) survive an interrupted re-run.

    The first repo's fix triggers ^C. alpha completes before it is noticed; beta
    (a pending dry-run diff) and gamma (an existing PR) were queued behind it and
    must keep their results rather than be downgraded to interrupted. The fix
    sets the event itself, so the graceful line comes from the worker loop.
    """
    fix_dir = tmp_path / "fix"
    _write_fix(
        fix_dir,
        body=(
            "    from repomod.utils import interrupt\n"
            "    interrupt.event.set()\n"
            "    (repo / 'README.md').write_text('changed')\n"
            "    return True"
        ),
    )
    for repo in (REPO_A, REPO_B, REPO_C):
        _make_git_repo(fix_dir / "repos" / repo.slug)
    earlier = {
        "acme/repo-beta": {
            "status": "pending",
            "fix_hash": "olderhash",
            "base_sha": "abc123",
        },
        "acme/repo-gamma": {
            "status": "pr_created",
            "fix_hash": "olderhash",
            "pr_url": "https://github.com/acme/repo-gamma/pull/9",
        },
    }
    atomic_write_yaml(earlier, fix_dir / "status.yaml")

    with _mock_sync(repos=[REPO_A, REPO_B, REPO_C]), _mock_clone_git():
        assert run_fix.main([str(fix_dir), "--workers", "1"]) == 130

    status = load_status(fix_dir / "status.yaml")
    assert status["acme/repo-alpha"]["status"] == "pending"
    assert status["acme/repo-beta"] == earlier["acme/repo-beta"]
    assert status["acme/repo-gamma"] == earlier["acme/repo-gamma"]
    assert capsys.readouterr().out.count("Gracefully exiting, please wait...") == 1


def test_ctrl_c_during_apply_marks_the_in_flight_repo_interrupted_and_retries_it(
    tmp_path: Path, fake_gh: FakeGh, capsys: Any
) -> None:
    """--apply must stop before push when the in-flight fix raises ShutdownError.

    Alpha is not finished and is not pushed. Beta's pending row and gamma's PR
    stay as they were. The next run, with the interrupt cleared and the fix no
    longer raising, runs the fix on alpha again. The fix sets the event itself,
    so the graceful line comes from the worker loop.
    """
    fix_dir = tmp_path / "fix"
    gate = tmp_path / "raise-shutdown"
    gate.write_text("yes")
    ran = tmp_path / "fix-ran"
    _write_fix(
        fix_dir,
        body=(
            "    from repomod.utils import ShutdownError, interrupt\n"
            "    (repo / 'README.md').write_text('changed')\n"
            f"    if Path({str(gate)!r}).exists():\n"
            "        interrupt.event.set()\n"
            "        raise ShutdownError\n"
            "    if repo.name == 'acme-repo-alpha':\n"
            f"        Path({str(ran)!r}).write_text('acme/repo-alpha\\n')\n"
            "    return True"
        ),
    )
    for repo in (REPO_A, REPO_B, REPO_C):
        _make_git_repo(fix_dir / "repos" / repo.slug)
    atomic_write_yaml(
        {
            "acme/repo-beta": {
                "status": "pending",
                "fix_hash": "olderhash",
                "base_sha": "abc123",
            },
            "acme/repo-gamma": {
                "status": "pr_created",
                "fix_hash": "olderhash",
                "pr_url": "https://github.com/acme/repo-gamma/pull/9",
            },
        },
        fix_dir / "status.yaml",
    )
    alpha = fix_dir / "repos" / REPO_A.slug
    finished: list[str] = []
    commands: list[list[str]] = []
    real_finish = ProgressDisplay.finish

    def spy_finish(
        self: ProgressDisplay,
        repo_name: str,
        msg: str = "",
        category: str = "done",
    ) -> None:
        finished.append(repo_name)
        real_finish(self, repo_name, msg, category)

    interrupt.event.clear()
    try:
        with (
            _mock_sync(repos=[REPO_A, REPO_B, REPO_C]),
            _mock_clone_git() as git_run,
            patch.object(ProgressDisplay, "finish", spy_finish),
        ):
            record = git_run.side_effect

            def recording(
                cmd: Any, **kwargs: Any
            ) -> subprocess.CompletedProcess[Any]:
                commands.append([str(part) for part in cmd])
                return record(cmd, **kwargs)

            git_run.side_effect = recording
            assert run_fix.main([str(fix_dir), "--apply", "--workers", "1"]) == 130

        status = load_status(fix_dir / "status.yaml")
        assert status["acme/repo-alpha"] == {"status": "interrupted"}
        assert status["acme/repo-beta"] == {
            "status": "pending",
            "fix_hash": "olderhash",
            "base_sha": "abc123",
        }
        assert status["acme/repo-gamma"] == {
            "status": "pr_created",
            "fix_hash": "olderhash",
            "pr_url": "https://github.com/acme/repo-gamma/pull/9",
        }
        assert finished == []
        assert [argv for argv in commands if "push" in argv] == []
        assert fake_gh.count("pr", "create") == 0
        assert not ran.exists()
        head = subprocess.run(
            ["git", "log", "--format=%s", "HEAD"],
            cwd=alpha,
            capture_output=True,
            text=True,
            check=True,
        )
        assert head.stdout.splitlines() == ["init"]
        branch = subprocess.run(
            ["git", "log", "--format=%s", "test-branch"],
            cwd=alpha,
            capture_output=True,
            text=True,
        )
        commits = branch.stdout.splitlines() if branch.returncode == 0 else []
        assert commits in ([], ["init"])
        assert capsys.readouterr().out.count("Gracefully exiting, please wait...") == 1

        gate.unlink()
        interrupt.event.clear()
        with (
            _mock_sync(repos=[REPO_A, REPO_B, REPO_C]),
            _mock_clone_git(),
            patch.object(
                run_fix,
                "commit_and_push",
                return_value=PRInfo(
                    "https://github.com/acme/repo-alpha/pull/4", "OPEN"
                ),
            ),
        ):
            assert run_fix.main([str(fix_dir), "--apply", "--workers", "1"]) == 0
        assert ran.read_text() == "acme/repo-alpha\n"
    finally:
        interrupt.event.clear()


def test_interrupted_commit_keeps_the_pending_row_and_does_not_finish(
    tmp_path: Path,
) -> None:
    """CalledProcessError while ^C is set must not replace a dry-run pending row.

    set_status would drop the reviewed hash and base_sha, and pd.finish would
    mark the repo done so the sweep never treats it as interrupted.
    """
    repo = Repo("acme/backend", "main", ["rust"])
    pending = {
        "status": "pending",
        "fix_hash": "reviewed-hash",
        "base_sha": "abc123",
    }
    status_file = tmp_path / "status.yaml"
    logs_dir = tmp_path / "logs"
    logs_dir.mkdir()
    atomic_write_yaml({"acme/backend": pending}, status_file)
    all_status = load_status(status_file)
    pd = ProgressDisplay(total=1, workers=1)
    ctx = run_fix.RunContext(
        fix=_make_fix_module(lambda _repo, _config: True),
        config=FixConfig(branch="test-branch", pr_title="t", pr_body="p"),
        config_dict={},
        fix_hash="current-hash",
        accepted_hashes=frozenset({"current-hash"}),
        repos_dir=tmp_path / "repos",
        logs_dir=logs_dir,
        patches_dir=tmp_path / "patches",
        status_file=status_file,
        apply=True,
        all_status=all_status,
        pd=pd,
        existing_prs={},
    )
    interrupt.event.clear()

    def killed(*_args: object, **_kwargs: object) -> None:
        interrupt.event.set()
        raise subprocess.CalledProcessError(-2, ["git", "commit"])

    try:
        with (
            patch.object(run_fix, "clone"),
            patch.object(run_fix, "run_fix_and_validate", return_value=True),
            patch.object(run_fix, "get_diff", return_value="diff\n"),
            patch.object(run_fix, "head_sha", return_value="abc123"),
            patch.object(run_fix, "commit_and_push", side_effect=killed) as commit,
        ):
            run_fix.process_repo(WorkItem(repo), ctx)
    finally:
        interrupt.event.clear()

    commit.assert_called_once()
    assert all_status["acme/backend"] == pending
    assert load_status(status_file)["acme/backend"] == pending
    assert not pd.is_completed("acme/backend")
    assert pd.counts == {"done": 0, "failed": 0}


class _TreeEdit:
    def __init__(self, edit: Any) -> None:
        self._edit = edit

    def accept_repo(self, repo: dict[str, Any], config: dict[str, Any]) -> bool:
        return True

    def run(self, repo: Path, config: dict[str, Any]) -> bool:
        self._edit(repo)
        return True


def _review_context(
    tmp_path: Path, edit: Any
) -> tuple[Repo, run_fix.RunContext, Path]:
    repo = Repo("acme/backend", "main", ["rust"])
    repo_dir = tmp_path / "repos" / repo.slug
    _make_git_repo(repo_dir)
    logs_dir = tmp_path / "logs"
    logs_dir.mkdir()
    ctx = run_fix.RunContext(
        fix=_TreeEdit(edit),
        config=FixConfig(
            branch="ci-fixes", pr_title="ci: ship the change", pr_body="p"
        ),
        config_dict={},
        fix_hash="reviewedhash",
        accepted_hashes=frozenset({"reviewedhash"}),
        repos_dir=tmp_path / "repos",
        logs_dir=logs_dir,
        patches_dir=tmp_path / "patches-reviewedhash",
        status_file=tmp_path / "status.yaml",
        apply=True,
        all_status={},
        pd=ProgressDisplay(total=1, workers=1),
        existing_prs={},
    )
    return repo, ctx, repo_dir


def _run_reviewed(
    repo: Repo,
    ctx: run_fix.RunContext,
    seen: dict[str, Any],
    commands: list[list[str]],
    *,
    actionlint: Any = None,
) -> None:
    original = subprocess.run

    def record(cmd: Any, **kwargs: Any) -> subprocess.CompletedProcess[Any]:
        commands.append([str(part) for part in cmd])
        return original(cmd, **kwargs)

    def spy_has(repo_dir: Path) -> bool:
        seen["has_changes"] = git.has_changes(repo_dir)
        seen["diff"] = git.get_diff(repo_dir)
        return bool(seen["has_changes"])

    def spy_files(repo_dir: Path) -> list[str]:
        files = git.changed_files(repo_dir)
        seen["files"] = files
        return files

    with ExitStack() as stack:
        stack.enter_context(patch.object(run_fix, "clone"))
        stack.enter_context(
            patch.object(
                git,
                "push_and_open_pr",
                return_value=PRInfo("https://github.com/acme/backend/pull/7", "OPEN"),
            )
        )
        stack.enter_context(patch.object(run_fix, "has_changes", side_effect=spy_has))
        stack.enter_context(patch("repomod.lint.changed_files", side_effect=spy_files))
        stack.enter_context(patch("repomod.git.subprocess.run", side_effect=record))
        if actionlint is not None:
            stack.enter_context(
                patch("repomod.lint.run_actionlint", side_effect=actionlint)
            )
        run_fix.execute_fix_on_repo(repo, ctx)


def _commit_patch(repo_dir: Path) -> str:
    return subprocess.run(
        ["git", "show", "--format=", "--no-color", "HEAD"],
        cwd=repo_dir,
        check=True,
        capture_output=True,
        text=True,
    ).stdout


def _index_commands(commands: list[list[str]]) -> list[str]:
    kinds: list[str] = []
    for cmd in commands:
        if cmd[:2] == ["git", "checkout"]:
            kinds.append("checkout")
        elif cmd[:3] == ["git", "add", "--all"]:
            kinds.append("add")
        elif cmd[:2] == ["git", "commit"]:
            kinds.append("commit")
    return kinds


def test_new_file_is_reviewed_and_committed_from_the_index(tmp_path: Path) -> None:
    """An added file is staged once, reviewed, and committed as that index."""

    def edit(repo: Path) -> None:
        (repo / "NOTES.md").write_text("shipped\n")

    repo, ctx, repo_dir = _review_context(tmp_path, edit)
    seen: dict[str, Any] = {}
    commands: list[list[str]] = []
    _run_reviewed(repo, ctx, seen, commands)

    assert seen["has_changes"] is True
    assert "NOTES.md" in seen["diff"]
    assert seen["files"] == ["NOTES.md"]
    published = stored_patch(
        tmp_path, "acme/backend", load_status(ctx.status_file)["acme/backend"]
    ).read_text()
    assert published == seen["diff"]
    assert _commit_patch(repo_dir) == seen["diff"]
    assert _index_commands(commands) == ["checkout", "add", "commit"]
    branch = subprocess.run(
        ["git", "branch", "--show-current"],
        cwd=repo_dir,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    assert branch == "ci-fixes"


def _repo_with_local_origin(tmp_path: Path, dest: Path, name: str) -> None:
    """A clone whose origin is a local bare repo, so reuse can fetch offline.

    `url.<bare>.insteadOf` sends the GitHub origin clone() pins back to that bare.
    """
    seed = tmp_path / "seed"
    seed.mkdir()
    (seed / "README.md").write_text("hello\n")
    for args in (
        ["init", "--initial-branch=main"],
        ["config", "user.email", "test@test.com"],
        ["config", "user.name", "Test"],
        ["add", "."],
        ["commit", "-m", "init", "--quiet"],
    ):
        subprocess.run(["git", *args], cwd=seed, check=True, capture_output=True)
    bare = tmp_path / "origin.git"
    subprocess.run(
        ["git", "clone", "--bare", str(seed), str(bare)],
        check=True,
        capture_output=True,
    )
    dest.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["git", "clone", str(bare), str(dest)], check=True, capture_output=True
    )
    for args in (
        ["config", "user.email", "test@test.com"],
        ["config", "user.name", "Test"],
        ["config", f"url.{bare}.insteadOf", f"https://github.com/{name}.git"],
    ):
        subprocess.run(["git", *args], cwd=dest, check=True, capture_output=True)


def test_apply_commits_the_stored_new_file_patch_without_rerunning_the_fix(
    tmp_path: Path,
) -> None:
    """A later apply on the same dest must push the dry run's new file, not re-run."""
    runs: list[Path] = []

    def edit(repo: Path) -> None:
        runs.append(repo)
        (repo / "NOTES.md").write_text("shipped\n")

    repo = Repo("acme/backend", "main", ["rust"])
    repo_dir = tmp_path / "repos" / repo.slug
    _repo_with_local_origin(tmp_path, repo_dir, repo.name)
    outside = tmp_path / "repos" / "keep.txt"
    outside.write_text("keep\n")
    logs_dir = tmp_path / "logs"
    logs_dir.mkdir()
    ctx = run_fix.RunContext(
        fix=_TreeEdit(edit),
        config=FixConfig(
            branch="ci-fixes", pr_title="ci: ship the change", pr_body="p"
        ),
        config_dict={},
        fix_hash="reviewedhash",
        accepted_hashes=frozenset({"reviewedhash"}),
        repos_dir=tmp_path / "repos",
        logs_dir=logs_dir,
        patches_dir=tmp_path / "patches-reviewedhash",
        status_file=tmp_path / "status.yaml",
        apply=False,
        all_status={},
        pd=ProgressDisplay(total=1, workers=1),
        existing_prs={},
    )

    run_fix.execute_fix_on_repo(repo, ctx)

    assert len(runs) == 1
    stored = stored_patch(
        tmp_path, "acme/backend", load_status(ctx.status_file)["acme/backend"]
    ).read_text()
    assert "NOTES.md" in stored
    ctx.apply = True
    with patch.object(
        git,
        "push_and_open_pr",
        return_value=PRInfo("https://github.com/acme/backend/pull/7", "OPEN"),
    ):
        run_fix.execute_fix_on_repo(repo, ctx)

    assert len(runs) == 1
    assert _commit_patch(repo_dir) == stored
    assert outside.read_text() == "keep\n"


def test_tracked_edit_and_untracked_file_are_both_reviewed(tmp_path: Path) -> None:
    """A tracked edit plus a new file are both in the diff that is committed."""
    workflow = ".github/workflows/ci.yml"

    def edit(repo: Path) -> None:
        (repo / workflow).write_text("name: ci\nrun: echo shipped\n")
        (repo / "NOTES.md").write_text("shipped\n")

    repo, ctx, repo_dir = _review_context(tmp_path, edit)
    (repo_dir / ".github" / "workflows").mkdir(parents=True)
    (repo_dir / workflow).write_text("name: ci\n")
    subprocess.run(
        ["git", "add", "--all"], cwd=repo_dir, check=True, capture_output=True
    )
    subprocess.run(
        ["git", "commit", "-m", "workflow", "--quiet"],
        cwd=repo_dir,
        check=True,
        capture_output=True,
    )
    seen: dict[str, Any] = {}
    commands: list[list[str]] = []

    def pre_existing(_repo_dir: Path, _files: list[str]) -> set[LintFinding]:
        return {LintFinding(workflow, "pre-existing")}

    _run_reviewed(repo, ctx, seen, commands, actionlint=pre_existing)

    assert seen["has_changes"] is True
    assert "NOTES.md" in seen["diff"]
    assert "echo shipped" in seen["diff"]
    assert set(seen["files"]) == {workflow, "NOTES.md"}
    published = stored_patch(
        tmp_path, "acme/backend", load_status(ctx.status_file)["acme/backend"]
    ).read_text()
    assert published == seen["diff"]
    assert _commit_patch(repo_dir) == seen["diff"]
    assert _index_commands(commands) == ["checkout", "add", "commit"]


REVIEWED_DIFF = "diff --git a/README.md b/README.md\n--- reviewed\n"
UNREVIEWED_DIFF = "diff --git a/README.md b/README.md\n--- unreviewed\n"


def _reviewed_pair_context(
    tmp_path: Path, *, apply: bool
) -> tuple[Repo, run_fix.RunContext, dict[str, str], Path]:
    """A pending row and the patch file it authorizes, for acme/backend."""
    repo = Repo("acme/backend", "main", ["rust"])
    fix_hash = "reviewedhash"
    pending = {
        "status": "pending",
        "fix_hash": fix_hash,
        "base_sha": "abc123",
    }
    fix_dir = tmp_path / "fix"
    status_file = fix_dir / "status.yaml"
    patches = fix_dir / f"patches-{fix_hash}"
    patches.mkdir(parents=True)
    (patches / "acme-backend.patch").write_text(REVIEWED_DIFF)
    (patches / "acme-frontend.patch").write_text("diff --git a/kept\n")
    legacy = fix_dir / "patches-old999"
    legacy.mkdir()
    (legacy / "acme-legacy-tool.patch").write_text("diff --git a/legacy\n")
    atomic_write_yaml(
        {
            "acme/backend": pending,
            "acme/legacy-tool": {
                "status": "pending",
                "fix_hash": "old999",
                "base_sha": "legacy-sha",
            },
        },
        status_file,
    )
    logs_dir = fix_dir / "logs"
    logs_dir.mkdir()
    pd = ProgressDisplay(total=1, workers=1)
    ctx = run_fix.RunContext(
        fix=_make_fix_module(lambda _repo, _config: True),
        config=FixConfig(branch="test-branch", pr_title="t", pr_body="p"),
        config_dict={},
        fix_hash=fix_hash,
        accepted_hashes=frozenset({fix_hash}),
        repos_dir=fix_dir / "repos",
        logs_dir=logs_dir,
        patches_dir=patches,
        status_file=status_file,
        apply=apply,
        all_status=load_status(status_file),
        pd=pd,
        existing_prs={},
    )
    return repo, ctx, pending, fix_dir


def _assert_reviewed_pair_intact(
    fix_dir: Path, pending: dict[str, str], all_status: dict[str, dict[str, str]]
) -> None:
    """A later process: load status, then read the path stored_patch names."""
    loaded = load_status(fix_dir / "status.yaml")
    assert loaded["acme/backend"] == pending
    assert all_status["acme/backend"] == pending
    backend = stored_patch(fix_dir, "acme/backend", loaded["acme/backend"])
    assert backend == fix_dir / "patches-reviewedhash" / "acme-backend.patch"
    assert backend.read_text() == REVIEWED_DIFF
    legacy = stored_patch(fix_dir, "acme/legacy-tool", loaded["acme/legacy-tool"])
    assert legacy.read_text() == "diff --git a/legacy\n"
    assert (fix_dir / "patches-reviewedhash" / "acme-frontend.patch").read_text() == (
        "diff --git a/kept\n"
    )


def _no_review_context(
    tmp_path: Path, *, apply: bool, total: int = 1
) -> tuple[Path, run_fix.RunContext]:
    """A run with no status row and no stored patch."""
    fix_dir = tmp_path / "fix"
    logs_dir = fix_dir / "logs"
    logs_dir.mkdir(parents=True)
    ctx = run_fix.RunContext(
        fix=_make_fix_module(lambda _repo, _config: True),
        config=FixConfig(branch="test-branch", pr_title="t", pr_body="p"),
        config_dict={},
        fix_hash="currenthash",
        accepted_hashes=frozenset({"currenthash"}),
        repos_dir=fix_dir / "repos",
        logs_dir=logs_dir,
        patches_dir=fix_dir / "patches-currenthash",
        status_file=fix_dir / "status.yaml",
        apply=apply,
        all_status={},
        pd=ProgressDisplay(total=total, workers=1),
        existing_prs={},
    )
    return fix_dir, ctx


def test_status_write_crash_keeps_the_reviewed_pair(tmp_path: Path) -> None:
    """Kill the status write after a new diff exists. The old pair must remain.

    A later load of status.yaml must not authorize the new bytes with the old
    pending base_sha, and the other repos' patches stay put.
    """
    repo, ctx, pending, fix_dir = _reviewed_pair_context(tmp_path, apply=False)
    _make_git_repo(ctx.repos_dir / repo.slug)

    def crash(*_args: object, **_kwargs: object) -> None:
        raise ShutdownError

    with (
        patch.object(run_fix, "clone"),
        patch.object(run_fix, "run_fix_and_validate", return_value=True),
        patch.object(run_fix, "get_diff", return_value=UNREVIEWED_DIFF),
        patch.object(run_fix, "head_sha", return_value="abc123"),
        patch("repomod.status.atomic_write_yaml", side_effect=crash),
        pytest.raises(ShutdownError),
    ):
        run_fix.execute_fix_on_repo(repo, ctx)

    _assert_reviewed_pair_intact(fix_dir, pending, ctx.all_status)


def test_apply_shutdown_during_push_keeps_the_reviewed_pair(tmp_path: Path) -> None:
    """Push raises before success, so the reviewed patch and pending row stay."""
    repo, ctx, pending, fix_dir = _reviewed_pair_context(tmp_path, apply=True)

    with (
        patch.object(run_fix, "clone"),
        patch.object(run_fix, "head_sha", return_value="abc123"),
        patch.object(run_fix, "apply_patch", return_value=False),
        patch.object(run_fix, "run_fix_and_validate", return_value=True),
        patch.object(run_fix, "get_diff", return_value=UNREVIEWED_DIFF),
        patch.object(run_fix, "commit_and_push", side_effect=ShutdownError),
    ):
        run_fix.process_repo(WorkItem(repo), ctx)

    _assert_reviewed_pair_intact(fix_dir, pending, ctx.all_status)
    assert not ctx.pd.is_completed("acme/backend")


def test_apply_push_error_keeps_the_reviewed_pair(tmp_path: Path) -> None:
    """A push that fails before success must not replace the reviewed pair."""
    repo, ctx, pending, fix_dir = _reviewed_pair_context(tmp_path, apply=True)

    with (
        patch.object(run_fix, "clone"),
        patch.object(run_fix, "head_sha", return_value="abc123"),
        patch.object(run_fix, "apply_patch", return_value=False),
        patch.object(run_fix, "run_fix_and_validate", return_value=True),
        patch.object(run_fix, "get_diff", return_value=UNREVIEWED_DIFF),
        patch.object(
            run_fix,
            "commit_and_push",
            side_effect=subprocess.CalledProcessError(1, ["git", "push"]),
        ),
    ):
        run_fix.execute_fix_on_repo(repo, ctx)

    _assert_reviewed_pair_intact(fix_dir, pending, ctx.all_status)


def test_apply_push_error_without_a_reviewed_pair_records_failure_and_no_patch(
    tmp_path: Path,
) -> None:
    """Nothing was reviewed yet, so a failed push is recorded and stores no diff."""
    repo = Repo("acme/backend", "main", ["rust"])
    fix_dir = tmp_path / "fix"
    status_file = fix_dir / "status.yaml"
    patches = fix_dir / "patches-currenthash"
    patches.mkdir(parents=True)
    logs_dir = fix_dir / "logs"
    logs_dir.mkdir()
    ctx = run_fix.RunContext(
        fix=_make_fix_module(lambda _repo, _config: True),
        config=FixConfig(branch="test-branch", pr_title="t", pr_body="p"),
        config_dict={},
        fix_hash="currenthash",
        accepted_hashes=frozenset({"currenthash"}),
        repos_dir=fix_dir / "repos",
        logs_dir=logs_dir,
        patches_dir=patches,
        status_file=status_file,
        apply=True,
        all_status={},
        pd=ProgressDisplay(total=1, workers=1),
        existing_prs={},
    )

    with (
        patch.object(run_fix, "clone"),
        patch.object(run_fix, "run_fix_and_validate", return_value=True),
        patch.object(run_fix, "get_diff", return_value=UNREVIEWED_DIFF),
        patch.object(run_fix, "head_sha", return_value="abc123"),
        patch.object(
            run_fix,
            "commit_and_push",
            side_effect=subprocess.CalledProcessError(1, ["git", "push"]),
        ),
    ):
        run_fix.execute_fix_on_repo(repo, ctx)

    status = load_status(status_file)
    assert status["acme/backend"]["status"] == "failed"
    assert status["acme/backend"]["reason"] == "push/PR failed"
    assert list(patches.iterdir()) == []


def test_dry_run_publishes_a_new_pair_and_keeps_other_patches(
    tmp_path: Path,
) -> None:
    """The new pending row and its patch show up together; other repos stay."""
    repo, ctx, _pending, fix_dir = _reviewed_pair_context(tmp_path, apply=False)
    _make_git_repo(ctx.repos_dir / repo.slug)

    def run_once(diff: str, base_sha: str) -> None:
        with (
            patch.object(run_fix, "clone"),
            patch.object(run_fix, "run_fix_and_validate", return_value=True),
            patch.object(run_fix, "get_diff", return_value=diff),
            patch.object(run_fix, "head_sha", return_value=base_sha),
        ):
            run_fix.execute_fix_on_repo(repo, ctx)

    run_once(UNREVIEWED_DIFF, "def456")
    run_once("diff --git a/README.md b/README.md\n--- third\n", "fff999")

    loaded = load_status(fix_dir / "status.yaml")
    assert loaded["acme/backend"] == {
        "status": "pending",
        "fix_hash": "reviewedhash",
        "base_sha": "fff999",
    }
    backend = stored_patch(fix_dir, "acme/backend", loaded["acme/backend"])
    assert backend == fix_dir / "patches-reviewedhash" / "acme-backend.patch"
    assert backend.read_text() == "diff --git a/README.md b/README.md\n--- third\n"
    legacy = stored_patch(fix_dir, "acme/legacy-tool", loaded["acme/legacy-tool"])
    assert legacy == fix_dir / "patches-old999" / "acme-legacy-tool.patch"
    assert legacy.read_text() == "diff --git a/legacy\n"
    assert loaded["acme/legacy-tool"]["base_sha"] == "legacy-sha"
    assert (fix_dir / "patches-reviewedhash" / "acme-frontend.patch").read_text() == (
        "diff --git a/kept\n"
    )


def test_failed_get_diff_does_not_write_a_patch_or_status(tmp_path: Path) -> None:
    """A failed git diff leaves the reviewed pair in place and does not abort."""
    repo, ctx, pending, fix_dir = _reviewed_pair_context(tmp_path, apply=False)
    broken = ctx.repos_dir / repo.slug
    _make_git_repo(broken)
    (broken / ".git" / "index").write_bytes(b"not an index")

    with (
        patch.object(run_fix, "clone"),
        patch.object(run_fix, "run_fix_and_validate", return_value=True),
    ):
        run_fix.execute_fix_on_repo(repo, ctx)

    _assert_reviewed_pair_intact(fix_dir, pending, ctx.all_status)
    assert "diff failed" in (ctx.logs_dir / "acme-backend.log").read_text()
    assert ctx.pd.is_completed("acme/backend")
    assert ctx.pd.counts == {"done": 0, "failed": 1}


def test_get_diff_failure_records_failed_and_the_next_repo_still_runs(
    tmp_path: Path,
) -> None:
    """One repo's git diff failing is that repo's failure, not the run's."""
    backend = Repo("acme/backend", "main", ["rust"])
    frontend = Repo("acme/frontend", "main", ["rust"])
    fix_dir, ctx = _no_review_context(tmp_path, apply=False, total=2)

    def diff_for(repo_dir: Path) -> str:
        if repo_dir.name == backend.slug:
            raise subprocess.CalledProcessError(128, ["git", "diff", "--cached"])
        return UNREVIEWED_DIFF

    with (
        patch.object(run_fix, "clone"),
        patch.object(run_fix, "run_fix_and_validate", return_value=True),
        patch.object(run_fix, "get_diff", side_effect=diff_for),
        patch.object(run_fix, "head_sha", return_value="abc123"),
        patch.object(run_fix, "unstage"),
    ):
        interrupted = run_fix.run_work(
            ctx, [WorkItem(backend), WorkItem(frontend)], 1
        )

    assert interrupted is False
    status = load_status(fix_dir / "status.yaml")
    assert set(status) == {"acme/backend", "acme/frontend"}
    assert status["acme/backend"] == {"status": "failed", "reason": "diff failed"}
    assert status["acme/frontend"] == {
        "status": "pending",
        "fix_hash": "currenthash",
        "base_sha": "abc123",
    }
    assert list(fix_dir.rglob("acme-backend.patch")) == []
    saved = stored_patch(fix_dir, "acme/frontend", status["acme/frontend"])
    assert saved.read_text() == UNREVIEWED_DIFF
    assert "diff failed" in (ctx.logs_dir / "acme-backend.log").read_text()
    assert not (ctx.logs_dir / "acme-frontend.log").exists()
    assert ctx.pd.is_completed("acme/backend")
    assert ctx.pd.is_completed("acme/frontend")
    assert ctx.pd.counts == {"done": 1, "failed": 1}


def test_head_sha_oserror_records_failed_and_writes_no_patch(tmp_path: Path) -> None:
    """rev-parse failing is a repo failure, not a reason to abort the run."""
    repo = Repo("acme/backend", "main", ["rust"])
    fix_dir, ctx = _no_review_context(tmp_path, apply=False)

    with (
        patch.object(run_fix, "clone"),
        patch.object(run_fix, "run_fix_and_validate", return_value=True),
        patch.object(run_fix, "get_diff", return_value=UNREVIEWED_DIFF),
        patch.object(run_fix, "head_sha", side_effect=OSError("rev-parse failed")),
    ):
        run_fix.execute_fix_on_repo(repo, ctx)

    assert load_status(fix_dir / "status.yaml")["acme/backend"] == {
        "status": "failed",
        "reason": "diff failed",
    }
    assert list(fix_dir.rglob("*.patch")) == []
    assert "diff failed" in (ctx.logs_dir / "acme-backend.log").read_text()
    assert ctx.pd.counts == {"done": 0, "failed": 1}


def test_get_diff_error_after_interrupt_is_not_a_failure(tmp_path: Path) -> None:
    """CalledProcessError once ^C is set is cancellation, not a failed repo."""
    repo, ctx, pending, fix_dir = _reviewed_pair_context(tmp_path, apply=False)
    interrupt.event.clear()

    def killed(_repo_dir: Path) -> str:
        interrupt.event.set()
        raise subprocess.CalledProcessError(128, ["git", "diff", "--cached"])

    try:
        with (
            patch.object(run_fix, "clone"),
            patch.object(run_fix, "run_fix_and_validate", return_value=True),
            patch.object(run_fix, "get_diff", side_effect=killed),
        ):
            run_fix.process_repo(WorkItem(repo), ctx)
    finally:
        interrupt.event.clear()

    _assert_reviewed_pair_intact(fix_dir, pending, ctx.all_status)
    assert not (ctx.logs_dir / "acme-backend.log").exists()
    assert not ctx.pd.is_completed("acme/backend")
    assert ctx.pd.counts == {"done": 0, "failed": 0}


def test_patch_write_error_keeps_the_reviewed_pair(tmp_path: Path) -> None:
    """A failed patch write must not replace a pending review or abort."""
    repo, ctx, pending, fix_dir = _reviewed_pair_context(tmp_path, apply=False)
    _make_git_repo(ctx.repos_dir / repo.slug)

    with (
        patch.object(run_fix, "clone"),
        patch.object(run_fix, "run_fix_and_validate", return_value=True),
        patch.object(run_fix, "get_diff", return_value=UNREVIEWED_DIFF),
        patch.object(run_fix, "head_sha", return_value="fff999"),
        patch("repomod.status.write_patch", side_effect=OSError("disk full")),
    ):
        run_fix.execute_fix_on_repo(repo, ctx)

    _assert_reviewed_pair_intact(fix_dir, pending, ctx.all_status)
    assert "diff failed" in (ctx.logs_dir / "acme-backend.log").read_text()
    assert ctx.pd.counts == {"done": 0, "failed": 1}


def test_apply_publish_oserror_keeps_the_reviewed_pair(tmp_path: Path) -> None:
    """Saving the diff after a push is not a push failure and not an abort."""
    repo, ctx, pending, fix_dir = _reviewed_pair_context(tmp_path, apply=True)

    with (
        patch.object(run_fix, "clone"),
        patch.object(run_fix, "head_sha", return_value="abc123"),
        patch.object(run_fix, "apply_patch", return_value=False),
        patch.object(run_fix, "run_fix_and_validate", return_value=True) as rerun,
        patch.object(run_fix, "get_diff", return_value=UNREVIEWED_DIFF),
        patch.object(
            run_fix,
            "commit_and_push",
            return_value=PRInfo("https://github.com/acme/backend/pull/4", "OPEN"),
        ) as pushed,
        patch("repomod.status.write_patch", side_effect=OSError("disk full")),
    ):
        run_fix.execute_fix_on_repo(repo, ctx)

    rerun.assert_called_once()
    pushed.assert_called_once()
    _assert_reviewed_pair_intact(fix_dir, pending, ctx.all_status)
    log = (ctx.logs_dir / "acme-backend.log").read_text()
    assert "diff failed" in log
    assert "push/PR failed" not in log
    assert ctx.pd.counts == {"done": 0, "failed": 1}


@pytest.mark.parametrize(
    "exc",
    [
        pytest.param(subprocess.TimeoutExpired(["git", "apply"], 120), id="timeout"),
        pytest.param(OSError("cannot run git apply"), id="oserror"),
    ],
)
def test_apply_patch_timeout_or_oserror_does_not_reraise_or_rerun_the_fix(
    tmp_path: Path, exc: subprocess.TimeoutExpired | OSError
) -> None:
    """A timed-out or failed apply is not a patch that doesn't apply."""
    repo, ctx, pending, fix_dir = _reviewed_pair_context(tmp_path, apply=True)

    with (
        patch.object(run_fix, "clone"),
        patch.object(run_fix, "head_sha", return_value="abc123"),
        patch.object(run_fix, "apply_patch", side_effect=exc),
        patch.object(run_fix, "run_fix_and_validate", return_value=True) as rerun,
    ):
        run_fix.process_repo(WorkItem(repo), ctx)

    rerun.assert_not_called()
    _assert_reviewed_pair_intact(fix_dir, pending, ctx.all_status)
    assert "apply failed" in (ctx.logs_dir / "acme-backend.log").read_text()
    assert ctx.pd.is_completed("acme/backend")
    assert ctx.pd.counts == {"done": 0, "failed": 1}


def test_stored_patch_that_does_not_apply_runs_the_fix(tmp_path: Path) -> None:
    """git apply rejecting the stored diff re-runs the fix and is not a failure."""
    repo, ctx, pending, fix_dir = _reviewed_pair_context(tmp_path, apply=True)
    _make_git_repo(ctx.repos_dir / repo.slug)

    with (
        patch.object(run_fix, "clone"),
        patch.object(run_fix, "head_sha", return_value="abc123"),
        patch.object(run_fix, "run_fix_and_validate", return_value=False) as rerun,
    ):
        run_fix.execute_fix_on_repo(repo, ctx)

    rerun.assert_called_once()
    _assert_reviewed_pair_intact(fix_dir, pending, ctx.all_status)
    assert ctx.pd.counts == {"done": 0, "failed": 0}
    assert not (ctx.logs_dir / "acme-backend.log").exists()


def _config_with(fix_dir: Path, text: str) -> str:
    (fix_dir / "config.yaml").write_text(text)
    return run_fix.compute_fix_hash(fix_dir)


BASE_CONFIG = """\
branch: test-branch
pr_title: "test: a fix"
pr_body: |
  Test body.
"""


def test_skip_list_and_comment_edits_do_not_change_the_fix_hash(
    tmp_path: Path,
) -> None:
    """Neither changes what the fix does to a repo, so saved results stay valid."""
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir)
    original = _config_with(fix_dir, BASE_CONFIG)

    with_skip = _config_with(
        fix_dir,
        BASE_CONFIG + "skip:\n  # pinned for glibc\n  - acme/repo-alpha\n",
    )
    reformatted = _config_with(
        fix_dir,
        "# a new comment\nbranch:   test-branch\npr_title: 'test: a fix'\n"
        "pr_body: |\n  Test body.\n",
    )

    assert with_skip == original
    assert reformatted == original


def test_edits_that_change_what_the_fix_does_change_the_fix_hash(
    tmp_path: Path,
) -> None:
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir)
    original = _config_with(fix_dir, BASE_CONFIG)

    for changed in (
        BASE_CONFIG.replace("test-branch", "other-branch"),
        BASE_CONFIG.replace("a fix", "another fix"),
        BASE_CONFIG.replace("Test body.", "Different body."),
        BASE_CONFIG + "reviewers:\n  - octocat\n",
    ):
        assert _config_with(fix_dir, changed) != original

    _config_with(fix_dir, BASE_CONFIG)
    (fix_dir / "main.py").write_text((fix_dir / "main.py").read_text() + "\n# edit\n")
    assert run_fix.compute_fix_hash(fix_dir) != original


def test_results_recorded_under_the_old_style_hash_still_count_as_current() -> None:
    """Existing status.yaml files (hash of the raw bytes) must not all go stale."""
    config = FixConfig(branch="b", pr_title="t", pr_body="p")
    mod = _make_fix_module(lambda r, c: True)
    status = {
        "acme/repo-alpha": {"status": "pr_created", "fix_hash": "legacy-hash"},
        "acme/repo-beta": {"status": "skipped", "fix_hash": "older-hash"},
    }

    work, done = filter_work(
        [REPO_A, REPO_B],
        status,
        config,
        mod,
        "new-hash",
        None,
        apply=True,
        accepted_hashes=frozenset({"new-hash", "legacy-hash"}),
    )

    assert [w.repo.name for w in work] == ["acme/repo-beta"]
    assert done == 1


def test_a_skip_list_edit_after_a_dry_run_does_not_redo_the_dry_run(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The reported incident: dry run, then add repos to the skip list, then --apply."""
    run_log = tmp_path / "runs.log"
    monkeypatch.setenv("RUN_LOG", str(run_log))
    fix_dir = tmp_path / "fix"
    _write_fix(
        fix_dir,
        body=(
            "    import os\n"
            "    open(os.environ['RUN_LOG'], 'a').write('ran\\n')\n"
            "    (repo / 'README.md').write_text('changed')\n"
            "    return True"
        ),
    )
    (fix_dir / "config.yaml").write_text(BASE_CONFIG)
    for repo in (REPO_A, REPO_B):
        _make_git_repo(fix_dir / "repos" / repo.slug)

    with _mock_sync(repos=[REPO_A, REPO_B]), _mock_clone_git():
        run_fix.main([str(fix_dir), "--workers", "1"])
    assert run_log.read_text().count("ran") == 2

    (fix_dir / "config.yaml").write_text(
        BASE_CONFIG + "skip:\n  # excluded after review\n  - acme/repo-beta\n"
    )
    with _mock_sync(repos=[REPO_A, REPO_B]), _mock_clone_git():
        run_fix.main([str(fix_dir), "--workers", "1"])

    assert run_log.read_text().count("ran") == 2  # nothing was re-processed
    status = load_status(fix_dir / "status.yaml")
    assert status["acme/repo-alpha"]["status"] == "pending"
