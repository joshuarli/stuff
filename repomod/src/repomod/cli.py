from __future__ import annotations

import argparse
import json
import shlex
import sys
from collections.abc import Sequence
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

from repomod.catalog import TAGS, CatalogError, add_repository, find_repository, load_catalog, repository_path
from repomod.fix import FixOutcome, fix_repository, push_repository
from repomod.git import GitError, repository_lock, status
from repomod.github import GitHubError, ci_status, discover, remote_metadata, topics_many
from repomod.policy import PolicyError, filter_repositories, finding_dict, lint_repositories, selected_rules
from repomod.sync import sync_repositories
from repomod.types import Catalog, CiResult, GitStatus, Repository


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


def _status(args: argparse.Namespace) -> int:
    catalog = _catalog()
    repositories = _select(catalog, args.repo, args.tag)
    if args.offline:
        remotes = {}
    else:
        remotes = remote_metadata(repositories)
    ci = ci_status(repositories, remotes, offline=args.offline, refresh=args.refresh)
    findings = lint_repositories(catalog, repositories, local_only=True)
    lint_counts: dict[str, int] = {}
    lint_errors: dict[str, int] = {}
    lint_warnings: dict[str, int] = {}
    lint_operational: dict[str, int] = {}
    for finding in findings:
        lint_counts[finding.repository] = lint_counts.get(finding.repository, 0) + 1
        if finding.kind == "operational":
            lint_operational[finding.repository] = lint_operational.get(finding.repository, 0) + 1
        elif finding.severity == "error":
            lint_errors[finding.repository] = lint_errors.get(finding.repository, 0) + 1
        elif finding.severity == "warning":
            lint_warnings[finding.repository] = lint_warnings.get(finding.repository, 0) + 1
    rows: list[dict[str, object]] = []
    for repository in repositories:
        name = str(repository.id)
        _, local = _checkout_status(catalog, repository)
        remote = remotes.get(name)
        ci_result = ci[name]
        default_branch = remote.default_branch if remote else ci_result.default_branch
        remote_sha = remote.sha if remote else None
        rows.append(
            {
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
                "lint": lint_counts.get(name, 0),
                "lint_errors": lint_errors.get(name, 0),
                "lint_warnings": lint_warnings.get(name, 0),
                "lint_operational": lint_operational.get(name, 0),
            }
        )
        if args.details:
            for check in ci_result.checks:
                if check.state in {"failure", "failed", "error", "timed_out", "action_required", "startup_failure"}:
                    print(f"  {name}: {check.name} {check.state} {check.url or ''}", file=sys.stderr)
    if args.dirty:
        rows = [row for row in rows if row["dirty"]]
    if args.failed:
        rows = [
            row
            for row in rows
            if row["ci_state"] == "failed"
            or row["lint_errors"] != 0
            or row["lint_operational"] != 0
        ]
    if args.json:
        _print_json(rows)
        return 0
    repository_width = max([10, *(len(str(row["repository"])) for row in rows)])
    print(f"{'REPOSITORY':<{repository_width}}  {'LOCAL':<32}  {'DEFAULT':<18}  {'CI':<32}  LINT")
    for row in rows:
        default = row["default_branch"] or "unknown"
        print(
            f"{row['repository']:<{repository_width}}  {row['local']:<32}  "
            f"{default!s:<18}  "
                    f"{row['ci']:<32}  {row['lint']}"
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
    remotes = {} if args.dry_run else remote_metadata(catalog.repositories)
    results = sync_repositories(catalog, remotes, dry_run=args.dry_run, jobs=args.workers)
    failed = False
    for result in results:
        print(f"{result.repository}: {result.action}: {result.message}")
        failed |= result.failed
    return 1 if failed else 0


def _fix(args: argparse.Namespace) -> int:
    if args.workers < 1:
        raise ValueError("--workers must be at least one")
    catalog = _catalog()
    repositories = _select(catalog, args.repo, args.tag)
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
            futures[pool.submit(
                fix_repository,
                catalog,
                repository,
                fixers,
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
        committed = {
            outcome.repository
            for outcome in outcomes
            if outcome.commits
            and outcome.recovery_ref is None
            and outcome.recovery_worktree is None
        }
        failed |= _push_selected(catalog, tuple(repo for repo in repositories if str(repo.id) in committed)) != 0
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

    status_parser = commands.add_parser("status", help="show local Git, remote CI, and quick lint status")
    _filters(status_parser)
    status_parser.add_argument("--offline", action="store_true")
    status_parser.add_argument("--dirty", action="store_true")
    status_parser.add_argument("--failed", action="store_true")
    status_parser.add_argument("--refresh", action="store_true")
    status_parser.add_argument("--details", action="store_true")
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
    fix.add_argument("--push", action="store_true")

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
