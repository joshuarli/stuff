#!/usr/bin/env python3
"""Execute a fix across the configured GitHub repos.

Each fix is a directory containing:
    config.yaml  — branch, pr_title, pr_body, reviewers, sparse_paths, etc.
    main.py      — accept_repo(repo, config) and run(repo_path, config)

Usage:
    repomod pin-gha                                # dry-run
    repomod pin-gha --apply                        # push & PR
    repomod pin-gha --repos acme/backend,acme/web
    repomod pin-gha --plan                         # show what would run
    repomod pin-gha --status                       # show status summary
    repomod pin-gha --prs                          # PR states on GitHub
    repomod pin-gha --diffs                        # show the stored dry-run diffs
    repomod pin-gha --undraft --apply              # mark its draft PRs ready
    repomod sync                                   # refresh repos.yaml only
"""

import argparse
import ast
import hashlib
import importlib.util
import json
import logging
import os
import shutil
import subprocess
import sys
import time
import traceback
from collections.abc import Callable, Sequence
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
from typing import cast

import yaml

from repomod.git import (
    DefaultBranchError,
    InvalidBranchError,
    apply_patch,
    checkout_branch,
    clone,
    commit_and_push,
    format_error,
    get_diff,
    has_changes,
    head_sha,
    mark_ready,
    pr_by_repo,
    pr_states,
    refuse_default_branch,
    require_branch_name,
    resolve_pr_node_ids,
    search_prs,
    stage_worktree,
    unstage,
)
from repomod.lint import validate_changes
from repomod.status import (
    atomic_write_yaml,
    load_status,
    merge_status,
    print_full,
    print_rollout,
    print_summary,
    publish_reviewed,
    set_status,
    stored_patch,
    write_log,
)
from repomod.sync import sync_repos
from repomod.types import (
    FixConfig,
    FixModule,
    Repo,
    RolloutPR,
    RunContext,
    Status,
    WorkItem,
)
from repomod.ui import (
    BOLD,
    DIM,
    GREEN,
    RED,
    RST,
    YELLOW,
    ProgressDisplay,
    format_diff_box,
    spinner,
)
from repomod.utils import SUBPROCESS_TIMEOUT, ShutdownError, interrupt

HERE = Path(__file__).parent
FIXES_DIR = HERE / "fixes"
PROJECT_ROOT = HERE.parent.parent
REPOS_FILE = PROJECT_ROOT / "repos.yaml"
EXCLUDED_FILE_NAME = "repos-excluded.yaml"
DISABLED_FILE = PROJECT_ROOT / "repos-disabled.yaml"

REQUIRED_FUNCS = ("accept_repo", "run")
DEFAULT_WORKERS = min(8, (os.cpu_count() or 4) + 4)  # tuned for I/O-bound work
SYNC_MAX_AGE = 24 * 3600  # seconds — re-sync repos.yaml if older than this


def legacy_fix_hash(fix_dir: Path) -> str:
    """Raw bytes of main.py and config.yaml.

    Accepted only when the fix imports nothing from its directory or the
    repomod package. Those modules are absent here, so a change to one of
    them would leave a saved result looking current.
    """
    h = hashlib.sha256()
    for name in ("main.py", "config.yaml"):
        p = fix_dir / name
        if p.exists():
            h.update(p.read_bytes())
    return h.hexdigest()[:12]


def _in_fix_or_package(path: Path, fix_dir: Path) -> bool:
    resolved = path.resolve()
    for root in (fix_dir.resolve(), HERE.resolve()):
        if resolved == root or root in resolved.parents:
            return True
    return False


def _module_file(root: Path, parts: tuple[str, ...]) -> Path | None:
    if not parts:
        return None
    rel = Path(*parts)
    module = root / rel.with_suffix(".py")
    if module.is_file():
        return module
    package = root / rel / "__init__.py"
    if package.is_file():
        return package
    return None


def _scoped_files(root: Path, parts: tuple[str, ...], fix_dir: Path) -> list[Path]:
    found: list[Path] = []
    for end in range(1, len(parts) + 1):
        path = _module_file(root, parts[:end])
        if path is not None and _in_fix_or_package(path, fix_dir):
            found.append(path)
    return found


def _absolute_files(name: str, fix_dir: Path) -> list[Path]:
    parts = tuple(name.split("."))
    found: list[Path] = []
    for root in (fix_dir, HERE.parent):
        found.extend(_scoped_files(root, parts, fix_dir))
    return found


def _import_from(path: Path, node: ast.ImportFrom, fix_dir: Path) -> list[Path]:
    names = [alias.name for alias in node.names if alias.name != "*"]
    if node.level:
        base = path.parent
        for _level in range(node.level - 1):
            base = base.parent
        parts = tuple(node.module.split(".")) if node.module is not None else ()
        found = _scoped_files(base, parts, fix_dir)
        for name in names:
            found.extend(_scoped_files(base, (*parts, name), fix_dir))
        return found
    if node.module is None:
        return []
    found = _absolute_files(node.module, fix_dir)
    for name in names:
        found.extend(_absolute_files(f"{node.module}.{name}", fix_dir))
    return found


def _imports_in(path: Path, fix_dir: Path) -> list[Path]:
    try:
        tree = ast.parse(path.read_bytes())
    except SyntaxError:
        return []
    found: list[Path] = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                found.extend(_absolute_files(alias.name, fix_dir))
        elif isinstance(node, ast.ImportFrom):
            found.extend(_import_from(path, node, fix_dir))
    return found


def _imported_sources(fix_dir: Path) -> tuple[Path, ...]:
    entry = (fix_dir / "main.py").resolve()
    if not entry.is_file():
        return ()
    seen: set[Path] = set()
    found: list[Path] = []

    def add(path: Path) -> None:
        resolved = path.resolve()
        if resolved in seen or resolved == entry:
            return
        if not _in_fix_or_package(resolved, fix_dir):
            return
        seen.add(resolved)
        found.append(resolved)
        for child in _imports_in(resolved, fix_dir):
            add(child)

    for path in _imports_in(entry, fix_dir):
        add(path)

    def sort_key(path: Path) -> tuple[int, str]:
        fix_root = fix_dir.resolve()
        if fix_root in path.parents:
            return (0, path.relative_to(fix_root).as_posix())
        return (1, path.relative_to(HERE.resolve()).as_posix())

    return tuple(sorted(found, key=sort_key))


def _accepted_hashes(fix_dir: Path, fix_hash: str) -> frozenset[str]:
    """Hashes that still describe this fix.

    The raw-byte hash is included only when it cannot hide an import. The
    import-blind hash is not current: a utils change would not re-run.
    """
    if _imported_sources(fix_dir):
        return frozenset({fix_hash})
    return frozenset({fix_hash, legacy_fix_hash(fix_dir)})


def compute_fix_hash(fix_dir: Path) -> str:
    """Hash what a fix does.

    main.py, the config without the skip list, and modules imported from
    the fix directory or the repomod package, including what those import.
    The skip list only chooses which repos run. Comments and formatting
    say nothing about behavior. The standard library and third-party
    packages are not hashed.
    """
    h = hashlib.sha256()
    main_py = fix_dir / "main.py"
    if main_py.exists():
        h.update(main_py.read_bytes())
    config_path = fix_dir / "config.yaml"
    if config_path.exists():
        try:
            raw = yaml.safe_load(config_path.read_text())
        except yaml.YAMLError:
            raw = None
        if isinstance(raw, dict):
            behavior = {k: v for k, v in raw.items() if k != "skip"}
            h.update(json.dumps(behavior, sort_keys=True, default=str).encode())
        else:
            h.update(config_path.read_bytes())
    for source in _imported_sources(fix_dir):
        h.update(source.read_bytes())
    return h.hexdigest()[:12]


def load_fix(fix_dir: Path) -> tuple[FixModule, FixConfig, str]:
    """Load and validate a fix directory. Returns (module, config, fix_hash)."""
    config_path = fix_dir / "config.yaml"
    if not config_path.exists():
        sys.exit(f"Config not found: {config_path}")
    try:
        config = FixConfig.from_yaml(config_path)
    except ValueError as exc:
        sys.exit(str(exc))

    main_py = fix_dir / "main.py"
    if not main_py.exists():
        sys.exit(f"Fix module not found: {main_py}")

    spec = importlib.util.spec_from_file_location("fix", main_py)
    if spec is None or spec.loader is None:
        sys.exit(f"Cannot load fix module: {main_py}")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    for attr in REQUIRED_FUNCS:
        if not hasattr(mod, attr):
            sys.exit(f"Fix module missing required function: {attr}")

    return cast(FixModule, mod), config, compute_fix_hash(fix_dir)


def preflight(*, lint: bool = True) -> None:
    """Verify external tools are available and authenticated."""
    if shutil.which("gh") is None:
        sys.exit("gh (GitHub CLI) is not installed. https://cli.github.com/")
    if shutil.which("git") is None:
        sys.exit("git is not installed.")
    if lint and shutil.which("actionlint") is None:
        sys.exit(
            "actionlint is not installed (used to validate workflow changes). "
            "https://github.com/rhysd/actionlint"
        )
    try:
        result = subprocess.run(
            ["gh", "auth", "status"],
            capture_output=True,
            text=True,
            timeout=SUBPROCESS_TIMEOUT,
            start_new_session=True,
        )
    except subprocess.TimeoutExpired as exc:
        if interrupt.is_set:
            raise ShutdownError from exc
        raise
    if result.returncode != 0:
        if interrupt.is_set:
            raise ShutdownError
        sys.exit(
            f"gh is not authenticated. Run `gh auth login`.\n{result.stderr.strip()}"
        )


def format_age(seconds: float) -> str:
    if seconds < 90:
        return f"{seconds:.0f}s"
    if seconds < 90 * 60:
        return f"{seconds / 60:.0f}m"
    return f"{seconds / 3600:.0f}h"


def load_disabled(path: Path) -> set[str]:
    """Repo names in the hand-maintained denylist. A missing file disables none."""
    if not path.exists():
        return set()
    raw = yaml.safe_load(path.read_text())
    if raw is None:
        return set()
    if not isinstance(raw, list):
        raise ValueError(f"{path.name} must be a list of repo names")
    names: set[str] = set()
    for item in raw:
        if not isinstance(item, str) or item == "":
            raise ValueError(f"{path.name} must be a list of repo names")
        names.add(item)
    return names


def read_repos_file() -> list[Repo] | None:
    """Load repos.yaml regardless of age, if it exists."""
    if not REPOS_FILE.exists():
        return None
    with open(REPOS_FILE) as f:
        raw = yaml.safe_load(f) or []
    disabled = load_disabled(DISABLED_FILE)
    return [Repo.from_dict(r) for r in raw if r["name"] not in disabled]


def sync_if_needed(force: bool) -> list[Repo] | None:
    """Sync repos, using cache if fresh. Returns None on ^C."""
    previous = read_repos_file()
    if not force and previous is not None:
        age = time.time() - REPOS_FILE.stat().st_mtime
        if age <= SYNC_MAX_AGE:
            print(
                f"  {DIM}Using cached repos.yaml ({len(previous)} repos, "
                f"{format_age(age)} old; --refresh to resync){RST}"
            )
            return previous

    try:
        with spinner("Syncing repos"):
            # The previous sync's memo lets sync skip re-checking repos it already
            # knows are active, even when the file is too old to use as a cache.
            result = sync_repos(
                {r.name: r for r in previous or []},
                disabled=load_disabled(DISABLED_FILE),
            )
            atomic_write_yaml([r.to_record() for r in result.repos], REPOS_FILE)
            # Checked in, so the diff shows which repos the activity filter
            # started or stopped excluding.
            atomic_write_yaml(result.excluded, REPOS_FILE.with_name(EXCLUDED_FILE_NAME))
        print(
            f"  {DIM}{len(result.excluded)} repos excluded as inactive "
            f"(see {EXCLUDED_FILE_NAME}){RST}"
        )
        return result.repos
    except KeyboardInterrupt:
        print(f"\n{YELLOW}Interrupted during sync.{RST}")
        return None


def resolve_fix_dir(name_or_path: str) -> Path:
    """Resolve a fix name like 'pin-gha' or a path to the fix directory."""
    as_path = Path(name_or_path)
    if as_path.is_dir():
        return as_path.resolve()
    by_name = FIXES_DIR / name_or_path
    if by_name.is_dir():
        return by_name.resolve()
    sys.exit(f"Fix not found: {name_or_path} (looked in {FIXES_DIR})")


SUBPROCESS_ERRORS = (subprocess.CalledProcessError, subprocess.TimeoutExpired)


# Outcomes worth keeping: a ^C must not overwrite them with "interrupted".
RESULT_STATUSES = ("pending", "skipped", "pr_created", "pr_merged", "pr_closed")


def mark_interrupted(ctx: RunContext, name: str) -> None:
    """Record that a repo didn't finish, without discarding an earlier result.

    A ^C during --apply used to turn a dry run's saved `pending` diff (and
    existing PRs) into a bare `interrupted`, throwing away work the interrupt
    never touched. Those results stay; re-processing handles them next run.
    """
    if ctx.all_status.get(name, {}).get("status") in RESULT_STATUSES:
        return
    set_status(ctx.all_status, ctx.status_file, name, status="interrupted")


def process_repo(
    item: WorkItem, ctx: RunContext, *, push: bool = True
) -> tuple[str, str] | None:
    """Process a single repo: clone, fix, diff/push. Thread-safe.

    Returns the diff and base sha when `push` is deferred so the caller can
    refresh PR state once, after every fix, before any push.
    """
    repo = item.repo
    if interrupt.is_set:
        return None

    try:
        return execute_fix_on_repo(repo, ctx, push=push)
    except ShutdownError:
        # Don't call pd.finish() — interrupted repos are not done.
        ctx.pd.cancel(repo.name)
        mark_interrupted(ctx, repo.name)
        return None


# Spelled as a named tuple: the formatter rewrites `except (*A, B):` into
# `except *A, B:`, which Python reads as `except*` (exception groups).
STORED_PATCH_ERRORS = (*SUBPROCESS_ERRORS, OSError, UnicodeDecodeError)
DIFF_SAVE_ERRORS = (*SUBPROCESS_ERRORS, OSError)


def reusable_dry_run_patch(repo: Repo, repo_dir: Path, ctx: RunContext) -> str | None:
    """The diff a dry run already produced for this repo, if it still applies as is.

    Pushing what was reviewed is faster and safer than re-running the fix. It is
    only valid while the dry run's result is current (same fix) and the repo's
    HEAD is still the base commit the diff was generated against.
    """
    if not ctx.apply:
        return None
    entry = ctx.all_status.get(repo.name, {})
    if entry.get("status") != "pending":
        return None
    if entry.get("fix_hash") not in ctx.accepted_hashes:
        return None
    patch = stored_patch(ctx.status_file.parent, repo.name, entry)
    try:
        if patch.exists() and entry.get("base_sha") == head_sha(repo_dir):
            return patch.read_text()
    except STORED_PATCH_ERRORS:
        # Unreadable patch or repo state: just re-run the fix.
        interrupt.check()
    return None


def run_fix_and_validate(repo: Repo, repo_dir: Path, ctx: RunContext) -> bool:
    """Run the fix on the clone, then validate what it changed.

    Returns True if there is a validated change to show or push. Returns False
    once the repo's outcome (failed, or skipped for no changes) has been recorded.
    """
    ctx.pd.set_phase(repo.name, "fixing")
    try:
        ctx.fix.run(repo_dir, ctx.config_dict)
    except ShutdownError:
        raise
    except Exception:
        interrupt.check()
        write_log(ctx.logs_dir, repo.slug, f"fix failed\n{traceback.format_exc()}")
        set_status(
            ctx.all_status,
            ctx.status_file,
            repo.name,
            status="failed",
            reason="fix failed",
        )
        ctx.pd.finish(repo.name, f"{RED}fix failed{RST}", "failed")
        return False

    if ctx.apply:
        try:
            checkout_branch(repo_dir, ctx.config.branch)
        except ShutdownError:
            raise
        except DIFF_SAVE_ERRORS as exc:
            _record_failed(ctx, repo, "checkout failed", exc)
            return False
    try:
        stage_worktree(repo_dir)
    except ShutdownError:
        raise
    except DIFF_SAVE_ERRORS as exc:
        _record_failed(ctx, repo, "stage failed", exc)
        return False

    try:
        changed = has_changes(repo_dir)
    except ShutdownError:
        raise
    except DIFF_SAVE_ERRORS as exc:
        _record_failed(ctx, repo, "changes failed", exc)
        return False
    if not changed:
        set_status(
            ctx.all_status,
            ctx.status_file,
            repo.name,
            status="skipped",
            reason="no changes",
            fix_hash=ctx.fix_hash,
        )
        ctx.pd.finish(repo.name, f"{GREEN}✓{RST} {DIM}no changes{RST}", "done")
        return False

    # Validate before showing or pushing: a fix must not introduce errors.
    ctx.pd.set_phase(repo.name, "validating")
    try:
        errors = validate_changes(ctx.fix, repo_dir, ctx.config_dict)
    except ShutdownError:
        raise
    except Exception:
        interrupt.check()
        write_log(
            ctx.logs_dir, repo.slug, f"validation crashed\n{traceback.format_exc()}"
        )
        set_status(
            ctx.all_status,
            ctx.status_file,
            repo.name,
            status="failed",
            reason="validation crashed",
        )
        ctx.pd.finish(repo.name, f"{RED}validation crashed{RST}", "failed")
        return False
    if errors:
        report = "\n".join(errors)
        write_log(ctx.logs_dir, repo.slug, f"validation errors\n{report}")
        set_status(
            ctx.all_status,
            ctx.status_file,
            repo.name,
            status="failed",
            reason=f"validation: {len(errors)} error(s)",
        )
        ctx.pd.print(f"\n{RED}{repo.name}: validation errors{RST}\n{report}")
        ctx.pd.finish(repo.name, f"{RED}validation errors{RST}", "failed")
        return False

    return True


def _fail_default_branch(ctx: RunContext, repo: Repo, exc: DefaultBranchError) -> None:
    set_status(
        ctx.all_status,
        ctx.status_file,
        repo.name,
        status="failed",
        reason=str(exc),
    )
    ctx.pd.finish(repo.name, f"{RED}{exc}{RST}", "failed")


def _record_failed(
    ctx: RunContext,
    repo: Repo,
    reason: str,
    exc: subprocess.CalledProcessError | subprocess.TimeoutExpired | OSError,
) -> None:
    interrupt.check()
    detail = str(exc) if isinstance(exc, OSError) else format_error(exc)
    write_log(ctx.logs_dir, repo.slug, f"{reason}\n{detail}")
    if ctx.all_status.get(repo.name, {}).get("status") != "pending":
        set_status(
            ctx.all_status,
            ctx.status_file,
            repo.name,
            status="failed",
            reason=reason,
        )
    ctx.pd.finish(repo.name, f"{RED}{reason}{RST}", "failed")


def _publish_review(ctx: RunContext, repo: Repo, diff: str, **fields: str) -> bool:
    try:
        publish_reviewed(ctx.all_status, ctx.status_file, repo.name, diff, **fields)
    except ShutdownError:
        raise
    except DIFF_SAVE_ERRORS as exc:
        _record_failed(ctx, repo, "diff failed", exc)
        return False
    return True


def execute_fix_on_repo(
    repo: Repo, ctx: RunContext, *, push: bool = True
) -> tuple[str, str] | None:
    """Inner processing — raises ShutdownError to signal interrupt."""
    repo_dir = ctx.repos_dir / repo.slug

    ctx.pd.set_phase(repo.name, "cloning")
    try:
        clone(repo.name, repo.default_branch, repo_dir, ctx.config.sparse_paths)
    except ShutdownError:
        raise
    except DIFF_SAVE_ERRORS as exc:
        _record_failed(ctx, repo, "clone failed", exc)
        return None

    if interrupt.is_set:
        raise ShutdownError

    patch = reusable_dry_run_patch(repo, repo_dir, ctx)
    if patch is not None:
        # Apply exactly what the dry run showed (and validated) instead of
        # re-running the fix, which can resolve differently the second time.
        ctx.pd.set_phase(repo.name, "applying dry-run patch")
        try:
            reused = apply_patch(repo_dir, patch)
        except ShutdownError:
            raise
        except (subprocess.TimeoutExpired, OSError) as exc:
            _record_failed(ctx, repo, "apply failed", exc)
            return None
    else:
        reused = False
    if reused:
        try:
            checkout_branch(repo_dir, ctx.config.branch)
        except ShutdownError:
            raise
        except DIFF_SAVE_ERRORS as exc:
            _record_failed(ctx, repo, "checkout failed", exc)
            return None
        try:
            stage_worktree(repo_dir)
        except ShutdownError:
            raise
        except DIFF_SAVE_ERRORS as exc:
            _record_failed(ctx, repo, "stage failed", exc)
            return None
    elif not run_fix_and_validate(repo, repo_dir, ctx):
        return None

    try:
        diff = get_diff(repo_dir)
        base_sha = head_sha(repo_dir)
    except ShutdownError:
        raise
    except DIFF_SAVE_ERRORS as exc:
        _record_failed(ctx, repo, "diff failed", exc)
        return None

    if not ctx.apply:
        try:
            unstage(repo_dir)
        except ShutdownError:
            raise
        except DIFF_SAVE_ERRORS as exc:
            _record_failed(ctx, repo, "unstage failed", exc)
            return None
        try:
            refuse_default_branch(ctx.config.branch, repo.default_branch)
        except DefaultBranchError as exc:
            _fail_default_branch(ctx, repo, exc)
            return None
        if not _publish_review(
            ctx,
            repo,
            diff,
            status="pending",
            fix_hash=ctx.fix_hash,
            base_sha=base_sha,
        ):
            return None
        ctx.pd.print(f"\n{format_diff_box(repo.name, diff)}")
        ctx.pd.finish(repo.name, "", "done")
        return None

    if interrupt.is_set:
        raise ShutdownError

    if not push:
        return diff, base_sha

    push_repo(repo, ctx, diff, base_sha)
    return None


def push_repo(repo: Repo, ctx: RunContext, diff: str, base_sha: str) -> None:
    """Push one prepared repo and record the PR. Raises ShutdownError on ^C."""
    repo_dir = ctx.repos_dir / repo.slug
    if interrupt.is_set:
        raise ShutdownError

    ctx.pd.set_phase(repo.name, "pushing")
    try:
        pr = commit_and_push(
            repo_dir,
            ctx.config.pr_spec(repo.name),
            ctx.existing_prs.get(repo.name),
            default_branch=repo.default_branch,
        )
        interrupt.check()
        # Merged/closed PRs aren't touched (see commit_and_push); record them as
        # their own statuses so they aren't reported as freshly created.
        pr_status = "pr_created" if pr.state == "OPEN" else f"pr_{pr.state.lower()}"
        fields: dict[str, str] = {"status": pr_status, "pr_url": pr.url}
        if pr.state == "OPEN":
            fields["pushed_sha"] = head_sha(repo_dir)
        fields["fix_hash"] = ctx.fix_hash
        fields["base_sha"] = base_sha
        if pr.node_id:
            fields["pr_node_id"] = pr.node_id
        if not _publish_review(
            ctx,
            repo,
            diff,
            **fields,
        ):
            return
        if pr.state == "OPEN":
            ctx.pd.finish(repo.name, f"{GREEN}✓ PR{RST} {pr.url}", "done")
        else:
            ctx.pd.finish(
                repo.name,
                f"{YELLOW}PR already {pr.state.lower()}, not pushed{RST} {pr.url}",
                "done",
            )
    except ShutdownError:
        raise
    except DefaultBranchError as exc:
        _fail_default_branch(ctx, repo, exc)
    except DIFF_SAVE_ERRORS as exc:
        _record_failed(ctx, repo, "push/PR failed", exc)


def filter_work(
    repos: list[Repo],
    all_status: Status,
    config: FixConfig,
    fix: FixModule,
    fix_hash: str,
    only_repos: set[str] | None,
    *,
    apply: bool = False,
    accepted_hashes: frozenset[str] | None = None,
) -> tuple[list[WorkItem], int]:
    """Filter repos into work items. Returns (work, already_done_count).

    accepted_hashes: hashes that still count as current. Defaults to fix_hash.
    The raw-byte hash is added only when the fix imports nothing that hash misses.
    """
    current_hashes = accepted_hashes or frozenset({fix_hash})
    skip_repos = set(config.skip)
    config_dict = config.as_dict()
    # "pending" = dry-run saw changes but didn't push. In --apply mode,
    # these need to be re-processed so the PR actually gets created.
    done_statuses: tuple[str, ...] = (
        "pr_created",
        "pr_merged",
        "pr_closed",
        "skipped",
    )
    if not apply:
        done_statuses = (*done_statuses, "pending")
    work: list[WorkItem] = []
    already_done = 0

    for repo in repos:
        name = repo.name

        if only_repos is not None and name not in only_repos:
            continue
        if name in skip_repos:
            continue

        repo_status = all_status.get(name, {})
        prev_hash = repo_status.get("fix_hash")
        prev_status = repo_status.get("status")

        if prev_hash in current_hashes and prev_status in done_statuses:
            already_done += 1
            continue

        stale = "fix changed" if prev_status == "pr_created" else None

        if not fix.accept_repo(repo.to_dict(), config_dict):
            continue

        work.append(WorkItem(repo=repo, stale_reason=stale))

    return work, already_done


def lookup_existing_prs(branch: str) -> dict[str, RolloutPR]:
    """One batched search of PRs on `branch`. A failed lookup exits the run."""
    try:
        return pr_by_repo(search_prs(branch))
    except ShutdownError:
        raise
    except SUBPROCESS_ERRORS as exc:
        interrupt.check()
        sys.exit(f"Could not look up existing PRs: {format_error(exc)}")


def _announce_shutdown(pd: ProgressDisplay, announced: list[bool]) -> None:
    """Tell the user the run is stopping, once.

    Printed from the worker loop. The signal handler must not print: it runs
    on the thread that may already hold ProgressDisplay's lock. `announced`
    is a one-element cell shared by both loops.
    """
    if announced[0] or not interrupt.is_set:
        return
    announced[0] = True
    pd.print(f"\n{YELLOW}Gracefully exiting, please wait...{RST}")


def _halt_pool(
    pool: ThreadPoolExecutor, pd: ProgressDisplay, announced: list[bool]
) -> bool:
    """Cancel queued work once ^C has been noticed. Re-reads the event.

    Worker threads set it during `future.result()`, so a read from earlier in
    the same iteration is not still false.
    """
    if not interrupt.is_set:
        return False
    _announce_shutdown(pd, announced)
    pool.shutdown(wait=False, cancel_futures=True)
    return True


def _map_work[T, R](
    items: Sequence[T],
    workers: int,
    fn: Callable[[T], R | None],
    pd: ProgressDisplay,
    announced: list[bool],
) -> list[R]:
    """Run `fn` over `items`. Joins every worker before returning, including on ^C."""
    results: list[R] = []
    try:
        if workers == 1 or len(items) <= 1:
            for item in items:
                if interrupt.is_set:
                    break
                result = fn(item)
                if result is not None:
                    results.append(result)
            return results

        n = min(workers, len(items))
        with ThreadPoolExecutor(max_workers=n) as pool:
            futures = [pool.submit(fn, item) for item in items]
            for future in as_completed(futures):
                # Before the pool join, so the line shows while workers finish.
                if _halt_pool(pool, pd, announced):
                    break
                result = future.result()
                if result is not None:
                    results.append(result)
                if _halt_pool(pool, pd, announced):
                    break
        return results
    finally:
        # Also after the last item, which has no following iteration to look.
        _announce_shutdown(pd, announced)


def run_work(ctx: RunContext, work: list[WorkItem], workers: int) -> bool:
    """Execute work items with progress display. Returns True if interrupted."""
    announced = [False]

    def _prepare(item: WorkItem) -> tuple[WorkItem, str, str] | None:
        payload = process_repo(item, ctx, push=not ctx.apply)
        if payload is None:
            return None
        diff, base_sha = payload
        return item, diff, base_sha

    def _push(prepared: tuple[WorkItem, str, str]) -> None:
        item, diff, base_sha = prepared
        if interrupt.is_set:
            return
        try:
            push_repo(item.repo, ctx, diff, base_sha)
        except ShutdownError:
            ctx.pd.cancel(item.repo.name)
            mark_interrupted(ctx, item.repo.name)

    interrupt.install()
    try:
        prepared = _map_work(work, workers, _prepare, ctx.pd, announced)
        # The opening search goes stale while fixes run. Refresh once, after
        # every fix and before any push. A repo the refresh returns uses that
        # state; one it omits keeps the opening PR.
        if ctx.apply and prepared and not interrupt.is_set:
            try:
                refreshed = lookup_existing_prs(ctx.config.branch)
            except ShutdownError:
                pass
            else:
                ctx.existing_prs = {**ctx.existing_prs, **refreshed}
                _map_work(prepared, workers, _push, ctx.pd, announced)

        if interrupt.is_set:
            _announce_shutdown(ctx.pd, announced)
            for item in work:
                name = item.repo.name
                if not ctx.pd.is_completed(name):
                    mark_interrupted(ctx, name)

        # Print summary while still inside the ProgressDisplay context
        # (before __exit__ erases the footer)
        c = ctx.pd.counts
        interrupted = len(work) - (c["done"] - ctx.pd.initial_done) - c["failed"]
        parts = []
        if c["done"]:
            parts.append(f"{GREEN}{c['done']} done{RST}")
        if c["failed"]:
            parts.append(f"{RED}{c['failed']} failed{RST}")
        if interrupted > 0:
            parts.append(f"{YELLOW}{interrupted} interrupted{RST}")
        if parts:
            ctx.pd.print(f"\n{BOLD}Summary:{RST} {', '.join(parts)}")
        if interrupt.is_set:
            ctx.pd.print(f"{YELLOW}Progress saved to {ctx.status_file}{RST}")
        if c["failed"] and ctx.logs_dir.exists():
            ctx.pd.print(f"  {RED}Logs:{RST} {DIM}{ctx.logs_dir}{RST}")
    finally:
        was_interrupted = interrupt.is_set
        interrupt.restore()
    return was_interrupted


def _recorded_pr_url(info: dict[str, str]) -> str | None:
    return info.get("pr_url")


def cache_pr_node_ids(all_status: Status, status_file: Path) -> None:
    """Record node ids for recorded PRs whose id isn't known yet.

    `gh pr create` only prints a URL. One batched lookup here means later
    commands (e.g. --undraft) can address PRs by id without searching.
    """
    missing = {
        name: info["pr_url"]
        for name, info in all_status.items()
        if _recorded_pr_url(info) is not None and info.get("pr_node_id") is None
    }
    if not missing:
        return
    try:
        ids = resolve_pr_node_ids(list(missing.values()))
    except SUBPROCESS_ERRORS as exc:
        interrupt.check()
        # Not fatal: the PRs exist and ids are re-resolved on demand.
        print(f"{YELLOW}Could not cache PR node ids: {format_error(exc)}{RST}")
        return
    merge_status(
        all_status,
        status_file,
        {name: {"pr_node_id": ids[url]} for name, url in missing.items() if url in ids},
    )


def undraft_command(fix_dir: Path, args: argparse.Namespace) -> int:
    """Mark this fix's draft PRs ready for review (listing only, without --apply).

    Works from the PRs recorded in status.yaml, addressed by cached node id, so it
    needs no search (and no waiting for the search index after an --apply).
    """
    preflight(lint=False)
    status_file = fix_dir / "status.yaml"
    all_status = load_status(status_file)
    only_repos = set(args.repos.split(",")) if args.repos else None
    ours = {
        name: info
        for name, info in all_status.items()
        if _recorded_pr_url(info) is not None
        and (only_repos is None or name in only_repos)
    }

    try:
        missing = {
            name: info["pr_url"]
            for name, info in ours.items()
            if info.get("pr_node_id") is None
        }
        if missing:
            ids = resolve_pr_node_ids(list(missing.values()))
            merge_status(
                all_status,
                status_file,
                {n: {"pr_node_id": ids[u]} for n, u in missing.items() if u in ids},
            )
        node_ids = [
            all_status[name]["pr_node_id"]
            for name in ours
            if all_status[name].get("pr_node_id") is not None
        ]
        drafts = sorted(
            (pr for pr in pr_states(node_ids) if pr.state == "OPEN" and pr.draft),
            key=lambda pr: pr.repo,
        )
        if args.limit is not None:
            drafts = drafts[: args.limit]
        failed = mark_ready(drafts) if args.apply and drafts else []
    except SUBPROCESS_ERRORS as exc:
        interrupt.check()
        sys.exit(f"GitHub request failed: {format_error(exc)}")

    if not drafts:
        print(f"{DIM}No draft PRs to mark ready.{RST}")
        return 0
    verb = "Marked ready" if args.apply else "Would mark ready"
    print(f"{BOLD}{verb}:{RST} {len(drafts) - len(failed)} PRs")
    for pr in drafts:
        mark = f"{RED}✗{RST}" if pr in failed else f"{GREEN}✓{RST}"
        print(f"  {mark} {pr.repo}  {DIM}{pr.url}{RST}")
    if not args.apply:
        print(f"{DIM}Dry run: pass --apply to mark these ready for review.{RST}")
    if failed:
        print(f"{RED}{len(failed)} PRs could not be marked ready.{RST}")
        return 1
    return 0


def diffs_command(fix_dir: Path, args: argparse.Namespace) -> int:
    """Pretty-print every pending repo's stored dry-run diff, without re-running."""
    all_status = load_status(fix_dir / "status.yaml")
    only_repos = set(args.repos.split(",")) if args.repos else None
    pending = sorted(
        name
        for name, info in all_status.items()
        if info.get("status") == "pending"
        and (only_repos is None or name in only_repos)
    )
    if args.limit is not None:
        pending = pending[: args.limit]
    if not pending:
        print(f"{DIM}No pending dry-run changes. Run the fix first.{RST}")
        return 0

    for name in pending:
        patch = stored_patch(fix_dir, name, all_status[name])
        if patch.exists():
            print(f"\n{format_diff_box(name, patch.read_text())}")
        else:
            print(f"\n{YELLOW}{name}: no stored patch; re-run the dry run{RST}")
    print(f"\n{BOLD}{len(pending)}{RST} repos would change")
    return 0


def sync_command() -> int:
    """Force-refresh repos.yaml without running any fix."""
    preflight(lint=False)
    repos = sync_if_needed(force=True)
    if repos is None:
        return 130
    print(f"Synced {len(repos)} repos to {REPOS_FILE.name}")
    return 0


def main(argv: Sequence[str] | None = None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    # Handled before argparse because the fix name is a bare positional,
    # so "sync" can't be a real subcommand without breaking `repomod <fix>`.
    if argv[:1] == ["sync"]:
        return sync_command()

    parser = argparse.ArgumentParser(description="Run a fix across GitHub repos")
    parser.add_argument("fix", help="Fix name (e.g. pin-gha) or path to fix directory")
    parser.add_argument(
        "--apply", action="store_true", help="Push and create PRs (default: dry-run)"
    )
    parser.add_argument(
        "--repos", help="Comma-separated repos (e.g. acme/backend,acme/web)"
    )
    parser.add_argument(
        "--workers",
        type=int,
        default=DEFAULT_WORKERS,
        help=f"Parallel workers (default: {DEFAULT_WORKERS})",
    )
    parser.add_argument("--limit", type=int, default=None, help="Max repos to process")
    parser.add_argument(
        "--status", action="store_true", help="Show status summary and exit"
    )
    parser.add_argument(
        "--prs",
        action="store_true",
        help="Show merged/open/closed state of this fix's PRs on GitHub and exit",
    )
    parser.add_argument(
        "--diffs",
        action="store_true",
        help="Show the stored dry-run diffs of every pending repo and exit",
    )
    parser.add_argument(
        "--undraft",
        action="store_true",
        help="Mark this fix's draft PRs ready for review (needs --apply; else lists)",
    )
    parser.add_argument(
        "--plan", action="store_true", help="Show what would run, without cloning"
    )
    parser.add_argument(
        "--refresh",
        action="store_true",
        help=f"Force repo sync (default: use cache if <{format_age(SYNC_MAX_AGE)} old)",
    )
    args = parser.parse_args(argv)

    logging.basicConfig(level=logging.WARNING, format="%(message)s")

    fix_dir = resolve_fix_dir(args.fix)

    if args.status:
        log_dirs = sorted(
            fix_dir.glob("logs-*"),
            key=lambda p: p.stat().st_mtime,
            reverse=True,
        )
        logs_dir = log_dirs[0] if log_dirs else fix_dir / "logs"
        print_full(fix_dir / "status.yaml", logs_dir)
        return 0

    if args.diffs:
        return diffs_command(fix_dir, args)

    if args.undraft:
        return undraft_command(fix_dir, args)

    if args.prs:
        preflight(lint=False)
        branch = FixConfig.from_yaml(fix_dir / "config.yaml").branch
        print_rollout(branch, search_prs(branch))
        return 0

    preflight()

    fix, config, fix_hash = load_fix(fix_dir)
    try:
        require_branch_name(config.branch)
    except InvalidBranchError as exc:
        print(exc, file=sys.stderr)
        return 1
    status_file = fix_dir / "status.yaml"
    repos_dir = fix_dir / "repos"
    logs_dir = fix_dir / f"logs-{fix_hash}"
    patches_dir = fix_dir / f"patches-{fix_hash}"
    repos_dir.mkdir(exist_ok=True)
    logs_dir.mkdir(exist_ok=True)
    patches_dir.mkdir(exist_ok=True)

    repos = sync_if_needed(args.refresh)
    if repos is None:
        return 130

    all_status = load_status(status_file)
    only_repos: set[str] | None = set(args.repos.split(",")) if args.repos else None
    work, already_done = filter_work(
        repos,
        all_status,
        config,
        fix,
        fix_hash,
        only_repos,
        apply=args.apply,
        accepted_hashes=_accepted_hashes(fix_dir, fix_hash),
    )
    if args.limit is not None:
        work = work[: args.limit]

    if not work:
        print(f"{DIM}No repos to process.{RST}")
        print_summary(all_status, logs_dir, fix_dir)
        return 0

    if args.plan:
        print(f"\n{BOLD}Plan:{RST} {len(work)} repos to process")
        for item in work:
            suffix = (
                f"  {YELLOW}{item.stale_reason}{RST}"
                if item.stale_reason is not None
                else ""
            )
            print(f"  {item.repo.name}{suffix}")
        return 0

    existing_prs: dict[str, RolloutPR] = {}
    if args.apply:
        # Opening snapshot for the run, not a lookup per repo. The refresh
        # after the fixes overrides this per repo and does not drop the rest.
        existing_prs = lookup_existing_prs(config.branch)

    actual_workers = min(args.workers, len(work)) if args.workers > 1 else 1
    label = "" if args.apply else "dry run"
    total = already_done + len(work)
    with ProgressDisplay(
        total=total,
        workers=actual_workers,
        label=label,
        done=already_done,
    ) as pd:
        ctx = RunContext(
            fix=fix,
            config=config,
            config_dict=config.as_dict(),
            fix_hash=fix_hash,
            accepted_hashes=_accepted_hashes(fix_dir, fix_hash),
            repos_dir=repos_dir,
            logs_dir=logs_dir,
            patches_dir=patches_dir,
            status_file=status_file,
            apply=args.apply,
            all_status=all_status,
            pd=pd,
            existing_prs=existing_prs,
        )
        was_interrupted = run_work(ctx, work, args.workers)

    if args.apply and not was_interrupted:
        cache_pr_node_ids(all_status, status_file)
    if not was_interrupted:
        print_summary(all_status, logs_dir, fix_dir)
    return 130 if was_interrupted else 0


def cli() -> None:
    """Entry point for `repomod` console script."""
    try:
        code = main()
    except ShutdownError:
        code = 130
    raise SystemExit(code)


if __name__ == "__main__":
    cli()
