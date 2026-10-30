from __future__ import annotations

import argparse
import shlex
import sys
from concurrent.futures import ThreadPoolExecutor, as_completed

from repomod import catalog as catalog_module
from repomod import fix as fix_engine
from repomod import git, github, policy
from repomod.types import Repository

from . import common


def _fix(args: argparse.Namespace) -> int:
    if args.workers < 1:
        raise ValueError("--workers must be at least one")
    catalog = common._catalog()
    repositories = common._select(catalog, args.repo, args.tag)
    policy.validate_ignored_rules(repositories)
    available = policy.selected_rules(rule_ids=args.rule)
    fixers = tuple(rule for rule in available if rule.fix is not None)
    if not fixers:
        raise policy.PolicyError("selected rules have no automatic fixer")
    remotes = github.remote_metadata(repositories)
    outcomes: list[fix_engine.FixOutcome] = []
    eligible = [
        repository
        for repository in repositories
        if (remote := remotes.get(str(repository.id))) is not None
        and remote.default_branch is not None
        and remote.sha is not None
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
                fix_engine.fix_repository,
                catalog,
                repository,
                repository_fixers,
                remote.default_branch,
                remote.sha,
            )] = repository
        for future in as_completed(futures):
            repository = futures[future]
            try:
                outcomes.append(future.result())
            except Exception as error:
                outcomes.append(fix_engine.FixOutcome(str(repository.id), failures=[("repository", str(error))]))
    for repository in repositories:
        remote = remotes.get(str(repository.id))
        if (
            remote is None
            or remote.error is not None
            or remote.default_branch is None
            or remote.sha is None
            or remote.archived is not False
        ):
            message = (
                remote.error
                if remote is not None and remote.error is not None
                else "GitHub did not report the default branch commit"
                if remote is not None and remote.sha is None
                else "repository is archived or remote default branch is unavailable"
            )
            outcomes.append(fix_engine.FixOutcome(str(repository.id), failures=[("metadata", message)]))
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
            canonical = catalog_module.repository_path(catalog, repository)
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
        common._print_repository_block(outcome.repository, stdout_lines, stderr_lines)
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
        with git.repository_lock(catalog.root, repository.id):
            outcome = branches[str(repository.id)]
            remote = remotes[str(repository.id)]
            assert remote.default_branch is not None
            assert outcome.branch is not None
            assert outcome.base_head is not None
            assert outcome.branch_head is not None
            state, message = fix_engine.push_fix_branch(
                repository,
                catalog_module.repository_path(catalog, repository),
                outcome.branch,
                remote.default_branch,
                outcome.base_head,
                outcome.branch_head,
            )
        if state != "pushed":
            return state, message
        try:
            pull_request = github.ensure_fix_pull_request(
                repository.id,
                outcome.branch,
                remote.default_branch,
            )
        except Exception as error:
            return "pr-failed", str(error)
        match pull_request.auto_merge:
            case github.AutoMergeEnabled():
                return "pushed", f"{pull_request.url}; auto-merge enabled"
            case github.AutoMergeSkipped(reason):
                return "pushed", f"{pull_request.url}; auto-merge skipped: {reason}"
            case github.AutoMergeFailed(error):
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
