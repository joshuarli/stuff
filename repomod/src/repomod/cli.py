from __future__ import annotations

import argparse
import json
import os
import re
import shlex
import shutil
import sys
from collections.abc import Sequence
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
from typing import Any

from repomod.catalog import TAGS, CatalogError, add_repository, find_repository, load_catalog, repository_path
from repomod.clean import CleanResult, clean_repositories
from repomod.fix import FixOutcome, fix_repository, push_fix_branch, push_repository
from repomod.git import GitError, repository_lock, status
from repomod.github import (
    AutoMergeEnabled,
    AutoMergeFailed,
    AutoMergeSkipped,
    GitHubError,
    UpstreamStatus,
    ci_status,
    discover,
    ensure_fix_pull_request,
    open_pull_requests,
    remote_metadata,
    topics_many,
    upstream_status,
)
from repomod.policy import (
    PolicyError,
    filter_repositories,
    finding_dict,
    format_finding_short,
    lint_legend,
    lint_repositories,
    selected_rules,
    validate_ignored_rules,
)
from repomod.sync import sync_repositories
from repomod.types import Catalog, CiResult, Finding, GitStatus, Repository

_TODO_ITEM = re.compile(r"### [A-Z][A-Z0-9]*-\d+ · P\d+ · .+")


def _catalog_path() -> Path:
    return Path(__file__).resolve().parents[2] / "catalog.toml"


def _catalog() -> Catalog:
    return load_catalog(_catalog_path())


def _tags(values: Sequence[str]) -> tuple[str, ...]:
    unknown = set(values) - TAGS
    if unknown:
        raise CatalogError(f"unknown tags: {', '.join(sorted(unknown))}")
    if len(values) != len(set(values)):
        raise CatalogError("duplicate tags are not allowed")
    return tuple(values)


def _select(
    catalog: Catalog, repo_values: Sequence[str], tags: Sequence[str]
) -> tuple[Repository, ...]:
    selected_tags = _tags(tags)
    if repo_values:
        repositories = tuple(find_repository(catalog, value) for value in repo_values)
        return filter_repositories(repositories, tags=selected_tags)
    return filter_repositories(catalog.repositories, tags=selected_tags)


def _print_json(value: object) -> None:
    print(json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")))


def _local_label(value: GitStatus, default_branch: str | None) -> str:
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
    if value.branch is not None and value.branch != default_branch:
        return f"{state} on {value.branch}"
    return state


def _ci_label(value: CiResult) -> str:
    label = value.state
    if value.cached:
        age = "?" if value.age_seconds is None else f"{value.age_seconds}s"
        source_sha = (value.sha or "unknown")[:12]
        label += f" cached {age} sha {source_sha}"
    return label


def _checkout_status(catalog: Catalog, repository: Repository) -> tuple[Path, GitStatus]:
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


def _status_checkout(catalog: Catalog, repository: Repository) -> tuple[Path, GitStatus, int]:
    path, local = _checkout_status(catalog, repository)
    todo_items = _todo_item_count(path) if local.valid else 0
    return path, local, todo_items


def _paint(value: str, color: str) -> str:
    if (
        not sys.stdout.isatty()
        or os.environ.get("TERM") == "dumb"
        or "NO_COLOR" in os.environ
        or os.environ.get("CLICOLOR") == "0"
    ):
        return value
    return f"\033[{color}m{value}\033[0m"


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


def _local_color(value: str) -> str:
    if value.startswith(("missing", "invalid")):
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
            line = prefix + _paint(label, color)
            length = len(prefix) + len(label)
        else:
            line += separator + _paint(label, color)
            length += len(separator) + len(label)
    if length > len(prefix):
        print(line)


def _finding_color(finding: Finding) -> str:
    if finding.kind == "operational" or finding.severity == "error":
        return "31"
    if finding.severity == "warning":
        return "33"
    return "36"


def _print_repository_block(
    name: str, stdout_lines: Sequence[str], stderr_lines: Sequence[str]
) -> None:
    if not stdout_lines and not stderr_lines:
        return
    header = _paint(name, "36")
    if stdout_lines:
        print(header)
    else:
        print(header, file=sys.stderr)
    for line in stdout_lines:
        print(f"    {line}")
    for line in stderr_lines:
        print(f"    {line}", file=sys.stderr)


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


def _upstream_row(upstream: UpstreamStatus | None) -> dict[str, object] | None:
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
        print(_paint(name, "36"))
        _print_wrapped_items(
            (
                (f"local {local}", _local_color(local)),
                (f"CI {_ci_label(ci)}", _state_color(ci.state)),
                (f"TODO {todo_count}", "33" if todo_count else "90"),
                (f"lints {row['lint']}", lint_color),
            )
        )

        if ci.workflows is None:
            print(f"  workflows {_paint('unavailable', '33')}")
        elif not ci.workflows:
            print(f"  workflows {_paint('none found', '90')}")
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
            print(f"  upstream {_paint(label, color)}")

        if lint_findings:
            for finding in lint_findings:
                print(f"    {_paint(format_finding_short(finding), _finding_color(finding))}")
    visible_findings = [
        finding
        for row in rows
        for finding in findings_by_repository.get(str(row["repository"]), [])
    ]
    for code, description in lint_legend(visible_findings):
        print(f"{code}: {description}")


def _status(args: argparse.Namespace) -> int:
    catalog = _catalog()
    repositories = _select(catalog, args.repo, args.tag)
    with ThreadPoolExecutor(
        max_workers=min(32, max(1, 2 * len(repositories))), thread_name_prefix="repomod-status"
    ) as pool:
        checkout_futures = {}
        findings_futures = {}
        for repository in repositories:
            name = str(repository.id)
            checkout_futures[name] = pool.submit(_status_checkout, catalog, repository)
            findings_futures[name] = pool.submit(
                lint_repositories,
                catalog,
                (repository,),
                local_only=True,
            )
        remotes = {} if args.offline else remote_metadata(repositories)
        ci = ci_status(repositories, remotes, offline=args.offline, refresh=args.refresh)
        upstreams = upstream_status(repositories, remotes, offline=args.offline, refresh=args.refresh)
        pull_requests = {} if args.offline else open_pull_requests(repositories)
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
        prs = pull_requests.get(name)
        requests, prs_error = prs if prs is not None else ([], None)
        default_branch = remote.default_branch if remote else ci_result.default_branch
        remote_sha = remote.sha if remote else None
        row: dict[str, object] = {
            "repository": name,
            "local": _local_label(local, default_branch),
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
            row["lint_findings"] = [finding_dict(finding) for finding in repository_findings]
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
        _print_json(rows)
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


def _lint(args: argparse.Namespace) -> int:
    catalog = _catalog()
    repositories = _select(catalog, args.repo, args.tag)
    findings = lint_repositories(catalog, repositories, tags=_tags(args.tag), rule_ids=args.rule)
    if args.json:
        _print_json([finding_dict(finding) for finding in findings])
    else:
        current: str | None = None
        for finding in findings:
            if finding.repository != current:
                if current is not None:
                    print()
                print(_paint(finding.repository, "36"))
                current = finding.repository
            print(f"    {_paint(format_finding_short(finding), _finding_color(finding))}")
        for code, description in lint_legend(findings):
            print(f"{code}: {description}")
        if not findings:
            print("no policy findings")
    if any(finding.kind == "operational" for finding in findings):
        return 2
    has_errors = any(
        finding.kind == "violation" and finding.severity == "error"
        for finding in findings
    )
    has_warnings = any(finding.severity == "warning" for finding in findings)
    return 1 if has_errors or (has_warnings and getattr(args, "strict", False)) else 0


def _rules(args: argparse.Namespace) -> int:
    for rule in selected_rules():
        tags = rule.applies_to.describe()
        action = "fixable" if rule.fix is not None else "lint only"
        print(f"{rule.id:<18} {tags:<48} {action:<10} {rule.description}")
    return 0


def _discover(args: argparse.Namespace) -> int:
    catalog = _catalog()
    owners = tuple(dict.fromkeys((catalog.personal_owner, "laputa-systems")))
    names = discover(owners)
    catalogued = {str(repo.id).casefold() for repo in catalog.repositories}
    for name in names:
        if name.casefold() not in catalogued:
            print(f"{name}  not catalogued")
    if not any(name.casefold() not in catalogued for name in names):
        print("no uncatalogued active repositories")
    return 0


def _add(args: argparse.Namespace) -> int:
    catalog = _catalog()
    parts = args.repository.split("/")
    if len(parts) != 2:
        raise CatalogError("add expects OWNER/REPO")
    changed = add_repository(catalog, parts[0], parts[1], list(_tags(args.tag)))
    print(f"{'added' if changed else 'already catalogued'} {args.repository}")
    return 0


def _list(args: argparse.Namespace) -> int:
    catalog = _catalog()
    for repository in catalog.repositories:
        path, local = _checkout_status(catalog, repository)
        presence = "missing" if not local.present else "valid" if local.valid else "invalid"
        tags = ",".join(repository.tags)
        suffix = f"; autofix disabled: {repository.autofix_reason}" if not repository.autofix else ""
        print(f"{repository.id}\t{tags}\t{presence}\t{path}{suffix}")
    return 0


def _path(args: argparse.Namespace) -> int:
    catalog = _catalog()
    repository = find_repository(catalog, args.repository)
    print(repository_path(catalog, repository))
    return 0


def _sync(args: argparse.Namespace) -> int:
    catalog = _catalog()
    repositories = catalog.repositories
    if args.dry_run:
        remotes = {}
    else:
        print(
            f"Fetching GitHub metadata for {len(repositories)} repositories...",
            file=sys.stderr,
            flush=True,
        )
        remotes = remote_metadata(
            repositories,
            on_batch_complete=lambda completed, total: _sync_progress(
                "GitHub metadata batches", completed, total
            ),
        )
    print(
        f"Syncing {len(repositories)} repositories...",
        file=sys.stderr,
        flush=True,
    )
    results = sync_repositories(
        catalog,
        remotes,
        dry_run=args.dry_run,
        jobs=args.workers,
        on_complete=lambda completed, total, result: _sync_progress(
            "Repository progress", completed, total, f"{result.repository}: {result.action}"
        ),
    )
    failed = False
    for result in results:
        print(f"{result.repository}: {result.action}: {result.message}")
        failed |= result.failed
    return 1 if failed else 0


def _sync_progress(stage: str, completed: int, total: int, detail: str = "") -> None:
    message = f"{stage}: {completed}/{total} complete"
    if detail:
        message += f" ({detail})"
    if sys.stderr.isatty():
        print(f"\r{message}\033[K", end="", file=sys.stderr, flush=True)
        if completed == total:
            print(file=sys.stderr)
    else:
        print(message, file=sys.stderr, flush=True)


def _fix(args: argparse.Namespace) -> int:
    if args.workers < 1:
        raise ValueError("--workers must be at least one")
    catalog = _catalog()
    repositories = _select(catalog, args.repo, args.tag)
    validate_ignored_rules(repositories)
    available = selected_rules(rule_ids=args.rule)
    fixers = tuple(rule for rule in available if rule.fix is not None)
    if not fixers:
        raise PolicyError("selected rules have no automatic fixer")
    remotes = remote_metadata(repositories)
    outcomes: list[FixOutcome] = []
    eligible = [
        repository
        for repository in repositories
        if (remote := remotes.get(str(repository.id))) is not None
        and remote.default_branch is not None
        and (not args.push or remote.sha is not None)
        and remote.error is None
        and remote.archived is False
    ]
    with ThreadPoolExecutor(max_workers=min(args.workers, 16, max(1, len(eligible)))) as pool:
        futures = {}
        for repository in eligible:
            remote = remotes[str(repository.id)]
            if remote.default_branch is None:
                raise RuntimeError(f"{repository.id} lost its default branch metadata")
            repository_fixers = tuple(
                rule for rule in fixers if rule.id not in repository.ignored_rules
            )
            futures[pool.submit(
                fix_repository,
                catalog,
                repository,
                repository_fixers,
                remote.default_branch,
                remote.sha if args.push else None,
            )] = repository
        for future in as_completed(futures):
            repository = futures[future]
            try:
                outcomes.append(future.result())
            except Exception as error:
                outcomes.append(FixOutcome(str(repository.id), failures=[("repository", str(error))]))
    for repository in repositories:
        remote = remotes.get(str(repository.id))
        if (
            remote is None
            or remote.error is not None
            or remote.default_branch is None
            or (args.push and remote.sha is None)
            or remote.archived is not False
        ):
            message = (
                remote.error
                if remote is not None and remote.error is not None
                else "GitHub did not report the default branch commit"
                if args.push and remote is not None and remote.sha is None
                else "repository is archived or remote default branch is unavailable"
            )
            outcomes.append(FixOutcome(str(repository.id), failures=[("metadata", message)]))
    outcomes.sort(key=lambda item: item.repository.casefold())
    failed = False
    for outcome in outcomes:
        stdout_lines: list[str] = []
        stderr_lines: list[str] = []
        if outcome.branch is not None:
            stdout_lines.append(f"branch {outcome.branch}")
        for rule, sha in outcome.commits:
            stdout_lines.append(f"committed {rule} {sha[:12]}")
        for rule, message in outcome.failures:
            stderr_lines.append(f"failed {rule}: {message}")
            failed = True
        if outcome.skipped is not None:
            stderr_lines.append(f"skipped: {outcome.skipped}")
            failed |= bool(args.repo)
        if outcome.recovery_ref is not None:
            repository = next(repo for repo in repositories if str(repo.id) == outcome.repository)
            canonical = repository_path(catalog, repository)
            stderr_lines.append(
                f"recover with git -C {shlex.quote(str(canonical))} "
                f"merge --ff-only {shlex.quote(outcome.recovery_ref)}"
            )
            stderr_lines.append(
                f"after verifying recovery, remove the ref with git -C "
                f"{shlex.quote(str(canonical))} update-ref -d {shlex.quote(outcome.recovery_ref)}"
            )
            failed = True
        if outcome.recovery_worktree is not None:
            stderr_lines.append(
                f"preserved recovery worktree "
                f"{shlex.quote(outcome.recovery_worktree)}; inspect with "
                f"git -C {shlex.quote(outcome.recovery_worktree)} status and "
                f"git -C {shlex.quote(outcome.recovery_worktree)} log --oneline"
            )
            failed = True
        if outcome.cleanup_error is not None:
            stderr_lines.append(outcome.cleanup_error)
            failed = True
        _print_repository_block(outcome.repository, stdout_lines, stderr_lines)
    if args.push:
        branches = {
            outcome.repository: outcome
            for outcome in outcomes
            if outcome.branch is not None
            and outcome.base_head is not None
            and outcome.branch_head is not None
            and outcome.branch_head != outcome.base_head
            and outcome.recovery_worktree is None
        }

        def push_fix(repository: Repository) -> tuple[str, str | None]:
            with repository_lock(catalog.root, repository.id):
                outcome = branches[str(repository.id)]
                remote = remotes[str(repository.id)]
                assert remote.default_branch is not None
                assert outcome.branch is not None
                assert outcome.base_head is not None
                assert outcome.branch_head is not None
                state, message = push_fix_branch(
                    repository,
                    repository_path(catalog, repository),
                    outcome.branch,
                    remote.default_branch,
                    outcome.base_head,
                    outcome.branch_head,
                )
            if state != "pushed":
                return state, message
            try:
                pull_request = ensure_fix_pull_request(
                    repository.id,
                    outcome.branch,
                    remote.default_branch,
                )
            except Exception as error:
                return "pr-failed", str(error)
            match pull_request.auto_merge:
                case AutoMergeEnabled():
                    return "pushed", f"{pull_request.url}; auto-merge enabled"
                case AutoMergeSkipped(reason):
                    return "pushed", f"{pull_request.url}; auto-merge skipped: {reason}"
                case AutoMergeFailed(error):
                    return "pr-automerge-failed", f"{pull_request.url}; auto-merge failed: {error}"

        with ThreadPoolExecutor(max_workers=min(4, max(1, len(branches)))) as pool:
            futures = {
                pool.submit(push_fix, repository): repository
                for repository in repositories
                if str(repository.id) in branches
            }
            push_results = []
            for future, repository in futures.items():
                try:
                    push_results.append((repository, future.result()))
                except Exception as error:
                    push_results.append((repository, ("failed", str(error))))
        for repository, (state, message) in sorted(
            push_results, key=lambda item: str(item[0].id).casefold()
        ):
            if state == "pushed":
                print(f"{repository.id}: pushed; PR {message}")
            elif state == "pr-automerge-failed":
                print(f"{repository.id}: pushed; PR {message}", file=sys.stderr)
                failed = True
            elif state == "pr-failed":
                print(f"{repository.id}: pushed; PR failed: {message}", file=sys.stderr)
                failed = True
            else:
                print(f"{repository.id}: {state}" + (f": {message}" if message is not None else ""))
                failed |= state == "failed"
    return 1 if failed else 0


def _push_selected(catalog: Catalog, repositories: tuple[Repository, ...]) -> int:
    if not repositories:
        return 0
    failed = False
    def push(repository: Repository) -> tuple[str, str | None]:
        with repository_lock(catalog.root, repository.id):
            return push_repository(repository, repository_path(catalog, repository))

    with ThreadPoolExecutor(max_workers=min(4, len(repositories))) as pool:
        futures = {pool.submit(push, repository): repository for repository in repositories}
        results = []
        for future, repository in futures.items():
            try:
                results.append((repository, future.result()))
            except Exception as error:
                results.append((repository, ("failed", str(error))))
    for repository, (state, message) in sorted(results, key=lambda item: str(item[0].id).casefold()):
        print(f"{repository.id}: {state}" + (f": {message}" if message is not None else ""))
        failed |= state == "failed"
    return 1 if failed else 0


def _push(args: argparse.Namespace) -> int:
    catalog = _catalog()
    repositories = _select(catalog, args.repo, args.tag)
    return _push_selected(catalog, repositories)


def _count(value: int, noun: str) -> str:
    return f"{value} {noun}" if value == 1 else f"{value} {noun}s"


def _confirm(prompt: str) -> bool:
    if not sys.stdin.isatty():
        raise ValueError("confirmation requires a terminal; pass --yes to remove without prompting")
    print(prompt, end="", flush=True)
    return input().strip().casefold() in {"y", "yes"}


def _print_clean_results(results: Sequence[CleanResult]) -> None:
    for result in results:
        if result.action == "absent":
            continue
        detail = f"{result.message}" if result.path is None else f"{result.path}: {result.message}"
        print(
            f"{result.repository}: {result.action}: {detail}",
            file=sys.stderr if result.failed else sys.stdout,
        )


def _clean(args: argparse.Namespace) -> int:
    if args.workers < 1:
        raise ValueError("--workers must be at least one")
    catalog = _catalog()
    repositories = _select(catalog, args.repo, args.tag)
    plan = clean_repositories(
        catalog, repositories, dry_run=True, force=args.force, jobs=args.workers
    )
    _print_clean_results(plan)
    failed = any(result.failed for result in plan)
    planned = {result.repository for result in plan if result.action == "planned"}
    if not planned:
        print("no checkouts to remove")
        return 1 if failed else 0
    if args.dry_run:
        print(f"dry run: would remove {_count(len(planned), 'checkout')}")
        return 1 if failed else 0
    if not args.yes and not _confirm(
        f"Remove {_count(len(planned), 'catalogued checkout')} from {catalog.root}? [y/N] "
    ):
        print("aborted; nothing removed", file=sys.stderr)
        return 1 if failed else 0
    results = clean_repositories(
        catalog,
        tuple(repository for repository in repositories if str(repository.id) in planned),
        dry_run=False,
        force=args.force,
        jobs=args.workers,
    )
    _print_clean_results(results)
    failed |= any(result.failed for result in results)
    removed = sum(result.action == "removed" for result in results)
    print(f"removed {_count(removed, 'checkout')}")
    return 1 if failed else 0


def _topics(args: argparse.Namespace) -> int:
    catalog = _catalog()
    repositories = _select(catalog, args.repo, args.tag)
    results = topics_many(repositories, apply=args.operation == "apply", jobs=args.workers)
    failed = False
    for name, changed, additions, removals, error in results:
        if error is not None:
            print(f"{name}: failed: {error}", file=sys.stderr)
            failed = True
            continue
        if not additions and not removals:
            print(f"{name}: already compliant")
            continue
        changes = []
        if additions:
            changes.append("add " + ", ".join(additions))
        if removals:
            changes.append("remove " + ", ".join(removals))
        print(f"{name}: {'applied' if changed else 'diff'}: {'; '.join(changes)}")
    return 1 if failed else 0


def _pins(args: argparse.Namespace) -> int:
    from repomod.rules.actions import refresh_pins
    from repomod.rules.python_standard import refresh_python_tool_versions
    from repomod.rules.typescript_standard import refresh_typescript_tool_pins

    updated = []
    failures = []
    try:
        if refresh_pins():
            updated.append("GitHub Actions")
    except (GitHubError, OSError, ValueError) as error:
        failures.append(f"GitHub Actions: {error}")
    try:
        if refresh_python_tool_versions():
            updated.append("Python tools")
    except (OSError, ValueError) as error:
        failures.append(f"Python tools: {error}")
    try:
        if refresh_typescript_tool_pins():
            updated.append("TypeScript tools")
    except (GitHubError, OSError, ValueError) as error:
        failures.append(f"TypeScript tools: {error}")
    if failures:
        raise ValueError("pin refresh failed: " + "; ".join(failures))
    print("approved pins updated: " + ", ".join(updated) if updated else "approved pins are current")
    return 0


def _audit(args: argparse.Namespace) -> int:
    from repomod.audit import audit_repositories
    from repomod.audit import fix_repository as audit_fix_repository

    catalog = _catalog()
    repositories = _select(catalog, args.repo, args.tag)
    validate_ignored_rules(repositories)
    if args.fix:
        from concurrent.futures import ThreadPoolExecutor as AuditPool

        with AuditPool(max_workers=min(args.workers, 16, max(1, len(repositories)))) as pool:
            fixes = list(
                pool.map(
                    lambda repository: audit_fix_repository(catalog, repository),
                    repositories,
                )
            )
        for outcome in sorted(fixes, key=lambda item: item.repository.casefold()):
            _print_repository_block(
                outcome.repository,
                [f"fixed: {command}" for command in outcome.fixed],
                [f"fix failed: {error}" for error in outcome.fix_errors],
            )
        if any(outcome.fix_errors for outcome in fixes):
            return 2
    outcomes = audit_repositories(
        catalog,
        tuple(repositories),
        offline=args.offline,
        refresh=args.refresh,
        jobs=args.workers,
        upstream_statuses=upstream_status(repositories, offline=args.offline, refresh=args.refresh),
    )
    findings = [finding for outcome in outcomes for finding in outcome.findings]
    if args.json:
        _print_json(
            [
                {
                    "repository": outcome.repository,
                    "findings": [finding_dict(finding) for finding in outcome.findings],
                    "fixed": outcome.fixed,
                    "fix_errors": outcome.fix_errors,
                }
                for outcome in outcomes
            ]
        )
    else:
        shown = False
        for outcome in outcomes:
            if not outcome.findings:
                continue
            if shown:
                print()
            print(_paint(outcome.repository, "36"))
            for finding in outcome.findings:
                print(f"    {_paint(format_finding_short(finding), _finding_color(finding))}")
            shown = True
        if not findings:
            print("no dependency audit findings")
    if any(finding.kind == "operational" for finding in findings):
        return 2
    return 1 if findings else 0


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="repomod", description="Local-first repository fleet manager")
    commands = parser.add_subparsers(dest="command", required=True)

    commands.add_parser("discover", help="show active owned repositories missing from the catalog")

    add = commands.add_parser("add", help="add a repository and its policy tags to the catalog")
    add.add_argument("repository")
    add.add_argument("--tag", action="append", required=True)

    commands.add_parser("list", help="list catalogued repositories and local checkout presence")

    path = commands.add_parser("path", help="print a repository's canonical checkout path")
    path.add_argument("repository")

    sync = commands.add_parser("sync", help="clone or safely fast-forward catalogued checkouts")
    sync.add_argument("--dry-run", action="store_true")
    sync.add_argument("--workers", type=int, default=4)

    status_parser = commands.add_parser("status", help="show local Git, remote CI, TODO, and lint status")
    _filters(status_parser)
    status_parser.add_argument("--offline", action="store_true")
    status_parser.add_argument("--dirty", action="store_true")
    status_parser.add_argument("--failed", action="store_true")
    status_parser.add_argument("--refresh", action="store_true")
    status_parser.add_argument(
        "--details", action="store_true", help="show failed CI checks"
    )
    status_parser.add_argument("--json", action="store_true")

    lint = commands.add_parser("lint", help="check declarative repository policies")
    _filters(lint)
    lint.add_argument("--rule", action="append", default=[])
    lint.add_argument("--json", action="store_true")
    lint.add_argument("--strict", action="store_true", help="treat warnings as violations")

    commands.add_parser("rules", help="list policy rules")

    fix = commands.add_parser("fix", help="repair policy violations with one commit per fixer")
    _filters(fix)
    fix.add_argument("--rule", action="append", default=[])
    fix.add_argument("--workers", type=int, default=4)
    fix.add_argument(
        "--push",
        action="store_true",
        help="push fix branches and open or reuse pull requests",
    )

    push = commands.add_parser("push", help="push eligible local commits without force")
    _filters(push)

    topics = commands.add_parser("topics", help="compare or replace GitHub repository topics")
    topic_commands = topics.add_subparsers(dest="operation", required=True)
    for operation in ("diff", "apply"):
        topic = topic_commands.add_parser(operation)
        _filters(topic)
        topic.add_argument("--workers", type=int, default=4)

    pins = commands.add_parser(
        "pins",
        help="maintain centrally approved GitHub Action, Python, and TypeScript tool pins",
    )
    pin_commands = pins.add_subparsers(dest="pin_operation", required=True)
    pin_commands.add_parser("refresh")

    audit = commands.add_parser("audit", help="audit registry dependencies for staleness, vulns, and bans")
    _filters(audit)
    audit.add_argument("--json", action="store_true")
    audit.add_argument("--offline", action="store_true", help="use cached registry and OSV data only")
    audit.add_argument("--refresh", action="store_true", help="ignore cached registry and OSV data")
    audit.add_argument("--fix", action="store_true", help="run native upgraders (uv, pnpm, cargo, go) when available")
    audit.add_argument("--workers", type=int, default=4)
    return parser


def _filters(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--repo", action="append", default=[])
    parser.add_argument("--tag", action="append", default=[])


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        dispatch = {
            "discover": _discover,
            "add": _add,
            "list": _list,
            "path": _path,
            "sync": _sync,
            "status": _status,
            "lint": _lint,
            "rules": _rules,
            "fix": _fix,
            "push": _push,
            "topics": _topics,
            "pins": _pins,
            "audit": _audit,
        }
        return dispatch[args.command](args)
    except (CatalogError, PolicyError, GitError, GitHubError, OSError, ValueError) as error:
        print(f"repomod: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
