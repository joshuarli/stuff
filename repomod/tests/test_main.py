"""Integration tests for repomod.main pipeline."""

import os
import subprocess
import tempfile
import time
from collections.abc import Generator
from contextlib import contextmanager
from pathlib import Path
from typing import Any
from unittest.mock import patch

import pytest
import yaml

from repomod import main as run_fix
from repomod.main import compute_fix_hash, filter_work
from repomod.status import atomic_write_yaml, load_status
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
from repomod.utils import interrupt

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
    """Return a subprocess.run side_effect that stubs out clone/fetch/reset."""
    original_run = subprocess.run

    def side_effect(cmd: Any, **kwargs: Any) -> subprocess.CompletedProcess[Any]:
        cmd_str = " ".join(str(c) for c in cmd)
        if any(
            x in cmd_str
            for x in (
                "git clone",
                "git fetch",
                "git reset --hard",
                "git branch --list",
                "git checkout --force",
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
    _make_git_repo(fix_dir / "repos" / "test-repo")

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
    _make_git_repo(fix_dir / "repos" / "test-repo")

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
        _make_git_repo(fix_dir / "repos" / repo.short_name)

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
    _make_git_repo(fix_dir / "repos" / "test-repo")

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
    _make_git_repo(fix_dir / "repos" / "test-repo")

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
    _make_git_repo(fix_dir / "repos" / REPO_A.short_name)
    _make_git_repo(fix_dir / "repos" / REPO_B.short_name)
    (fix_dir / "repos" / REPO_B.short_name / ".fail").write_text("")

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
    _make_git_repo(fix_dir / "repos" / "test-repo")
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
    repo_dir = fix_dir / "repos" / "test-repo"
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
    _make_git_repo(fix_dir / "repos" / "test-repo")

    with _mock_sync(), _mock_clone_git():
        run_fix.main([str(fix_dir)])

    patches_dir = fix_dir / f"patches-{compute_fix_hash(fix_dir)}"
    assert list(patches_dir.iterdir()) == []


def test_e2e_apply_records_already_merged_pr_without_calling_it_created(
    tmp_path: Path,
) -> None:
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir)
    _make_git_repo(fix_dir / "repos" / "test-repo")
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
    _make_git_repo(fix_dir / "repos" / "test-repo")

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
    _make_git_repo(fix_dir / "repos" / "test-repo")

    with _mock_sync(), _mock_clone_git():
        run_fix.main([str(fix_dir)])

    status = load_status(fix_dir / "status.yaml")
    assert status["acme/test-repo"]["status"] == "pending"


def test_e2e_crashing_validate_fails_the_repo_with_a_traceback(tmp_path: Path) -> None:
    fix_dir = tmp_path / "fix"
    _write_fix_with_validate(fix_dir, "    raise RuntimeError('validator bug')")
    _make_git_repo(fix_dir / "repos" / "test-repo")

    with _mock_sync(), _mock_clone_git():
        run_fix.main([str(fix_dir)])

    status = load_status(fix_dir / "status.yaml")
    assert status["acme/test-repo"]["reason"] == "validation crashed"
    log_path = fix_dir / f"logs-{compute_fix_hash(fix_dir)}" / "acme-test-repo.log"
    assert "validator bug" in log_path.read_text()


def _dry_run_two_repos(tmp_path: Path) -> Path:
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir)
    _make_git_repo(fix_dir / "repos" / REPO_A.short_name)
    _make_git_repo(fix_dir / "repos" / REPO_B.short_name)
    with _mock_sync(repos=[REPO_B, REPO_A]), _mock_clone_git():
        run_fix.main([str(fix_dir), "--workers", "1"])
    return fix_dir


def test_dry_run_ends_with_each_pending_repos_diffstat(
    tmp_path: Path, capsys: Any
) -> None:
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir)
    _make_git_repo(fix_dir / "repos" / REPO_A.short_name)

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


def test_ctrl_c_during_apply_keeps_earlier_results_instead_of_marking_interrupted(
    tmp_path: Path,
) -> None:
    """A dry run's saved diff (and an existing PR) survive an interrupted re-run.

    The first repo's fix triggers ^C. alpha completes before it is noticed; beta
    (a pending dry-run diff) and gamma (an existing PR) were queued behind it and
    must keep their results rather than be downgraded to interrupted.
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
        _make_git_repo(fix_dir / "repos" / repo.short_name)
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
            patch.object(run_fix, "write_patch"),
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
        _make_git_repo(fix_dir / "repos" / repo.short_name)

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
