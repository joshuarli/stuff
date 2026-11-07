from __future__ import annotations

import argparse
import shutil
import sys
from collections.abc import Sequence
from concurrent.futures import ThreadPoolExecutor
from typing import Any

from repomod import github
from repomod.types import CiResult, WorkflowStatus

from . import common


def _ci_label(value: CiResult) -> str:
    label = value.state
    if value.cached:
        age = "?" if value.age_seconds is None else f"{value.age_seconds}s"
        source_sha = (value.sha or "unknown")[:12]
        label += f" cached {age} sha {source_sha}"
    return label


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


def _status_row_visible(row: dict[str, object], ci: CiResult) -> bool:
    if _release_surfaced(row):
        return True
    if ci.state not in {"passed", "none"}:
        return True
    if ci.workflows is None:
        return True
    return any(_workflow_surfaced(workflow) for workflow in ci.workflows)


def status_text(
    rows: Sequence[dict[str, object]],
    ci_by_repository: dict[str, CiResult],
) -> None:
    width = max(40, shutil.get_terminal_size((80, 24)).columns)
    visible_rows = [
        row
        for row in rows
        if _status_row_visible(row, ci_by_repository[str(row["repository"])])
    ]
    for index, row in enumerate(visible_rows):
        if index:
            print()
        name = str(row["repository"])
        ci = ci_by_repository[name]
        print(common._paint(name, "36"))
        items = []
        failed_workflows = ci.workflows is not None and any(
            workflow.state == "failed" for workflow in ci.workflows
        )
        if ci.state not in {"passed", "none"} and not (
            ci.state == "failed" and failed_workflows
        ):
            items.append((f"CI {_ci_label(ci)}", _state_color(ci.state)))
        if items:
            _print_wrapped_items(items)

        if ci.workflows is None:
            print(f"  workflows {common._paint('unavailable', '33')}")
        else:
            workflows = [
                workflow for workflow in ci.workflows if _workflow_surfaced(workflow)
            ]
            if workflows:
                workflow_items = [
                    (f"{_workflow_label(workflow.state)} {workflow.name}", _state_color(workflow.state))
                    for workflow in workflows
                ]
                workflow_length = len("  workflows ") + sum(len(item) for item, _ in workflow_items)
                workflow_length += 3 * max(0, len(workflow_items) - 1)
                if workflow_length > width:
                    print("  workflows")
                    _print_wrapped_items(workflow_items, prefix="    ")
                else:
                    _print_wrapped_items(
                        [(f"workflows {workflow_items[0][0]}", workflow_items[0][1]), *workflow_items[1:]]
                    )

        release: Any = row.get("release")
        if _release_surfaced(row):
            assert isinstance(release, dict)
            label, color = _release_label(release)
            print(f"  release {common._paint(label, color)}")


def _status(args: argparse.Namespace) -> int:
    catalog = common.default_catalog()
    repositories = common.select_repositories(catalog, args.repo, args.tag)
    remotes = {} if args.offline else github.remote_metadata(repositories)
    with ThreadPoolExecutor(max_workers=2, thread_name_prefix="repomod-status") as pool:
        ci_future = pool.submit(
            github.ci_status, repositories, remotes, offline=args.offline, refresh=args.refresh
        )
        releases_future = pool.submit(
            github.release_status, repositories, remotes, offline=args.offline, refresh=args.refresh
        )
        ci = ci_future.result()
        releases = releases_future.result()
    rows: list[dict[str, object]] = []
    for repository in repositories:
        name = str(repository.id)
        remote = remotes.get(name)
        ci_result = ci[name]
        release = releases.get(name)
        default_branch = remote.default_branch if remote else ci_result.default_branch
        remote_sha = remote.sha if remote else None
        row: dict[str, object] = {
            "repository": name,
            "default_branch": default_branch,
            "default_sha": remote_sha,
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
            "release": _release_row(release),
        }
        rows.append(row)
    if args.failed:
        rows = [
            row
            for row in rows
            if row["ci_state"] == "failed"
            or any(
                workflow.state == "failed"
                for workflow in ci[str(row["repository"])].workflows or ()
            )
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
                release: Any = row.get("release")
                if isinstance(release, dict) and isinstance(release.get("error"), str):
                    print(f"  {name}: release detail: {release['error']}", file=sys.stderr)
            for check in ci[name].checks:
                if check.state in {"failure", "failed", "error", "timed_out", "action_required", "startup_failure"}:
                    print(f"  {name}: {check.name} {check.state} {check.url or ''}", file=sys.stderr)
    return 0
