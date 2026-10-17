"""Pin GitHub Actions to full-length commit SHAs."""

import re
import subprocess
from functools import cache
from pathlib import Path
from typing import Any

from repomod.utils import SUBPROCESS_TIMEOUT, github_rate_limiter, retry

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

    cmd = ("git", "ls-remote", "--exit-code", f"https://github.com/{repo}", ref)
    with github_rate_limiter:
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


def pin_file(fp: Path) -> bool:
    lines = fp.read_text().splitlines(keepends=True)
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
        fp.write_text("".join(newlines))
    return changed


def run(repo: Path, config: dict[str, Any]) -> bool:
    github_dir = repo / ".github"
    if not github_dir.exists():
        return False

    changed = False
    for pattern in ("*.yml", "*.yaml"):
        for fp in github_dir.rglob(pattern):
            if pin_file(fp):
                changed = True
    return changed
