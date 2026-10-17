"""Upgrade ubuntu-20.04 / ubuntu-22.04 (and -arm) runners to ubuntu-24.04.

Only runner labels are rewritten. Not touched:
  - `container:` / image references such as ghcr.io/.../test-ubuntu-20.04-amd64
    (the "-" before "ubuntu" excludes them): those are old-glibc build environments
    that repos pick on purpose, independent of the host runner;
  - comments;
  - larger-runner labels such as ubuntu-22.04-4core, which are custom names;
  - files that declare themselves generated ("Do not edit this YAML file..."): an
    edit would be overwritten on the next regeneration or fail the repo's
    up-to-date check, and the label really lives in the generator's template.

Two cases need more than a string replace, both because actionlint rejects duplicate
matrix values:
  - a matrix that already lists the 24.04 label has the old entry dropped;
  - a matrix listing several old labels (20.04 and 22.04) collapses to one 24.04 entry.
This covers one-line lists ([a, b]), block lists (- a) and multi-line lists with one
item per line. Anything more exotic can still end up with a duplicate; the pipeline's
actionlint check then fails that repo instead of letting a broken workflow through.

Deliberately self-contained, with no shared helpers: the fix hash covers only this
directory, so behavior shared via another module could change without a new hash.
"""

import re
from pathlib import Path
from typing import Any

TARGET = "ubuntu-24.04"
# The bare label and the -arm variant, not e.g. "ubuntu-22.04.1" or "xubuntu-22.04".
LABEL_RE = re.compile(r"(?<![\w.-])ubuntu-(?:20\.04|22\.04)(?P<arm>-arm)?(?![\w.-])")
COMMENT_RE = re.compile(r"(?:^|\s)#")
# Generated files say so in their header, e.g. test workflows generated from a
# template; an edit would be overwritten.
GENERATED_RE = re.compile(r"do not edit|generated (?:automatically|by)", re.IGNORECASE)
GENERATED_HEADER_LINES = 5
FLOW_LIST_RE = re.compile(r"\[[^\[\]]*\]")
BLOCK_ITEM_RE = re.compile(
    r"""^(?P<indent>\s*)-\s+["']?(?P<label>ubuntu-(?:20\.04|22\.04)(?:-arm)?)["']?\s*(?:#.*)?$"""
)


def accept_repo(repo: dict[str, Any], config: dict[str, Any]) -> bool:
    return True


def upgraded(text: str) -> str:
    return LABEL_RE.sub(lambda m: f"{TARGET}{m['arm'] or ''}", text)


def unquote(item: str) -> str:
    return item.strip().strip("\"'")


def upgrade_flow_list(match: re.Match[str]) -> str:
    """[ubuntu-20.04, ubuntu-22.04] -> [ubuntu-24.04]; keeps unrelated entries."""
    items = match.group()[1:-1].split(",")
    present = {unquote(item) for item in items}
    emitted: set[str] = set()
    kept: list[str] = []
    for item in items:
        label = unquote(item)
        if LABEL_RE.fullmatch(label):
            target = upgraded(label)
            if target in present or target in emitted:
                continue
            emitted.add(target)
            item = upgraded(item)
        kept.append(item)
    if kept and kept[0] != items[0]:
        # The first item was dropped or rewritten; keep the list's opening padding.
        kept[0] = items[0][: len(items[0]) - len(items[0].lstrip())] + kept[0].lstrip()
    return f"[{','.join(kept)}]"


def siblings(lines: list[str], index: int, indent: str, step: int) -> list[str]:
    """Scalar values of block-sequence items next to lines[index], in one direction."""
    labels: list[str] = []
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
                labels.append(unquote(stripped[2:].split(" #")[0]))
        i += step
    return labels


def is_redundant_block_item(lines: list[str], index: int) -> bool:
    """Whether upgrading this `- ubuntu-2x.04` item would duplicate a sibling."""
    block = BLOCK_ITEM_RE.match(lines[index].rstrip("\n"))
    if not block:
        return False
    target = upgraded(block["label"])
    before = siblings(lines, index, block["indent"], -1)
    after = siblings(lines, index, block["indent"], 1)
    already_there = target in before or target in after
    # An earlier old label becomes the target first, so this one is the duplicate.
    earlier_becomes_target = any(
        LABEL_RE.fullmatch(label) and upgraded(label) == target for label in before
    )
    return already_there or earlier_becomes_target


def split_comment(line: str) -> tuple[str, str]:
    comment = COMMENT_RE.search(line)
    split = comment.end() - 1 if comment else len(line)
    return line[:split], line[split:]


def redundant_multiline_list_items(lines: list[str]) -> tuple[set[int], set[int]]:
    """Redundant old labels in `key: [` ... `]` lists, one item per line.

    Returns (lines to drop, closing lines to trim down to just the bracket): a
    redundant item on the closing line can't take its line with it.
    """
    drop: set[int] = set()
    trim: set[int] = set()
    i = 0
    while i < len(lines):
        code, _ = split_comment(lines[i])
        if not code.rstrip().endswith("["):
            i += 1
            continue
        items: list[tuple[int, str, bool]] = []
        j = i + 1
        while j < len(lines):
            item_code, _ = split_comment(lines[j])
            closing = "]" in item_code
            item = item_code.split("]")[0].strip().rstrip(",").strip()
            if item:
                items.append((j, unquote(item), closing))
            if closing:
                break
            j += 1
        labels = {label for _, label, _ in items}
        emitted: set[str] = set()
        for index, label, closing in items:
            if not LABEL_RE.fullmatch(label):
                continue
            target = upgraded(label)
            if target in labels or target in emitted:
                (trim if closing else drop).add(index)
            else:
                emitted.add(target)
        i = j + 1
    return drop, trim


def upgrade_line(lines: list[str], index: int) -> str | None:
    """The upgraded line, or None if the line should be removed."""
    if is_redundant_block_item(lines, index):
        return None
    code, rest = split_comment(lines[index])
    code = FLOW_LIST_RE.sub(upgrade_flow_list, code)
    return upgraded(code) + rest


def upgrade_text(text: str) -> str:
    lines = text.splitlines(keepends=True)
    drop, trim = redundant_multiline_list_items(lines)
    out: list[str | None] = []
    for i, line in enumerate(lines):
        if i in drop:
            out.append(None)
        elif i in trim:
            out.append(re.sub(r"^(\s*)[^\]]*\]", r"\1]", line))
        else:
            out.append(upgrade_line(lines, i))
    return "".join(line for line in out if line is not None)


def is_generated(text: str) -> bool:
    header = text.splitlines()[:GENERATED_HEADER_LINES]
    return any(GENERATED_RE.search(line) for line in header)


def upgrade_file(fp: Path) -> bool:
    text = fp.read_text()
    if is_generated(text):
        return False
    new_text = upgrade_text(text)
    if new_text == text:
        return False
    fp.write_text(new_text)
    return True


def run(repo: Path, config: dict[str, Any]) -> bool:
    github_dir = repo / ".github"
    if not github_dir.exists():
        return False

    changed = False
    for pattern in ("*.yml", "*.yaml"):
        for fp in github_dir.rglob(pattern):
            if upgrade_file(fp):
                changed = True
    return changed
