from __future__ import annotations

import fcntl
import os
import re
import shutil
import subprocess
import tempfile
from collections.abc import Generator
from contextlib import contextmanager
from pathlib import Path
from urllib.parse import urlsplit

from repomod.types import GitStatus, RepoId

GIT_TIMEOUT = 120
_SHA = re.compile(r"[0-9a-f]{40}\Z", re.IGNORECASE)


class GitError(RuntimeError):
    pass


@contextmanager
def repository_lock(root: Path, repo: RepoId) -> Generator[None]:
    lock_root = root / ".repomod" / "locks"
    current = root
    for part in (".repomod", "locks", repo.owner):
        current = current / part
        if current.is_symlink():
            raise GitError(f"repomod lock path contains a symlink: {current}")
    lock_root.joinpath(repo.owner).mkdir(parents=True, exist_ok=True)
    lock_path = lock_root / repo.owner / f"{repo.name}.lock"
    if lock_path.is_symlink():
        raise GitError(f"repomod lock path contains a symlink: {lock_path}")
    descriptor = os.open(lock_path, os.O_CREAT | os.O_RDWR, 0o600)
    try:
        fcntl.flock(descriptor, fcntl.LOCK_EX)
        yield
    finally:
        fcntl.flock(descriptor, fcntl.LOCK_UN)
        os.close(descriptor)


def run_git(
    arguments: list[str],
    cwd: Path | None = None,
    *,
    input_text: str | None = None,
    check: bool = True,
    timeout: int = GIT_TIMEOUT,
) -> subprocess.CompletedProcess[str]:
    command = ["git", *arguments]
    try:
        result = subprocess.run(
            command,
            cwd=cwd,
            input=input_text,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=timeout,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise GitError(f"git {arguments[0]} failed: {error}") from error
    if check and result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise GitError(f"git {' '.join(arguments[:2])} failed: {detail}")
    return result


def expected_origin(repo: RepoId, remote_url: str) -> bool:
    value = remote_url.strip()
    if not value or any(character.isspace() for character in value):
        return False
    try:
        if value.startswith("git@github.com:"):
            path = value.removeprefix("git@github.com:")
        elif value.startswith(("ssh://", "https://")):
            parsed = urlsplit(value)
            if parsed.hostname is None or parsed.hostname.casefold() != "github.com":
                return False
            if parsed.query or parsed.fragment or parsed.port is not None:
                return False
            if parsed.scheme == "ssh" and parsed.username not in {"git", None}:
                return False
            if parsed.scheme == "https" and parsed.username is not None:
                return False
            path = parsed.path.lstrip("/")
        else:
            return False
    except ValueError:
        return False
    path = path.removesuffix(".git").removesuffix("/")
    parts = path.split("/")
    return (
        len(parts) == 2
        and all(parts)
        and "/".join(parts).casefold() == str(repo).casefold()
    )


def origin_url(path: Path) -> str | None:
    result = run_git(["config", "--get-all", "remote.origin.url"], cwd=path, check=False)
    if result.returncode != 0:
        return None
    urls = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    return urls[0] if len(urls) == 1 else None


def tracked_paths(path: Path, pathspecs: tuple[str, ...]) -> tuple[str, ...]:
    result = run_git(["ls-files", "-z", "--", *pathspecs], cwd=path, check=False)
    if result.returncode != 0:
        raise GitError(result.stderr.strip() or "git ls-files failed")
    return tuple(sorted((value for value in result.stdout.split("\0") if value), key=str.casefold))


def root_ignore_matches(path: Path, candidates: tuple[str, ...]) -> frozenset[str]:
    root_ignore = path / ".gitignore"
    if root_ignore.is_symlink() or not root_ignore.is_file() or not candidates:
        return frozenset[str]()
    result = run_git(
        ["check-ignore", "--no-index", "--verbose", "-z", "--stdin"],
        cwd=path,
        input_text="".join(candidate + "\0" for candidate in candidates),
        check=False,
    )
    if result.returncode == 1:
        return frozenset[str]()
    if result.returncode != 0:
        raise GitError(result.stderr.strip() or "git check-ignore failed")
    fields = result.stdout.split("\0")
    if len(fields) % 4 != 1 or fields[-1]:
        raise GitError("git check-ignore returned invalid verbose output")
    try:
        root_ignore_resolved = root_ignore.resolve(strict=True)
    except OSError:
        return frozenset[str]()
    matched: set[str] = set()
    for index in range(0, len(fields) - 1, 4):
        source = Path(fields[index])
        if not source.is_absolute():
            source = path / source
        try:
            if source.resolve(strict=True) == root_ignore_resolved:
                matched.add(fields[index + 3])
        except OSError:
            continue
    return frozenset(matched)


def is_git_checkout(path: Path) -> bool:
    git_path = path / ".git"
    if not path.exists() or path.is_symlink() or git_path.is_symlink() or not git_path.exists():
        return False
    result = run_git(["rev-parse", "--is-inside-work-tree"], cwd=path, check=False)
    return result.returncode == 0 and result.stdout.strip() == "true"


def _porcelain_counts(output: str) -> tuple[int, int, int]:
    staged = 0
    unstaged = 0
    untracked = 0
    records = output.split("\0")
    index = 0
    while index < len(records):
        record = records[index]
        index += 1
        if not record or record.startswith("# "):
            continue
        kind = record[0]
        if kind == "?":
            untracked += 1
            continue
        if kind == "2":
            fields = record.split(" ", 9)
            status = fields[1] if len(fields) > 1 else ".."
            if status[0] != ".":
                staged += 1
            if status[1] != ".":
                unstaged += 1
            index += 1
            continue
        if kind in {"1", "u"}:
            fields = record.split(" ", 9)
            status = fields[1] if len(fields) > 1 else ".."
            if status[0] != ".":
                staged += 1
            if status[1] != ".":
                unstaged += 1
    return staged, unstaged, untracked


def status(path: Path, repo: RepoId) -> GitStatus:
    if not path.exists():
        return GitStatus(str(repo), path, False, False, None, False, 0, 0, 0, None, None, None, False)
    if not is_git_checkout(path):
        return GitStatus(
            str(repo), path, True, False, None, False, 0, 0, 0, None, None, None, False,
            "directory is not a valid Git checkout",
        )
    head = run_git(["symbolic-ref", "--quiet", "--short", "HEAD"], cwd=path, check=False)
    detached = head.returncode != 0
    branch = None if detached else head.stdout.strip()
    porcelain = run_git(["status", "--porcelain=v2", "--branch", "-z"], cwd=path).stdout
    staged, unstaged, untracked = _porcelain_counts(porcelain)
    upstream: str | None = None
    ahead: int | None = None
    behind: int | None = None
    headers = []
    for record in porcelain.split("\0"):
        if not record.startswith("# "):
            break
        headers.append(record)
    for header in headers:
        if header.startswith("# branch.upstream "):
            upstream = header.removeprefix("# branch.upstream ")
        elif header.startswith("# branch.ab "):
            counts = header.removeprefix("# branch.ab ").split()
            if len(counts) == 2:
                ahead, behind = int(counts[0].removeprefix("+")), int(counts[1].removeprefix("-"))
    remote = origin_url(path)
    origin_valid = remote is not None and expected_origin(repo, remote)
    origin_error = None
    if remote is None:
        origin_error = "origin is missing or has multiple URLs"
    elif not origin_valid:
        origin_error = "origin does not match the catalogued GitHub repository"
    return GitStatus(
        str(repo), path, True, True, branch, detached, staged, unstaged, untracked,
        ahead, behind, upstream, origin_valid, origin_error,
    )


def shallow(path: Path) -> bool:
    result = run_git(["rev-parse", "--is-shallow-repository"], cwd=path, check=False)
    return result.returncode == 0 and result.stdout.strip() == "true"


def fetch_default(path: Path, branch: str, deepen: int | None = None) -> None:
    arguments = ["fetch", "--no-tags"]
    if deepen is not None:
        arguments.append("--depth=1" if deepen == 1 else f"--deepen={deepen}")
    arguments.extend(["origin", f"+refs/heads/{branch}:refs/remotes/origin/{branch}"])
    run_git(arguments, cwd=path)


def is_ancestor(path: Path, older: str, newer: str) -> bool:
    result = run_git(["merge-base", "--is-ancestor", older, newer], cwd=path, check=False)
    if result.returncode not in {0, 1}:
        raise GitError(result.stderr.strip() or "git merge-base failed")
    return result.returncode == 0


def _checkout_matches(path: Path, branch: str, expected_head: str) -> bool:
    output = run_git(
        ["status", "--porcelain=v2", "--branch", "-z"], cwd=path
    ).stdout
    current_branch = None
    current_head = None
    for record in output.split("\0"):
        if record.startswith("# branch.head "):
            current_branch = record.removeprefix("# branch.head ")
        elif record.startswith("# branch.oid "):
            current_head = record.removeprefix("# branch.oid ")
    return (
        current_branch == branch
        and current_head == expected_head
        and not any(_porcelain_counts(output))
    )


def safe_fast_forward(path: Path, branch: str, start_head: str | None = None) -> str:
    start = start_head or run_git(["rev-parse", "HEAD"], cwd=path).stdout.strip()
    target = f"refs/remotes/origin/{branch}"
    if is_ancestor(path, start, target):
        if not _checkout_matches(path, branch, start):
            return "checkout changed during sync"
        run_git(["merge", "--ff-only", target], cwd=path)
        return "fast-forwarded"
    if is_ancestor(path, target, start):
        return "local branch is ahead of the remote default branch"
    if shallow(path):
        for depth in (32, 64, 128, 256, 512):
            fetch_default(path, branch, depth)
            if is_ancestor(path, start, target):
                if not _checkout_matches(path, branch, start):
                    return "checkout changed during sync"
                run_git(["merge", "--ff-only", target], cwd=path)
                return "fast-forwarded"
            if is_ancestor(path, target, start):
                return "local branch is ahead of the remote default branch"
            if not shallow(path):
                break
    return "local and remote default branches have diverged or ancestry is unknown"


def clone_repository(path: Path, repo: RepoId, remote_url: str, default_branch: str) -> None:
    if path.exists() or path.is_symlink():
        raise GitError(f"checkout path already exists: {path}")
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.parent.is_symlink():
        raise GitError(f"checkout parent is a symlink: {path.parent}")
    temporary_parent = Path(tempfile.mkdtemp(prefix=".repomod-clone-", dir=path.parent))
    staging = temporary_parent / "checkout"
    try:
        run_git(
            [
                "clone",
                "--filter=blob:none",
                "--depth=1",
                "--no-tags",
                "--single-branch",
                f"--branch={default_branch}",
                remote_url,
                str(staging),
            ]
        )
        actual_origin = origin_url(staging)
        if actual_origin is None or not expected_origin(repo, actual_origin):
            raise GitError(f"cloned origin does not match catalogued repository {repo}")
        if path.exists() or path.is_symlink():
            raise GitError(f"checkout path appeared during clone: {path}")
        os.rename(staging, path)
    except BaseException:
        shutil.rmtree(temporary_parent, ignore_errors=True)
        raise
    temporary_parent.rmdir()


def clone_url(repo: RepoId) -> str:
    return f"https://github.com/{repo.owner}/{repo.name}.git"


def head(path: Path) -> str:
    value = run_git(["rev-parse", "HEAD"], cwd=path).stdout.strip()
    if not _SHA.fullmatch(value):
        raise GitError("git returned an invalid HEAD object id")
    return value
