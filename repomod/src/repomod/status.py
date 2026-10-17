"""Status tracking: load, save, print summaries, per-repo logging."""

import datetime
import threading
from collections import Counter
from pathlib import Path
from typing import Any, cast

import yaml

from repomod.types import RolloutPR, Status
from repomod.ui import BOLD, CYAN, DIM, GREEN, RED, RST, YELLOW

_lock = threading.Lock()


def atomic_write_yaml(data: Any, path: Path) -> None:
    """Write YAML atomically via temp file + rename."""
    tmp = path.with_suffix(".tmp")
    with open(tmp, "w") as f:
        yaml.dump(
            data, f, default_flow_style=False, sort_keys=False, allow_unicode=True
        )
    tmp.rename(path)


def load_status(path: Path) -> Status:
    if not path.exists():
        return {}
    with open(path) as f:
        raw = yaml.safe_load(f)
    if raw is None:
        return {}
    if not isinstance(raw, dict):
        raise ValueError(f"Expected a YAML mapping in {path}, got {type(raw).__name__}")
    return cast("Status", raw)


def set_status(all_status: Status, path: Path, repo_name: str, **fields: str) -> None:
    """Update a repo's status and persist atomically. Acquires the lock."""
    with _lock:
        all_status[repo_name] = fields
        atomic_write_yaml(all_status, path)


def merge_status(
    all_status: Status, path: Path, updates: dict[str, dict[str, str]]
) -> None:
    """Add fields to existing repo entries in a single write. Acquires the lock."""
    with _lock:
        for repo_name, fields in updates.items():
            all_status[repo_name] = {**all_status[repo_name], **fields}
        atomic_write_yaml(all_status, path)


def write_log(logs_dir: Path, slug: str, content: str) -> None:
    """Append to a per-repo log file. slug is e.g. 'acme-backend'."""
    log_path = logs_dir / f"{slug}.log"
    timestamp = datetime.datetime.now(datetime.UTC).strftime("%Y-%m-%d %H:%M:%S UTC")
    with open(log_path, "a") as f:
        f.write(f"--- {timestamp} ---\n{content}\n")


def write_patch(patches_dir: Path, slug: str, diff: str) -> None:
    """Save the fix's diff for a repo. Overwrites: a repo has one current patch."""
    (patches_dir / f"{slug}.patch").write_text(diff)


def patch_stat(patch: str) -> tuple[int, int, int]:
    """(files, added lines, removed lines) of a unified diff."""
    files = added = removed = 0
    for line in patch.splitlines():
        if line.startswith("diff --git "):
            files += 1
        elif line.startswith("+") and not line.startswith("+++"):
            added += 1
        elif line.startswith("-") and not line.startswith("---"):
            removed += 1
    return files, added, removed


def stored_patch(fix_dir: Path, repo_name: str, info: dict[str, str]) -> Path:
    """Where a repo's dry-run diff was saved (keyed by the fix hash it ran with)."""
    return (
        fix_dir
        / f"patches-{info.get('fix_hash')}"
        / f"{repo_name.replace('/', '-')}.patch"
    )


MAX_LISTED_PENDING = 40


def print_pending(all_status: Status, fix_dir: Path) -> None:
    """List repos with a proposed change, each with its diffstat."""
    pending = sorted(n for n, i in all_status.items() if i.get("status") == "pending")
    if not pending:
        return
    print(f"  {CYAN}Pending (would change):{RST}")
    for name in pending[:MAX_LISTED_PENDING]:
        patch = stored_patch(fix_dir, name, all_status[name])
        if patch.exists():
            files, added, removed = patch_stat(patch.read_text())
            noun = "file" if files == 1 else "files"
            stat = f"{files} {noun}  {GREEN}+{added}{RST} {RED}-{removed}{RST}"
        else:
            stat = f"{DIM}(no stored patch){RST}"
        print(f"    {name}  {stat}")
    if len(pending) > MAX_LISTED_PENDING:
        print(f"    {DIM}…and {len(pending) - MAX_LISTED_PENDING} more{RST}")
    print(f"  {DIM}See the diffs: repomod {fix_dir.name} --diffs{RST}")


def print_summary(
    all_status: Status, logs_dir: Path | None = None, fix_dir: Path | None = None
) -> None:
    counts: Counter[str] = Counter()
    failed_repos: list[str] = []
    for name, info in all_status.items():
        s = info.get("status", "unknown")
        counts[s] += 1
        if s == "failed":
            failed_repos.append(name)

    parts = []
    for status_name, color, label in [
        ("pr_created", GREEN, "PRs"),
        ("pr_merged", GREEN, "merged"),
        ("pr_closed", YELLOW, "closed"),
        ("pending", CYAN, "pending"),
        ("skipped", DIM, "skipped"),
        ("failed", RED, "failed"),
        ("interrupted", YELLOW, "interrupted"),
    ]:
        if counts[status_name]:
            parts.append(f"{color}{counts[status_name]} {label}{RST}")

    print(f"\n{BOLD}Summary:{RST} {', '.join(parts)}")
    if fix_dir is not None:
        print_pending(all_status, fix_dir)
    if failed_repos:
        print(f"  {RED}Failed:{RST}")
        for name in failed_repos:
            slug = name.replace("/", "-")
            if logs_dir:
                log_path = logs_dir / f"{slug}.log"
                print(f"    {name}  {DIM}{log_path}{RST}")
            else:
                print(f"    {name}")


def print_full(status_file: Path, logs_dir: Path) -> None:
    """Print a detailed status report for --status flag."""
    all_status = load_status(status_file)
    if not all_status:
        print(f"{DIM}No status file found.{RST}")
        return

    print_summary(all_status, fix_dir=status_file.parent)

    for name, info in all_status.items():
        s = info.get("status", "unknown")
        if s in ("pr_created", "pr_merged", "pr_closed"):
            print(f"  {GREEN}✓{RST} {name}  {DIM}{info.get('pr_url', '')}{RST}")
        elif s == "failed":
            log_path = logs_dir / f"{name.replace('/', '-')}.log"
            hint = f"{DIM}→ {log_path}{RST}" if log_path.exists() else ""
            print(f"  {RED}✗{RST} {name}  {hint}")
        elif s == "interrupted":
            print(f"  {YELLOW}⊘{RST} {name}")


def print_rollout(branch: str, prs: list[RolloutPR]) -> None:
    """Print PR states for a fix's branch; open and closed PRs are the ones to chase."""
    counts = Counter(pr.state for pr in prs)
    print(
        f"{BOLD}{branch}:{RST} {GREEN}{counts['MERGED']} merged{RST}, "
        f"{CYAN}{counts['OPEN']} open{RST}, {YELLOW}{counts['CLOSED']} closed{RST}"
    )
    for state, color, label in [
        ("OPEN", CYAN, "Open"),
        ("CLOSED", YELLOW, "Closed without merging"),
    ]:
        matching = [pr for pr in prs if pr.state == state]
        if matching:
            print(f"  {color}{label}:{RST}")
            for pr in matching:
                draft = f" {DIM}(draft){RST}" if pr.draft else ""
                print(f"    {pr.repo}  {DIM}{pr.url}{RST}{draft}")
