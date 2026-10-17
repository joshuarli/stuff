"""Pin GitHub Actions to full-length commit SHAs."""

import io
import os
import re
import subprocess
from functools import cache
from pathlib import Path
from typing import Any

from repomod.utils import SUBPROCESS_TIMEOUT, git_transport_limiter, retry

ACTION_RE = re.compile(
    r"""(?<=uses: )["']?(?P<action>[^@\s"']+)@(?P<ref>[^#\s"']+)["']?"""
)


@retry()
@cache
def resolve_ref(repo: str, ref: str) -> str:
    """Resolve a GitHub ref (tag or branch) to a full-length commit SHA.

    Results are cached in-memory for the duration of the run so the same
    ref (e.g., actions/checkout@v4) isn't resolved 277 times. Cache is
    not persisted to disk — tags are mutable and we always want the latest.
    """
    if len(ref) == 40:
        try:
            int(ref, 16)
        except ValueError:
            pass
        else:
            return ref

    cmd = (
        "git",
        "ls-remote",
        "--exit-code",
        f"https://github.com/{repo}",
        "--",
        ref,
    )
    with git_transport_limiter:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            check=True,
            timeout=SUBPROCESS_TIMEOUT,
        )
    for line in result.stdout.splitlines():
        sha, refname = line.split()
        if refname in (f"refs/tags/{ref}", f"refs/heads/{ref}"):
            return sha
    msg = f"unknown ref: {repo}@{ref}"
    raise ValueError(msg)


def accept_repo(repo: dict[str, Any], config: dict[str, Any]) -> bool:
    return True


def extract_repo(action: str) -> str:
    parts = action.split("/")
    return f"{parts[0]}/{parts[1]}"


def _required_open_flag(name: str) -> int:
    """Require a POSIX open flag (macOS and Linux, any architecture)."""
    flag = getattr(os, name, None)
    if not isinstance(flag, int):
        msg = f"{name} is required to write a workflow without following a symlink"
        raise OSError(msg)
    return flag


_NOFOLLOW = _required_open_flag("O_NOFOLLOW")
_CLOEXEC = _required_open_flag("O_CLOEXEC")
_DIRECTORY = _required_open_flag("O_DIRECTORY")


def outside_clone_or_symlink(repo: Path, path: Path) -> bool:
    """True when a symlink sits under repo or the file resolves outside it.

    Path.write_text follows a symlinked file, so an absolute symlink to a file
    outside the clone would be overwritten while git diff stays empty.
    """
    current = repo
    for part in path.relative_to(repo).parts:
        if part in ("", ".", ".."):
            return True
        current = current / part
        if current.is_symlink():
            return True
    return not path.resolve().is_relative_to(repo.resolve())


def _open_under(repo: Path, path: Path, flags: int) -> int:
    """Open path beneath repo. Each component below repo uses O_NOFOLLOW.

    O_NOFOLLOW on one open applies only to the final component, so a symlink
    in an earlier component would still redirect the fd. The clone directory
    itself is opened without O_NOFOLLOW: that path may be reached through a
    symlink.
    """
    parts = path.relative_to(repo).parts
    if not parts or any(part in ("", ".", "..") for part in parts):
        msg = f"refusing to open {path} under {repo}"
        raise OSError(msg)
    dir_fd = os.open(repo, os.O_RDONLY | _DIRECTORY | _CLOEXEC)
    try:
        for part in parts[:-1]:
            next_fd = os.open(
                part,
                os.O_RDONLY | _DIRECTORY | _NOFOLLOW | _CLOEXEC,
                dir_fd=dir_fd,
            )
            os.close(dir_fd)
            dir_fd = next_fd
        return os.open(parts[-1], flags | _NOFOLLOW | _CLOEXEC, dir_fd=dir_fd)
    finally:
        os.close(dir_fd)


def read_text_nofollow(repo: Path, path: Path) -> str:
    encoding = io.text_encoding(None)

    def opener(_name: str, flags: int) -> int:
        return _open_under(repo, path, flags)

    with open(path, encoding=encoding, opener=opener) as handle:
        return handle.read()


def write_text_nofollow(repo: Path, path: Path, text: str) -> None:
    encoding = io.text_encoding(None)

    def opener(_name: str, flags: int) -> int:
        return _open_under(repo, path, flags)

    with open(path, "w", encoding=encoding, opener=opener) as handle:
        handle.write(text)


def pin_file(fp: Path, repo: Path) -> bool:
    lines = read_text_nofollow(repo, fp).splitlines(keepends=True)
    changed = False
    newlines: list[str] = []

    for line in lines:
        m = ACTION_RE.search(line)
        if m:
            action, ref = m["action"], m["ref"]
            sha = resolve_ref(extract_repo(action), ref=ref)
            if sha != ref:
                prefix = line[: m.start()]
                suffix = line[m.end() :]
                suffix = re.sub(r"\s*#.*", "", suffix.rstrip()) + "\n"
                line = f"{prefix}{action}@{sha} # {ref}{suffix}"
                changed = True
        newlines.append(line)

    if changed:
        write_text_nofollow(repo, fp, "".join(newlines))
    return changed


def run(repo: Path, config: dict[str, Any]) -> bool:
    github_dir = repo / ".github"
    if not github_dir.exists():
        return False

    changed = False
    for pattern in ("*.yml", "*.yaml"):
        for fp in github_dir.rglob(pattern):
            if outside_clone_or_symlink(repo, fp):
                continue
            if pin_file(fp, repo):
                changed = True
    return changed
