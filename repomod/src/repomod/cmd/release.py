from __future__ import annotations

import argparse
import os
import sys
import time
from dataclasses import dataclass
from typing import Any

from repomod import github
from repomod.catalog import repository_path
from repomod.types import RemoteRepository, Repository

from . import common

_POLL_INTERVAL_SECONDS = 15
_RUN_APPEAR_TIMEOUT_SECONDS = 180
_RUN_APPEAR_POLL_SECONDS = 5
_WORKFLOW_FILES = ("release.yml", "release.yaml", "prerelease.yml", "prerelease.yaml")

RepoState = str

_STATE_EXISTS = "exists"
_STATE_DISPATCHED = "dispatched"
_STATE_RUNNING = "running"
_STATE_PASSED = "passed"
_STATE_FAILED = "failed"
_STATE_SKIPPED = "skipped"
_STATE_ERROR = "error"


@dataclass(slots=True)
class ReleaseTarget:
    repository: Repository
    workflow: str | None
    remote: RemoteRepository | None
    tag: str | None
    state: RepoState
    detail: str
    run_id: int | None = None
    run_url: str | None = None

    @property
    def name(self) -> str:
        return str(self.repository.id)

    @property
    def done(self) -> bool:
        return self.state in {_STATE_EXISTS, _STATE_PASSED, _STATE_FAILED, _STATE_SKIPPED, _STATE_ERROR}


def _release_workflow(catalog, repository: Repository) -> str | None:
    try:
        root = repository_path(catalog, repository)
    except (OSError, ValueError):
        return None
    workflows = root / ".github" / "workflows"
    if not workflows.is_dir():
        return None
    for name in _WORKFLOW_FILES:
        path = workflows / name
        if not path.is_file() or path.is_symlink():
            continue
        try:
            source = path.read_text(encoding="utf-8")
        except (OSError, UnicodeError):
            continue
        lowered = source.casefold()
        if "workflow_dispatch" in lowered and "release-" in lowered:
            return name
    return None


def _color(state: RepoState) -> str:
    return {
        _STATE_EXISTS: "32",
        _STATE_PASSED: "32",
        _STATE_RUNNING: "33",
        _STATE_DISPATCHED: "33",
        _STATE_FAILED: "31",
        _STATE_ERROR: "31",
        _STATE_SKIPPED: "90",
    }.get(state, "90")


def _label(target: ReleaseTarget) -> str:
    base = f"{target.state:<10} {target.name}"
    if target.detail:
        base += f"  {target.detail}"
    if target.run_url is not None and target.run_url != "":
        base += f"  {target.run_url}"
    return base


def _render(targets: list[ReleaseTarget], *, previous: int, tty: bool) -> int:
    lines = [common._paint(_label(target), _color(target.state)) for target in targets]
    if tty and previous:
        sys.stdout.write(f"\033[{previous}A")
    for line in lines:
        if tty:
            sys.stdout.write(f"\r\033[K{line}\n")
        else:
            sys.stdout.write(f"{line}\n")
    sys.stdout.flush()
    return len(lines)


def _attach_run(target: ReleaseTarget) -> bool:
    try:
        run = _find_run(target)
    except github.GitHubError:
        return False
    if run is None:
        return False
    status = run.get("status")
    conclusion = run.get("conclusion")
    run_id = run.get("databaseId")
    target.run_id = int(run_id) if isinstance(run_id, int) and not isinstance(run_id, bool) else None
    run_url = run.get("url")
    target.run_url = run_url if isinstance(run_url, str) else None
    if status != "completed":
        target.state = _STATE_RUNNING
        target.detail = f"{target.tag} {status}"
        return True
    lowered = conclusion.casefold() if isinstance(conclusion, str) else ""
    if lowered == "success":
        target.state = _STATE_RUNNING
        target.detail = f"{target.tag} run completed"
        return True
    return False


def _dispatch_target(catalog, target: ReleaseTarget) -> None:
    target.workflow = _release_workflow(catalog, target.repository)
    if target.workflow is None:
        target.state = _STATE_SKIPPED
        target.detail = "no dispatchable release workflow"
        return
    remote = target.remote
    if (
        remote is None
        or (remote.error is not None and remote.error != "")
        or remote.default_branch is None
        or remote.default_branch == ""
        or remote.sha is None
        or remote.sha == ""
    ):
        target.state = _STATE_ERROR
        target.detail = (
            remote.error
            if remote is not None and remote.error is not None and remote.error != ""
            else "default branch HEAD unavailable"
        )
        return
    target.tag = f"release-{remote.sha}"
    try:
        if github.release_tag_exists(target.repository.id, target.tag):
            target.state = _STATE_EXISTS
            target.detail = target.tag
            return
    except github.GitHubError as error:
        target.state = _STATE_ERROR
        target.detail = str(error)
        return
    if _attach_run(target):
        return
    try:
        github.dispatch_workflow(target.repository.id, target.workflow, remote.default_branch)
    except github.GitHubError as error:
        target.state = _STATE_ERROR
        target.detail = str(error)
        return
    target.state = _STATE_DISPATCHED
    target.detail = f"{target.tag} on {remote.default_branch}"


def _find_run(target: ReleaseTarget) -> dict[str, Any] | None:
    if target.workflow is None or target.remote is None or target.remote.sha is None:
        return None
    runs = github.workflow_runs(
        target.repository.id,
        target.workflow,
        branch=target.remote.default_branch,
        event="workflow_dispatch",
        limit=15,
    )
    for run in runs:
        if run.get("headSha") == target.remote.sha:
            return run
    return None


def _check_for_run(target: ReleaseTarget, deadline: float) -> bool:
    try:
        run = _find_run(target)
    except github.GitHubError as error:
        target.state = _STATE_ERROR
        target.detail = str(error)
        return True
    if run is not None:
        run_id = run.get("databaseId")
        target.run_id = int(run_id) if isinstance(run_id, int) and not isinstance(run_id, bool) else None
        run_url = run.get("url")
        target.run_url = run_url if isinstance(run_url, str) else None
        target.state = _STATE_RUNNING
        target.detail = f"{target.tag} run {target.run_id or '?'}"
        return True
    if time.time() >= deadline:
        target.state = _STATE_ERROR
        target.detail = "dispatched but workflow run did not appear"
        return True
    return False


def _poll_run(target: ReleaseTarget) -> bool:
    if target.run_id is None:
        target.state = _STATE_ERROR
        target.detail = "missing workflow run id"
        return True
    try:
        run = github.workflow_run(target.repository.id, target.run_id)
    except github.GitHubError as error:
        target.state = _STATE_ERROR
        target.detail = str(error)
        return True
    status = run.get("status")
    conclusion = run.get("conclusion")
    run_url = run.get("url")
    target.run_url = run_url if isinstance(run_url, str) else target.run_url
    if status != "completed":
        detail = f"{target.tag} {status}"
        changed = detail != target.detail or target.state != _STATE_RUNNING
        target.state = _STATE_RUNNING
        target.detail = detail
        return changed
    lowered = conclusion.casefold() if isinstance(conclusion, str) else "unknown"
    if lowered == "success":
        target.state = _STATE_PASSED
        target.detail = str(target.tag)
    else:
        target.state = _STATE_FAILED
        target.detail = f"{target.tag} {lowered}"
    return True


def _release(args: argparse.Namespace) -> int:
    catalog = common.default_catalog()
    repositories = common.select_repositories(catalog, args.repo, args.tag)
    remotes = github.remote_metadata(repositories)
    targets = [
        ReleaseTarget(
            repository=repository,
            workflow=None,
            remote=remotes.get(str(repository.id)),
            tag=None,
            state=_STATE_SKIPPED,
            detail="pending",
        )
        for repository in repositories
    ]

    tty = sys.stdout.isatty() and "NO_COLOR" not in os.environ
    rendered = 0
    for target in targets:
        _dispatch_target(catalog, target)
    rendered = _render(targets, previous=rendered, tty=tty)

    appear_deadlines = {
        target.name: time.time() + _RUN_APPEAR_TIMEOUT_SECONDS
        for target in targets
        if target.state == _STATE_DISPATCHED
    }
    while not all(target.done for target in targets):
        changed = False
        for target in targets:
            if (
                target.state == _STATE_DISPATCHED
                and _check_for_run(target, appear_deadlines[target.name])
            ) or (target.state == _STATE_RUNNING and _poll_run(target)):
                changed = True
        if changed:
            rendered = _render(targets, previous=rendered, tty=tty)
        if not all(target.done for target in targets):
            time.sleep(_POLL_INTERVAL_SECONDS)

    passed = sum(1 for target in targets if target.state in {_STATE_EXISTS, _STATE_PASSED})
    failed = sum(1 for target in targets if target.state in {_STATE_FAILED, _STATE_ERROR})
    skipped = sum(1 for target in targets if target.state == _STATE_SKIPPED)
    print(f"summary: ok {passed}; failed {failed}; skipped {skipped}")
    return 1 if failed else 0
