"""Upgrade macos-14 GitHub Actions runners to macos-15.

GitHub is retiring the macOS 14 runner images, so macos-14 can't just be kept.
Two cases need more than a string replace:
  - a matrix that already lists macos-15 would end up with a duplicate value
    (actionlint rejects it), so the macos-14 entry is dropped instead;
  - comments are left alone, so a comment mentioning macos-14 doesn't produce a PR.
"""

import io
import os
import re
from pathlib import Path
from typing import Any

# The bare label and the larger-runner variants (macos-14-large, macos-14-xlarge),
# but not unrelated strings like "macos-14.5" or "xmacos-14".
LABEL_RE = re.compile(r"(?<![\w.-])macos-14(?P<variant>-large|-xlarge)?(?![\w.-])")
COMMENT_RE = re.compile(r"(?:^|\s)#")
FLOW_LIST_RE = re.compile(r"\[[^\[\]]*\]")
BLOCK_ITEM_RE = re.compile(
    r"""^(?P<indent>\s*)-\s+["']?(?P<label>macos-14(?:-large|-xlarge)?)["']?\s*(?:#.*)?$"""
)


def accept_repo(repo: dict[str, Any], config: dict[str, Any]) -> bool:
    return True


def upgraded(label: str) -> str:
    return LABEL_RE.sub(lambda m: f"macos-15{m['variant'] or ''}", label)


def unquote(item: str) -> str:
    return item.strip().strip("\"'")


def upgrade_flow_list(match: re.Match[str]) -> str:
    """[macos-14, macos-15] -> [macos-15]; [macos-14, ubuntu] -> [macos-15, ubuntu]."""
    items = match.group()[1:-1].split(",")
    present = {unquote(item) for item in items}
    kept: list[str] = []
    for item in items:
        label = unquote(item)
        if LABEL_RE.fullmatch(label):
            if upgraded(label) in present:
                continue
            item = upgraded(item)
        kept.append(item)
    if kept and kept[0] != items[0]:
        # The first item was dropped; keep the list's original opening padding.
        kept[0] = items[0][: len(items[0]) - len(items[0].lstrip())] + kept[0].lstrip()
    return f"[{','.join(kept)}]"


def sibling_labels(lines: list[str], index: int, indent: str) -> set[str]:
    """Scalar values of the block-sequence items next to lines[index]."""
    labels: set[str] = set()
    for step in (-1, 1):
        i = index + step
        while 0 <= i < len(lines):
            line = lines[i]
            stripped = line.strip()
            line_indent = line[: len(line) - len(line.lstrip())]
            if stripped and not stripped.startswith("#"):
                if len(line_indent) < len(indent):
                    break
                if len(line_indent) == len(indent):
                    if not stripped.startswith("- "):
                        break
                    labels.add(unquote(stripped[2:].split(" #")[0]))
            i += step
    return labels


def upgrade_line(lines: list[str], index: int) -> str | None:
    """The upgraded line, or None if the line should be removed."""
    line = lines[index]
    block = BLOCK_ITEM_RE.match(line.rstrip("\n"))
    if block and upgraded(block["label"]) in sibling_labels(
        lines, index, block["indent"]
    ):
        return None

    comment = COMMENT_RE.search(line)
    split = comment.end() - 1 if comment else len(line)
    code, rest = line[:split], line[split:]
    code = FLOW_LIST_RE.sub(upgrade_flow_list, code)
    return upgraded(code) + rest


def upgrade_text(text: str) -> str:
    lines = text.splitlines(keepends=True)
    out = [upgrade_line(lines, i) for i in range(len(lines))]
    return "".join(line for line in out if line is not None)


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


def upgrade_file(fp: Path, repo: Path) -> bool:
    text = read_text_nofollow(repo, fp)
    new_text = upgrade_text(text)
    if new_text == text:
        return False
    write_text_nofollow(repo, fp, new_text)
    return True


def run(repo: Path, config: dict[str, Any]) -> bool:
    github_dir = repo / ".github"
    if not github_dir.exists():
        return False

    changed = False
    for pattern in ("*.yml", "*.yaml"):
        for fp in github_dir.rglob(pattern):
            if outside_clone_or_symlink(repo, fp):
                continue
            if upgrade_file(fp, repo):
                changed = True
    return changed
