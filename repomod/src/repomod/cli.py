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

from repomod.catalog import TAGS, CatalogError, add_repository, find_repository, load_catalog, repository_path
from repomod.fix import FixOutcome, fix_repository, push_fix_branch, push_repository
from repomod.git import GitError, repository_lock, status
from repomod.github import (
    GitHubError,
    ci_status,
    discover,
    ensure_fix_pull_request,
    remote_metadata,
    topics_many,
)
from repomod.policy import (
    PolicyError,
    filter_repositories,
    finding_dict,
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
    if not sys.stdout.isatty() or "NO_COLOR" in os.environ or os.environ.get("TERM") == "dumb":
        return value
    return f"\033[{color}m{value}\033[0m"


def _state_color(state: str) -> str:
    return {
        "passed": "1;32",
        "failed": "1;31",
        "pending": "1;33",
        "cancelled": "1;33",
        "skipped": "90",
        "neutral": "90",
        "none": "90",
        "unknown": "1;33",
        "stale": "1;33",
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
        return "1;31"
    if value.startswith(("dirty", "diverged", "ahead", "behind", "detached")):
        return "1;33"
    return "1;32"


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
            "1;31"
            if row["lint_errors"] or row["lint_operational"]
            else "1;33"
            if row["lint_warnings"]
            else "1;36"
            if lint_findings
            else "1;32"
        )
        print(_paint(name, "1;96"))
        _print_wrapped_items(
            (
                (f"local {local}", _local_color(local)),
                (f"CI {_ci_label(ci)}", _state_color(ci.state)),
                (f"TODO {todo_count}", "1;35" if todo_count else "90"),
                (f"lints {row['lint']}", lint_color),
            )
        )

        if ci.workflows is None:
            print(f"  workflows {_paint('unavailable', '1;33')}")
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

        if lint_findings:
            rule_counts: dict[str, int] = {}
            for finding in lint_findings:
                rule_counts[finding.rule] = rule_counts.get(finding.rule, 0) + 1
            entries = [
                rule + (f" x{count}" if count > 1 else "")
                for rule, count in sorted(rule_counts.items())
            ]
            _print_wrapped_items(
                [(f"lint findings {entries[0]}", lint_color), *((entry, lint_color) for entry in entries[1:])]
            )


def _status(args: argparse.Namespace) -> int:
    catalog = _catalog()
    repositories = _select(catalog, args.repo, args.tag)
    with ThreadPoolExecutor(
        max_workers=min(8, max(1, len(repositories))), thread_name_prefix="repomod-status"
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
            for check in ci[name].checks:
                if check.state in {"failure", "failed", "error", "timed_out", "action_required", "startup_failure"}:
                    print(f"  {name}: {check.name} {check.state} {check.url or ''}", file=sys.stderr)
            for finding in findings_by_repository.get(name, []):
                location = f"{finding.file}: " if finding.file is not None else ""
                print(
                    f"  {name}: [{finding.severity}] [{finding.rule}] {location}{finding.message}",
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
        for finding in findings:
            location = f"{finding.file}: " if finding.file is not None else ""
            print(
                f"{finding.repository} [{finding.severity}] [{finding.rule}] "
                f"{location}{finding.message}"
            )
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
            or remote.archived is not False
        ):
            message = (
                remote.error
                if remote is not None and remote.error is not None
                else "repository is archived or remote default branch is unavailable"
            )
            outcomes.append(FixOutcome(str(repository.id), failures=[("metadata", message)]))
    outcomes.sort(key=lambda item: item.repository.casefold())
    failed = False
    for outcome in outcomes:
        if outcome.branch is not None:
            print(f"{outcome.repository}: branch {outcome.branch}")
        for rule, sha in outcome.commits:
            print(f"{outcome.repository}: committed {rule} {sha[:12]}")
        for rule, message in outcome.failures:
            print(f"{outcome.repository}: failed {rule}: {message}", file=sys.stderr)
            failed = True
        if outcome.skipped is not None:
            print(f"{outcome.repository}: skipped: {outcome.skipped}", file=sys.stderr)
            failed |= bool(args.repo)
        if outcome.recovery_ref is not None:
            repository = next(repo for repo in repositories if str(repo.id) == outcome.repository)
            canonical = repository_path(catalog, repository)
            print(
                f"{outcome.repository}: recover with git -C {shlex.quote(str(canonical))} "
                f"merge --ff-only {shlex.quote(outcome.recovery_ref)}",
                file=sys.stderr,
            )
            print(
                f"{outcome.repository}: after verifying recovery, remove the ref with git -C "
                f"{shlex.quote(str(canonical))} update-ref -d {shlex.quote(outcome.recovery_ref)}",
                file=sys.stderr,
            )
            failed = True
        if outcome.recovery_worktree is not None:
            print(
                f"{outcome.repository}: preserved recovery worktree "
                f"{shlex.quote(outcome.recovery_worktree)}; inspect with "
                f"git -C {shlex.quote(outcome.recovery_worktree)} status and "
                f"git -C {shlex.quote(outcome.recovery_worktree)} log --oneline",
                file=sys.stderr,
            )
            failed = True
        if outcome.cleanup_error is not None:
            print(f"{outcome.repository}: {outcome.cleanup_error}", file=sys.stderr)
            failed = True
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
            return "pushed", pull_request

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

    changed = refresh_pins(allow_major=args.allow_major)
    print("approved pins updated" if changed else "approved pins are current")
    return 0


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
        "--details", action="store_true", help="show individual lint findings and failed CI checks"
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

    pins = commands.add_parser("pins", help="maintain centrally approved GitHub Action pins")
    pin_commands = pins.add_subparsers(dest="pin_operation", required=True)
    refresh = pin_commands.add_parser("refresh")
    refresh.add_argument("--allow-major", action="store_true")
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
        }
        return dispatch[args.command](args)
    except (CatalogError, PolicyError, GitError, GitHubError, OSError, ValueError) as error:
        print(f"repomod: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
