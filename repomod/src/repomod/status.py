"""Status tracking: load, save, print summaries, per-repo logging."""

import contextlib
import ctypes
import datetime
import errno
import fcntl
import os
import shutil
import sys
import threading
import uuid
from collections import Counter
from collections.abc import Generator
from pathlib import Path
from typing import Any, cast

import yaml

from repomod.types import RolloutPR, Status
from repomod.ui import BOLD, CYAN, DIM, GREEN, RED, RST, YELLOW

_lock = threading.Lock()
_LIVE = ".live"
_GENERATIONS = "generations"
_LOCK_NAME = ".lock"
# macOS renamex_np RENAME_SWAP and Linux renameat2 RENAME_EXCHANGE.
_RENAME_EXCHANGE = 2
_AT_FDCWD = -100


def _required_open_flag(name: str) -> int:
    """Require a POSIX open flag (macOS and Linux, any architecture)."""
    flag = getattr(os, name, None)
    if not isinstance(flag, int):
        raise OSError(
            f"{name} is required to publish status without following a symlink"
        )
    return flag


_NOFOLLOW = _required_open_flag("O_NOFOLLOW")
_CLOEXEC = _required_open_flag("O_CLOEXEC")


def _required_flock() -> tuple[Any, int, int]:
    """Require POSIX flock (macOS and Linux, any architecture)."""
    flock = getattr(fcntl, "flock", None)
    lock_ex = getattr(fcntl, "LOCK_EX", None)
    lock_un = getattr(fcntl, "LOCK_UN", None)
    if (
        not callable(flock)
        or not isinstance(lock_ex, int)
        or not isinstance(lock_un, int)
    ):
        raise OSError("fcntl.flock is required to publish status across processes")
    return flock, lock_ex, lock_un


_flock, _LOCK_EX, _LOCK_UN = _required_flock()


def atomic_write_yaml(data: Any, path: Path) -> None:
    """Replace path with a complete YAML document.

    The temporary file is created in the destination directory with O_EXCL and
    O_NOFOLLOW, so a symlink planted on a predictable temp name is not followed.
    File contents are fsynced before the name is replaced.
    """
    tmp, fd = _create_exclusive_temp(path)
    try:
        with os.fdopen(fd, "w") as handle:
            yaml.dump(
                data,
                handle,
                default_flow_style=False,
                sort_keys=False,
                allow_unicode=True,
            )
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp, path)
    except BaseException:
        with contextlib.suppress(FileNotFoundError):
            os.unlink(tmp)
        raise


def _create_exclusive_temp(path: Path) -> tuple[Path, int]:
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | _NOFOLLOW | _CLOEXEC
    while True:
        tmp = path.parent / f".{path.name}.{uuid.uuid4().hex}.tmp"
        try:
            return tmp, os.open(tmp, flags, 0o666)
        except OSError as err:
            if err.errno not in (errno.EEXIST, errno.ELOOP):
                raise


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
    _replace_row(all_status, path, repo_name, fields, {})


def publish_reviewed(
    all_status: Status,
    path: Path,
    repo_name: str,
    diff: str,
    **fields: str,
) -> None:
    """Store a reviewed diff and its status row so a crash cannot split them."""
    rel = Path(f"patches-{fields['fix_hash']}") / f"{repo_name.replace('/', '-')}.patch"
    _replace_row(all_status, path, repo_name, fields, {rel: diff})


def merge_status(
    all_status: Status, path: Path, updates: dict[str, dict[str, str]]
) -> None:
    """Add fields to existing repo entries in a single write. Acquires the lock."""
    with _lock, _generation_lock(path.parent):
        published = {name: dict(row) for name, row in load_status(path).items()}
        for repo_name, fields in updates.items():
            published[repo_name] = {**published[repo_name], **fields}
        _publish(path.parent, published, {})
        _remember(all_status, published)


def _replace_row(
    all_status: Status,
    path: Path,
    repo_name: str,
    fields: dict[str, str],
    patch_writes: dict[Path, str],
) -> None:
    # Thread lock first, then the cross-process flock, so the two never invert.
    with _lock, _generation_lock(path.parent):
        published = dict(load_status(path))
        published[repo_name] = _with_pr_identity(published.get(repo_name), fields)
        _publish(path.parent, published, patch_writes)
        _remember(all_status, published)


def _remember(all_status: Status, published: Status) -> None:
    stale = [name for name in all_status if name not in published]
    for name in stale:
        del all_status[name]
    all_status.update(published)


@contextlib.contextmanager
def _generation_lock(fix_dir: Path) -> Generator[None]:
    """Hold the generations lock across one publish.

    The body re-reads status and swings `.live` before this releases. The
    flock is dropped on the way out, including when the body raises
    ShutdownError, and that exception still propagates.
    """
    root = fix_dir / _GENERATIONS
    root.mkdir(parents=True, exist_ok=True)
    fd = os.open(
        root / _LOCK_NAME,
        os.O_RDWR | os.O_CREAT | _CLOEXEC | _NOFOLLOW,
        0o644,
    )
    try:
        _flock(fd, _LOCK_EX)
        try:
            yield
        finally:
            _flock(fd, _LOCK_UN)
    finally:
        os.close(fd)


def _with_pr_identity(
    previous: dict[str, str] | None, fields: dict[str, str]
) -> dict[str, str]:
    if previous is None:
        return fields
    wrote_url = "pr_url" in fields
    url_unchanged = not wrote_url or fields["pr_url"] == previous.get("pr_url")
    kept: dict[str, str] = {}
    if not wrote_url and "pr_url" in previous:
        kept["pr_url"] = previous["pr_url"]
    if url_unchanged and "pr_node_id" not in fields and "pr_node_id" in previous:
        kept["pr_node_id"] = previous["pr_node_id"]
    if not kept:
        return fields
    return {**fields, **kept}


def _publish(
    fix_dir: Path, all_status: Status, patch_writes: dict[Path, str]
) -> None:
    generation = _materialize(fix_dir, all_status, patch_writes)
    try:
        if _canonical_is_generation(fix_dir):
            _link_new_patch_dirs(fix_dir, patch_writes)
        else:
            _point_canonical_names_at_current_bytes(fix_dir, patch_writes)
        # status.yaml and patches-* are symlinks into `.live`, so this one
        # replace is the only instant both the row and the patch bytes change.
        _swing(fix_dir, generation)
    except Exception:
        if not _live_points_at(fix_dir, generation):
            shutil.rmtree(generation, ignore_errors=True)
        raise
    try:
        _fsync(fix_dir)
    except OSError:
        # The directory entry already names the new generation. Raising would
        # roll the in-memory row back and the next write would split the pair.
        return
    _drop_other_generations(fix_dir, generation)


def _materialize(
    fix_dir: Path, all_status: Status, patch_writes: dict[Path, str]
) -> Path:
    dest = _new_generation(fix_dir)
    try:
        _copy_patches(fix_dir, dest)
        for rel, diff in patch_writes.items():
            parent = dest / rel.parent
            parent.mkdir(parents=True, exist_ok=True)
            write_patch(parent, rel.stem, diff)
        atomic_write_yaml(all_status, dest / "status.yaml")
        _fsync_tree(dest)
    except Exception:
        shutil.rmtree(dest, ignore_errors=True)
        raise
    return dest


def _point_canonical_names_at_current_bytes(
    fix_dir: Path, patch_writes: dict[Path, str]
) -> None:
    snapshot = _new_generation(fix_dir)
    try:
        _copy_patches(fix_dir, snapshot)
        status = fix_dir / "status.yaml"
        if status.is_file():
            shutil.copyfile(status, snapshot / "status.yaml")
        _fsync_tree(snapshot)
        _swing(fix_dir, snapshot)
    except Exception:
        if not _live_points_at(fix_dir, snapshot):
            shutil.rmtree(snapshot, ignore_errors=True)
        raise
    _rewire(fix_dir, patch_writes)


def _rewire(fix_dir: Path, patch_writes: dict[Path, str]) -> None:
    status = fix_dir / "status.yaml"
    if not status.is_symlink():
        _point_at_live(status)
    for entry in list(fix_dir.iterdir()):
        if entry.name.startswith("patches-") and not entry.is_symlink():
            _point_at_live(entry)
    _link_new_patch_dirs(fix_dir, patch_writes)


def _link_new_patch_dirs(fix_dir: Path, patch_writes: dict[Path, str]) -> None:
    for rel in patch_writes:
        link = fix_dir / rel.parent
        if link.exists() or link.is_symlink():
            continue
        link.symlink_to(Path(_LIVE) / rel.parent.name)


def _point_at_live(path: Path) -> None:
    tmp = path.with_name(f".{path.name}.swap")
    _discard(tmp)
    tmp.symlink_to(Path(_LIVE) / path.name)
    if path.is_symlink() or path.exists():
        _exchange(path, tmp)
        _discard(tmp)
    else:
        os.replace(tmp, path)


def _exchange(left: Path, right: Path) -> None:
    # rename() cannot replace a directory with a symlink (EISDIR). The names
    # readers use have to become symlinks without a window where the path is gone.
    ctypes.set_errno(0)
    if sys.platform == "darwin":
        rename = ctypes.CDLL(None, use_errno=True).renamex_np
        rename.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
        rename.restype = ctypes.c_int
        rc = rename(os.fsencode(left), os.fsencode(right), _RENAME_EXCHANGE)
    elif sys.platform == "linux":
        rename = ctypes.CDLL(None, use_errno=True).renameat2
        rename.argtypes = [
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint,
        ]
        rename.restype = ctypes.c_int
        rc = rename(
            _AT_FDCWD,
            os.fsencode(left),
            _AT_FDCWD,
            os.fsencode(right),
            _RENAME_EXCHANGE,
        )
    else:
        raise OSError(f"cannot publish a reviewed pair on {sys.platform}")
    if rc != 0:
        err = ctypes.get_errno()
        raise OSError(err, os.strerror(err), str(left), None, str(right))


def _canonical_is_generation(fix_dir: Path) -> bool:
    status = fix_dir / "status.yaml"
    if not (fix_dir / _LIVE).is_symlink() or not status.is_symlink():
        return False
    return all(
        entry.is_symlink()
        for entry in fix_dir.iterdir()
        if entry.name.startswith("patches-")
    )


def _live_points_at(fix_dir: Path, generation: Path) -> bool:
    live = fix_dir / _LIVE
    return live.is_symlink() and live.resolve() == generation.resolve()


def _new_generation(fix_dir: Path) -> Path:
    dest = fix_dir / _GENERATIONS / uuid.uuid4().hex
    dest.mkdir(parents=True)
    return dest


def _swing(fix_dir: Path, generation: Path) -> None:
    tmp = fix_dir / f".live-{generation.name}"
    _discard(tmp)
    tmp.symlink_to(Path(_GENERATIONS) / generation.name)
    os.replace(tmp, fix_dir / _LIVE)


def _copy_patches(fix_dir: Path, dest: Path) -> None:
    if not fix_dir.is_dir():
        return
    for entry in fix_dir.iterdir():
        if not entry.name.startswith("patches-"):
            continue
        target = dest / entry.name
        target.mkdir()
        if not entry.is_dir():
            continue
        for child in entry.iterdir():
            if child.is_file():
                (target / child.name).write_bytes(child.read_bytes())


def _drop_other_generations(fix_dir: Path, keep: Path) -> None:
    root = fix_dir / _GENERATIONS
    if not root.is_dir():
        return
    root_resolved = root.resolve()
    keep_resolved = keep.resolve()
    for child in root.iterdir():
        # The lock file lives beside generation directories and is not one of them.
        if not child.is_dir():
            continue
        resolved = child.resolve()
        if resolved == keep_resolved or root_resolved not in resolved.parents:
            continue
        shutil.rmtree(resolved, ignore_errors=True)


def _discard(path: Path) -> None:
    if path.is_symlink():
        path.unlink()
    elif path.is_dir():
        shutil.rmtree(path)
    elif path.exists():
        path.unlink()


def _fsync_tree(root: Path) -> None:
    for dirpath, _dirnames, filenames in os.walk(root):
        current = Path(dirpath)
        for name in filenames:
            _fsync(current / name)
        _fsync(current)
    _fsync(root.parent)


def _fsync(path: Path) -> None:
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


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
