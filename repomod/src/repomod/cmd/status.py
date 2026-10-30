from __future__ import annotations

import argparse
import os
import re
import shutil
import sys
import time
from collections.abc import Sequence
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any

from repomod import github, policy
from repomod.catalog import CatalogError, repository_path
from repomod.git import branch_checked_out_time, oldest_unpushed_commit_time, status
from repomod.types import Catalog, CiResult, Finding, GitStatus, Repository

from . import common

STALE_LOCAL_WORK_DAYS = 7
_SECONDS_PER_DAY = 86400

_TODO_ITEM = re.compile(r"### [A-Z][A-Z0-9]*-\d+ · P\d+ · .+")


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


def _checkout_status(
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


def _todo_item_count(path: Path) -> int:
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
    path, local = _checkout_status(catalog, repository, checkout)
    todo_items = _todo_item_count(path) if local.valid else 0
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


def _status_text(
    rows: Sequence[dict[str, object]],
    findings_by_repository: dict[str, list[Finding]],
    ci_by_repository: dict[str, CiResult],
) -> None:
    width = max(40, shutil.get_terminal_size((80, 24)).columns)
    for index, row in enumerate(rows):
        if index:
            print()
        name = str(row["repository"])
        local = str(row["local"])
        ci = ci_by_repository[name]
        raw_todo_count = row["todo_items"]
        assert isinstance(raw_todo_count, int)
        todo_count = raw_todo_count
        lint_findings = findings_by_repository.get(name, [])
        lint_color = (
            "31"
            if row["lint_errors"] or row["lint_operational"]
            else "33"
            if row["lint_warnings"]
            else "36"
            if lint_findings
            else "32"
        )
        print(common._paint(name, "36"))
        _print_wrapped_items(
            (
                (f"local {local}", _local_color(local, row.get("stale_local_work") is True)),
                (f"CI {_ci_label(ci)}", _state_color(ci.state)),
                (f"TODO {todo_count}", "33" if todo_count else "90"),
                (f"lints {row['lint']}", lint_color),
            )
        )

        if ci.workflows is None:
            print(f"  workflows {common._paint('unavailable', '33')}")
        elif not ci.workflows:
            print(f"  workflows {common._paint('none found', '90')}")
        else:
            workflow_items = [
                (f"{_workflow_label(workflow.state)} {workflow.name}", _state_color(workflow.state))
                for workflow in ci.workflows
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

        prs = row.get("open_prs")
        if isinstance(prs, list) and prs:
            pr_items = [
                (
                    _pr_label(request),
                    "31"
                    if isinstance(request, dict) and request.get("mergeable") == "CONFLICTING"
                    else "33",
                )
                for request in prs
            ]
            _print_wrapped_items([(f"PRs {pr_items[0][0]}", pr_items[0][1]), *pr_items[1:]])

        upstream: Any = row.get("upstream")
        if isinstance(upstream, dict) and (
            isinstance(upstream.get("parent"), str) or isinstance(upstream.get("error"), str)
        ):
            label, color = _upstream_label(upstream)
            print(f"  upstream {common._paint(label, color)}")

        if lint_findings:
            for finding in lint_findings:
                print(f"    {common._paint(policy.format_finding_short(finding), common._finding_color(finding))}")
    visible_findings = [
        finding
        for row in rows
        for finding in findings_by_repository.get(str(row["repository"]), [])
    ]
    for code, description in policy.lint_legend(visible_findings):
        print(f"{code}: {description}")


def _status(args: argparse.Namespace) -> int:
    catalog = common._catalog()
    repositories = common._select(catalog, args.repo, args.tag)
    checkouts = common._checkouts(getattr(args, "checkout", None), args.repo, repositories)
    with ThreadPoolExecutor(
        max_workers=min(32, max(1, 2 * len(repositories))), thread_name_prefix="repomod-status"
    ) as pool:
        checkout_futures = {}
        findings_futures = {}
        for repository in repositories:
            name = str(repository.id)
            checkout_futures[name] = pool.submit(_status_checkout, catalog, repository, checkouts.get(name))
            findings_futures[name] = pool.submit(
                policy.lint_repositories,
                catalog,
                (repository,),
                local_only=True,
                checkouts=checkouts,
            )
        remotes = {} if args.offline else github.remote_metadata(repositories)
        ci = github.ci_status(repositories, remotes, offline=args.offline, refresh=args.refresh)
        upstreams = github.upstream_status(repositories, remotes, offline=args.offline, refresh=args.refresh)
        pull_requests = {} if args.offline else github.open_pull_requests(repositories)
        findings = [
            finding
            for name in sorted(findings_futures, key=str.casefold)
            for finding in findings_futures[name].result()
        ]
        checkouts = {name: future.result() for name, future in checkout_futures.items()}

    findings_by_repository: dict[str, list[Finding]] = {}
    for finding in findings:
        findings_by_repository.setdefault(finding.repository, []).append(finding)
    rows: list[dict[str, object]] = []
    for repository in repositories:
        name = str(repository.id)
        _, local, todo_items = checkouts[name]
        repository_findings = findings_by_repository.get(name, [])
        lint_errors = sum(
            finding.kind != "operational" and finding.severity == "error"
            for finding in repository_findings
        )
        lint_warnings = sum(
            finding.kind != "operational" and finding.severity == "warning"
            for finding in repository_findings
        )
        lint_operational = sum(finding.kind == "operational" for finding in repository_findings)
        remote = remotes.get(name)
        ci_result = ci[name]
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
            "lint": len(repository_findings),
            "lint_errors": lint_errors,
            "lint_warnings": lint_warnings,
            "lint_operational": lint_operational,
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
        }
        if args.json:
            row["lint_findings"] = [policy.finding_dict(finding) for finding in repository_findings]
        rows.append(row)
    if args.dirty:
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
            or row["lint_errors"] != 0
            or row["lint_operational"] != 0
            or _has_conflicting_pr(row)
            or _upstream_behind(row)
        ]
    if args.json:
        common._print_json(rows)
        return 0
    _status_text(rows, findings_by_repository, ci)
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
