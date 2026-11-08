from __future__ import annotations

import argparse
import os
import re
import shutil
import sys
import time
from collections.abc import Mapping, Sequence
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from repomod import github
from repomod.catalog import CatalogError, repository_path
from repomod.git import branch_checked_out_time, oldest_unpushed_commit_time, status
from repomod.types import (
    Catalog,
    CiResult,
    GitStatus,
    Repository,
    StatusArea,
    StatusItem,
    StatusLevel,
    WorkflowStatus,
)

from . import common

STALE_LOCAL_WORK_DAYS = 7
_SECONDS_PER_DAY = 86400

_TODO_ITEM = re.compile(r"### [A-Z][A-Z0-9]*-\d+ · P\d+ · .+")


@dataclass(frozen=True, slots=True)
class StatusReport:
    rows: list[dict[str, object]]
    ci: dict[str, CiResult]


def _age_days(since: int | None, now: float) -> int | None:
    return None if since is None else max(0, int((now - since) // _SECONDS_PER_DAY))


def _stale_local_work(branch_days: int | None, unpushed_days: int | None) -> bool:
    return any(
        days is not None and days >= STALE_LOCAL_WORK_DAYS for days in (branch_days, unpushed_days)
    )


def _local_label(
    value: GitStatus,
    default_branch: str | None,
    branch_days: int | None = None,
    unpushed_days: int | None = None,
) -> str:
    if not value.present:
        return "missing"
    if not value.valid:
        return "invalid"
    if not value.origin_valid:
        return "invalid origin"
    if value.detached:
        return "detached"
    if value.dirty:
        count = value.staged + value.unstaged + value.untracked
        state = f"dirty ({count})"
    elif (
        value.ahead is not None
        and value.behind is not None
        and value.ahead > 0
        and value.behind > 0
    ):
        state = f"diverged {value.ahead}/{value.behind} (last fetch)"
    elif value.ahead is not None and value.ahead > 0:
        state = f"ahead {value.ahead} (last fetch)"
    elif value.behind is not None and value.behind > 0:
        state = f"behind {value.behind} (last fetch)"
    else:
        state = "clean"
    if unpushed_days is not None:
        state += f", oldest unpushed {unpushed_days}d"
    if value.branch is not None and value.branch != default_branch:
        state += f" on {value.branch}"
        if branch_days is not None:
            state += f" for {branch_days}d"
    return state


def _ci_label(value: CiResult) -> str:
    label = value.state
    if value.cached:
        age = "?" if value.age_seconds is None else f"{value.age_seconds}s"
        source_sha = (value.sha or "unknown")[:12]
        label += f" cached {age} sha {source_sha}"
    return label


def checkout_status(
    catalog: Catalog, repository: Repository, checkout: Path | None = None
) -> tuple[Path, GitStatus]:
    if checkout is not None:
        return checkout, status(checkout, repository.id)
    parts = (
        (repository.id.name,)
        if repository.id.owner.casefold() == catalog.personal_owner.casefold()
        else (repository.id.owner, repository.id.name)
    )
    path = catalog.root.joinpath(*parts)
    try:
        repository_path(catalog, repository)
        return path, status(path, repository.id)
    except (CatalogError, OSError) as error:
        return path, GitStatus(
            str(repository.id),
            path,
            path.exists() or path.is_symlink(),
            False,
            None,
            False,
            0,
            0,
            0,
            None,
            None,
            None,
            False,
            str(error),
        )


def todo_item_count(path: Path) -> int:
    if not path.is_dir():
        return 0
    todo_path = path / "TODO.md"
    try:
        descriptor = os.open(todo_path, os.O_RDONLY | os.O_NOFOLLOW)
    except FileNotFoundError:
        return 0
    with os.fdopen(descriptor, encoding="utf-8") as todo_file:
        return sum(_TODO_ITEM.fullmatch(line.rstrip("\r\n")) is not None for line in todo_file)


def _status_checkout(
    catalog: Catalog, repository: Repository, checkout: Path | None = None
) -> tuple[Path, GitStatus, int]:
    path, local = checkout_status(catalog, repository, checkout)
    todo_items = todo_item_count(path) if local.valid else 0
    return path, local, todo_items


def _state_color(state: str) -> str:
    return {
        "passed": "32",
        "failed": "31",
        "pending": "33",
        "cancelled": "33",
        "skipped": "90",
        "neutral": "90",
        "none": "90",
        "unknown": "33",
        "stale": "33",
    }.get(state, "90")


def _workflow_label(state: str) -> str:
    return {
        "passed": "PASS",
        "failed": "FAIL",
        "pending": "RUN",
        "cancelled": "CANCEL",
        "skipped": "SKIP",
        "neutral": "NEUTRAL",
        "none": "NONE",
        "unknown": "UNKNOWN",
        "stale": "STALE",
    }.get(state, "UNKNOWN")


def _local_color(value: str, stale: bool = False) -> str:
    if value.startswith(("missing", "invalid")) or stale:
        return "31"
    if value.startswith(("dirty", "diverged", "ahead", "behind", "detached")):
        return "33"
    return "32"


def _print_wrapped_items(items: Sequence[tuple[str, str]], *, prefix: str = "  ") -> None:
    width = max(40, shutil.get_terminal_size((80, 24)).columns)
    line = prefix
    length = len(prefix)
    for label, color in items:
        separator = " · " if length > len(prefix) else ""
        if length > len(prefix) and length + len(separator) + len(label) > width:
            print(line)
            line = prefix + common._paint(label, color)
            length = len(prefix) + len(label)
        else:
            line += separator + common._paint(label, color)
            length += len(separator) + len(label)
    if length > len(prefix):
        print(line)


def _has_conflicting_pr(row: dict[str, object]) -> bool:
    requests = row.get("open_prs")
    if not isinstance(requests, list):
        return False
    return any(
        isinstance(request, dict) and request.get("mergeable") == "CONFLICTING"
        for request in requests
    )


def _pr_label(request: object) -> str:
    if not isinstance(request, dict):
        return "#? UNKNOWN"
    number = request.get("number", "?")
    return f"#{number} {request.get('mergeable', 'UNKNOWN')}·{request.get('merge_state', 'UNKNOWN')}"


def _upstream_row(upstream: github.UpstreamStatus | None) -> dict[str, object] | None:
    if upstream is None:
        return None
    return {
        "parent": upstream.parent,
        "parent_branch": upstream.parent_branch,
        "parent_sha": upstream.parent_sha,
        "behind_by": upstream.behind_by,
        "ahead_by": upstream.ahead_by,
        "state": upstream.state,
        "cached": upstream.cached,
        "age_seconds": upstream.age_seconds,
        "error": upstream.error,
    }


def _upstream_behind(row: dict[str, object]) -> bool:
    upstream = row.get("upstream")
    if not isinstance(upstream, dict):
        return False
    behind = upstream.get("behind_by")
    return isinstance(behind, int) and not isinstance(behind, bool) and behind > 0


def _upstream_label(upstream: dict[str, Any]) -> tuple[str, str]:
    parent = upstream.get("parent")
    state = upstream.get("state")
    cached = upstream.get("cached") is True and isinstance(upstream.get("age_seconds"), int)
    suffix = f" (cached {upstream['age_seconds']}s)" if cached else ""
    if not isinstance(parent, str) or not parent:
        return f"unavailable{suffix}", "33"
    behind = upstream.get("behind_by")
    ahead = upstream.get("ahead_by")
    behind_count = behind if isinstance(behind, int) and not isinstance(behind, bool) else None
    ahead_count = ahead if isinstance(ahead, int) and not isinstance(ahead, bool) else None
    if upstream.get("error") is not None or state == "unknown":
        return f"{parent} unavailable{suffix}", "33"
    if state == "identical":
        return f"{parent} in sync{suffix}", "32"
    if state == "ahead" and ahead_count is not None:
        return f"{parent} ahead {ahead_count}{suffix}", "33"
    if behind_count is not None and ahead_count is not None and ahead_count > 0:
        return f"{parent} behind {behind_count} · ahead {ahead_count}{suffix}", "33"
    if behind_count is not None:
        return f"{parent} behind {behind_count}{suffix}", "33"
    return f"{parent} {state}{suffix}", "33"


def _upstream_surfaced(row: dict[str, object]) -> bool:
    upstream = row.get("upstream")
    if not isinstance(upstream, dict):
        return False
    if _upstream_behind(row) or upstream.get("error") is not None or upstream.get("state") == "unknown":
        return True
    parent = upstream.get("parent")
    return not isinstance(parent, str) or not parent


def _local_surfaced(row: dict[str, object]) -> bool:
    local = str(row["local"])
    return row.get("stale_local_work") is True or local.startswith(
        ("missing", "invalid", "detached", "dirty", "diverged", "ahead", "behind")
    )


def _release_row(release: github.ReleaseStatus | None) -> dict[str, object] | None:
    if release is None:
        return None
    return {
        "tag": release.tag,
        "release_sha": release.release_sha,
        "url": release.url,
        "published_at": release.published_at,
        "head_sha": release.head_sha,
        "commits_since": release.commits_since,
        "state": release.state,
        "cached": release.cached,
        "age_seconds": release.age_seconds,
        "error": release.error,
        "prerelease": release.prerelease,
    }


def _release_label(release: dict[str, Any]) -> tuple[str, str]:
    tag = release.get("tag")
    state = release.get("state")
    cached = release.get("cached") is True and isinstance(release.get("age_seconds"), int)
    suffix = f" (cached {release['age_seconds']}s)" if cached else ""
    if release.get("error") is not None or state == "unknown":
        return f"unavailable{suffix}", "33"
    if state == "no-releases":
        return f"no releases{suffix}", "33"
    if not isinstance(tag, str) or not tag:
        return f"unavailable{suffix}", "33"
    name = f"{tag} (prerelease)" if release.get("prerelease") is True else tag
    commits = release.get("commits_since")
    count = commits if isinstance(commits, int) and not isinstance(commits, bool) else None
    if state == "up-to-date":
        return f"{name} · up to date{suffix}", "32"
    if state == "diverged" and count is not None:
        return f"{name} · {count} since (diverged){suffix}", "33"
    if count is not None:
        return f"{name} · {count} since{suffix}", "33"
    return f"{name} {state}{suffix}", "33"


def _release_surfaced(row: dict[str, object]) -> bool:
    release = row.get("release")
    if not isinstance(release, dict):
        return False
    if release.get("error") is not None or release.get("state") == "unknown":
        return True
    return release.get("state") in {"behind", "diverged"}


def _workflow_surfaced(workflow: WorkflowStatus) -> bool:
    return workflow.state not in {"passed", "skipped"}


def _url(value: object) -> str | None:
    return value if isinstance(value, str) and value else None


def status_items(row: dict[str, object], ci: CiResult) -> tuple[StatusItem, ...]:
    items: list[StatusItem] = []
    if _local_surfaced(row):
        local = str(row["local"])
        color = _local_color(local, row.get("stale_local_work") is True)
        items.append(StatusItem(StatusArea.LOCAL, local, StatusLevel(color)))
    failed_workflows = ci.workflows is not None and any(
        workflow.state == "failed" for workflow in ci.workflows
    )
    if ci.state not in {"passed", "none"} and not (ci.state == "failed" and failed_workflows):
        items.append(StatusItem(StatusArea.CI, _ci_label(ci), StatusLevel(_state_color(ci.state))))
    if ci.workflows is None:
        items.append(StatusItem(StatusArea.WORKFLOWS, "unavailable", StatusLevel.ATTENTION))
    else:
        items.extend(
            StatusItem(
                StatusArea.WORKFLOWS,
                f"{_workflow_label(workflow.state)} {workflow.name}",
                StatusLevel(_state_color(workflow.state)),
                workflow.url,
            )
            for workflow in ci.workflows
            if _workflow_surfaced(workflow)
        )
    requests = row.get("open_prs")
    if isinstance(requests, list):
        items.extend(
            StatusItem(
                StatusArea.PULL_REQUESTS,
                _pr_label(request),
                StatusLevel.FAILING,
                _url(request.get("url")),
            )
            for request in requests
            if isinstance(request, dict) and request.get("mergeable") == "CONFLICTING"
        )
    upstream: Any = row.get("upstream")
    if _upstream_surfaced(row):
        assert isinstance(upstream, dict)
        label, color = _upstream_label(upstream)
        items.append(StatusItem(StatusArea.UPSTREAM, label, StatusLevel(color)))
    release: Any = row.get("release")
    if _release_surfaced(row):
        assert isinstance(release, dict)
        label, color = _release_label(release)
        items.append(
            StatusItem(StatusArea.RELEASE, label, StatusLevel(color), _url(release.get("url")))
        )
    return tuple(items)


def _print_workflows(items: Sequence[StatusItem], width: int) -> None:
    labelled = [(item.label, item.level.value) for item in items]
    length = len("  workflows ") + sum(len(label) for label, _ in labelled)
    length += 3 * max(0, len(labelled) - 1)
    if length > width:
        print("  workflows")
        _print_wrapped_items(labelled, prefix="    ")
    else:
        _print_wrapped_items([(f"workflows {labelled[0][0]}", labelled[0][1]), *labelled[1:]])


def _print_status_items(items: Sequence[StatusItem], ci: CiResult, width: int) -> None:
    def of(*areas: StatusArea) -> list[StatusItem]:
        return [item for item in items if item.area in areas]

    summary = of(StatusArea.LOCAL, StatusArea.CI)
    if summary:
        _print_wrapped_items([(f"{item.area.value} {item.label}", item.level.value) for item in summary])
    if ci.workflows is None:
        print(f"  workflows {common._paint('unavailable', StatusLevel.ATTENTION.value)}")
    elif workflows := of(StatusArea.WORKFLOWS):
        _print_workflows(workflows, width)
    if requests := of(StatusArea.PULL_REQUESTS):
        first, *rest = requests
        _print_wrapped_items(
            [
                (f"{first.area.value} {first.label}", first.level.value),
                *((item.label, item.level.value) for item in rest),
            ]
        )
    for item in of(StatusArea.UPSTREAM, StatusArea.RELEASE):
        print(f"  {item.area.value} {common._paint(item.label, item.level.value)}")


def status_text(
    rows: Sequence[dict[str, object]],
    ci_by_repository: dict[str, CiResult],
) -> None:
    width = max(40, shutil.get_terminal_size((80, 24)).columns)
    surfaced = [
        (row, items)
        for row in rows
        if (items := status_items(row, ci_by_repository[str(row["repository"])]))
    ]
    for index, (row, items) in enumerate(surfaced):
        if index:
            print()
        name = str(row["repository"])
        print(common._paint(name, "36"))
        _print_status_items(items, ci_by_repository[name], width)


def collect_status(
    catalog: Catalog,
    repositories: tuple[Repository, ...],
    overrides: Mapping[str, Path],
    *,
    offline: bool,
    refresh: bool,
) -> StatusReport:
    with ThreadPoolExecutor(
        max_workers=min(32, max(1, 2 * len(repositories))), thread_name_prefix="repomod-status"
    ) as pool:
        checkout_futures = {
            str(repository.id): pool.submit(
                _status_checkout, catalog, repository, overrides.get(str(repository.id))
            )
            for repository in repositories
        }
        checkouts = {name: future.result() for name, future in checkout_futures.items()}
    remotes = {} if offline else github.remote_metadata(repositories)
    with ThreadPoolExecutor(max_workers=4, thread_name_prefix="repomod-status") as pool:
        ci_future = pool.submit(
            github.ci_status, repositories, remotes, offline=offline, refresh=refresh
        )
        releases_future = pool.submit(
            github.release_status, repositories, remotes, offline=offline, refresh=refresh
        )
        upstreams_future = pool.submit(
            github.upstream_status, repositories, remotes, offline=offline, refresh=refresh
        )
        pull_requests_future = (
            pool.submit(github.open_pull_requests, repositories) if not offline else None
        )
        ci = ci_future.result()
        releases = releases_future.result()
        upstreams = upstreams_future.result()
        pull_requests = pull_requests_future.result() if pull_requests_future is not None else {}
    rows: list[dict[str, object]] = []
    for repository in repositories:
        name = str(repository.id)
        _, local, todo_items = checkouts[name]
        remote = remotes.get(name)
        ci_result = ci[name]
        release = releases.get(name)
        default_branch = remote.default_branch if remote else ci_result.default_branch
        now = time.time()
        unpushed_since = (
            oldest_unpushed_commit_time(local.path)
            if local.valid and local.ahead is not None and local.ahead > 0
            else None
        )
        branch_since = (
            branch_checked_out_time(local.path, local.branch)
            if local.valid and local.branch is not None and local.branch != default_branch
            else None
        )
        branch_days = _age_days(branch_since, now)
        unpushed_days = _age_days(unpushed_since, now)
        prs = pull_requests.get(name)
        requests, prs_error = prs if prs is not None else ([], None)
        remote_sha = remote.sha if remote else None
        row: dict[str, object] = {
            "repository": name,
            "local": _local_label(local, default_branch, branch_days, unpushed_days),
            "stale_local_work": _stale_local_work(branch_days, unpushed_days),
            "branch_checked_out_at": branch_since,
            "oldest_unpushed_commit_at": unpushed_since,
            "branch": local.branch,
            "default_branch": default_branch,
            "default_sha": remote_sha,
            "staged": local.staged,
            "unstaged": local.unstaged,
            "untracked": local.untracked,
            "ahead": local.ahead,
            "behind": local.behind,
            "tracking_ref": local.upstream,
            "tracking_counts_are_last_fetched": True,
            "origin_valid": local.origin_valid,
            "local_error": local.error,
            "dirty": local.dirty,
            "ci": _ci_label(ci_result),
            "ci_state": ci_result.state,
            "ci_sha": ci_result.sha,
            "ci_cached": ci_result.cached,
            "ci_age_seconds": ci_result.age_seconds,
            "ci_error": ci_result.error,
            "workflows": (
                [
                    {"name": workflow.name, "state": workflow.state, "url": workflow.url}
                    for workflow in ci_result.workflows
                ]
                if ci_result.workflows is not None
                else None
            ),
            "todo_items": todo_items,
            "open_prs": None
            if prs is None
            else [
                {
                    "number": request.number,
                    "url": request.url,
                    "mergeable": request.mergeable,
                    "merge_state": request.merge_state,
                }
                for request in requests
            ],
            "prs_error": prs_error,
            "upstream": _upstream_row(upstreams.get(name)),
            "release": _release_row(release),
        }
        rows.append(row)
    return StatusReport(rows, ci)


def _status(args: argparse.Namespace) -> int:
    catalog = common.default_catalog()
    repositories = common.select_repositories(catalog, args.repo, args.tag)
    overrides = common._checkouts(getattr(args, "checkout", None), args.repo, repositories)
    report = collect_status(
        catalog, repositories, overrides, offline=args.offline, refresh=args.refresh
    )
    rows, ci = report.rows, report.ci
    if getattr(args, "dirty", False):
        rows = [row for row in rows if row["dirty"]]
    if args.failed:
        rows = [
            row
            for row in rows
            if row["ci_state"] == "failed"
            or any(
                workflow.state == "failed"
                for workflow in ci[str(row["repository"])].workflows or ()
            )
            or _has_conflicting_pr(row)
            or _upstream_behind(row)
        ]
    if args.json:
        common.print_json(rows)
        return 0
    status_text(rows, ci)
    if args.details:
        visible = {str(row["repository"]) for row in rows}
        for name in sorted(visible, key=str.casefold):
            if ci[name].error is not None:
                print(f"  {name}: CI detail: {ci[name].error}", file=sys.stderr)
            for row in rows:
                if str(row["repository"]) != name:
                    continue
                upstream: Any = row.get("upstream")
                if isinstance(upstream, dict) and isinstance(upstream.get("error"), str):
                    print(f"  {name}: upstream detail: {upstream['error']}", file=sys.stderr)
            for row in rows:
                if str(row["repository"]) != name:
                    continue
                release: Any = row.get("release")
                if isinstance(release, dict) and isinstance(release.get("error"), str):
                    print(f"  {name}: release detail: {release['error']}", file=sys.stderr)
            for check in ci[name].checks:
                if check.state in {"failure", "failed", "error", "timed_out", "action_required", "startup_failure"}:
                    print(f"  {name}: {check.name} {check.state} {check.url or ''}", file=sys.stderr)
            for row in rows:
                if str(row["repository"]) != name:
                    continue
                requests = row.get("open_prs")
                if not isinstance(requests, list):
                    continue
                for request in requests:
                    if (
                        isinstance(request, dict)
                        and request.get("mergeable") == "CONFLICTING"
                    ):
                        print(
                            f"  {name}: PR #{request.get('number')} has merge conflicts "
                            f"{request.get('url') or ''}",
                            file=sys.stderr,
                        )
    return 0
