from __future__ import annotations

import json
import re
from pathlib import Path

from repomod.git import GitError
from repomod.rules.actions import _validate_workflows
from repomod.rules.workflows import workflow_files
from repomod.types import Finding, Repository, Rule, TagPredicate

_KEY = re.compile(r"^( *)([A-Za-z0-9_-]+):(?:[ \t]*(.*))?$")
_GROUP = "${{ github.workflow }}-${{ github.head_ref || github.run_id }}"
_SHELL = "bash -euo pipefail {0}"
_GROUP_VALUE = json.dumps(_GROUP)

_CODES = {
    ("concurrency", "group"): "G001",
    ("concurrency", "cancel-in-progress"): "G002",
    ("defaults", "run", "shell"): "G003",
}


def _key_value(line: str) -> tuple[int, str, str] | None:
    value = line.rstrip()
    if not value.strip() or value.lstrip().startswith("#"):
        return None
    match = _KEY.fullmatch(value)
    if match is None:
        return None
    indentation = len(match.group(1))
    scalar = match.group(3) or ""
    scalar = re.sub(r"\s+#.*$", "", scalar).strip()
    if len(scalar) >= 2 and scalar[0] == scalar[-1] == "'":
        scalar = scalar[1:-1].replace("''", "'")
    elif len(scalar) >= 2 and scalar[0] == scalar[-1] == '"':
        try:
            decoded = json.loads(scalar)
        except json.JSONDecodeError:
            pass
        else:
            if isinstance(decoded, str):
                scalar = decoded
    key = match.group(2)
    assert isinstance(key, str)
    return indentation, key, scalar


def _block_end(lines: list[str], start: int, indentation: int) -> int:
    for index in range(start + 1, len(lines)):
        parsed = _key_value(lines[index])
        if lines[index].strip() and not lines[index].lstrip().startswith("#"):
            current_indentation = len(lines[index]) - len(lines[index].lstrip(" "))
            if current_indentation <= indentation:
                return index
        if parsed is not None and parsed[0] <= indentation:
            return index
    return len(lines)


def _mapping_value(lines: list[str], path: tuple[str, ...]) -> str | None:
    start = 0
    end = len(lines)
    indentation = 0
    value: str | None = None
    for depth, part in enumerate(path):
        found = None
        for line_index in range(start, end):
            parsed = _key_value(lines[line_index])
            if parsed is not None and parsed[0] == indentation and parsed[1] == part:
                found = line_index
                value = parsed[2]
                break
        if found is None:
            return None
        end = _block_end(lines, found, indentation)
        if depth != len(path) - 1:
            child_indentations = []
            for line in lines[found + 1 : end]:
                parsed = _key_value(line)
                if parsed is not None and parsed[0] > indentation:
                    child_indentations.append(parsed[0])
            if not child_indentations:
                return None
            start = found + 1
            indentation = min(child_indentations)
    return value


def _mapping_location(lines: list[str], path: tuple[str, ...]) -> tuple[int, int]:
    start = 0
    end = len(lines)
    indentation = 0
    parent: tuple[int, int] | None = None
    for depth, part in enumerate(path):
        found: int | None = None
        found_indentation = 0
        for line_index in range(start, end):
            parsed = _key_value(lines[line_index])
            if parsed is not None and parsed[0] == indentation and parsed[1] == part:
                found = line_index
                found_indentation = parsed[0]
                break
        if found is None:
            return parent or (1, 1)
        parent = (found + 1, found_indentation + 1)
        end = _block_end(lines, found, indentation)
        if depth != len(path) - 1:
            child_indentations = []
            for line in lines[found + 1 : end]:
                parsed = _key_value(line)
                if parsed is not None and parsed[0] > indentation:
                    child_indentations.append(parsed[0])
            if not child_indentations:
                return parent
            start = found + 1
            indentation = min(child_indentations)
    return parent or (1, 1)


def _workflow_settings(root: Path, repository: Repository) -> list[Finding]:
    workflows, problems = workflow_files(root, repository)
    findings = [
        Finding(str(repository.id), "gha-settings", path, message, kind="operational")
        for path, message in problems
    ]
    for workflow in workflows:
        relative = workflow.relative_to(root).as_posix()
        try:
            lines = workflow.read_text(encoding="utf-8").splitlines()
        except (OSError, UnicodeError) as error:
            findings.append(
                Finding(
                    str(repository.id),
                    "gha-settings",
                    relative,
                    f"cannot read workflow: {error}",
                    kind="operational",
                )
            )
            continue
        required = (
            (("concurrency", "group"), _GROUP),
            (("concurrency", "cancel-in-progress"), "true"),
            (("defaults", "run", "shell"), _SHELL),
        )
        for path, expected in required:
            if _mapping_value(lines, path) != expected:
                line, column = _mapping_location(lines, path)
                findings.append(
                    Finding(
                        str(repository.id),
                        "gha-settings",
                        relative,
                        f"{'.'.join(path)} must be {expected!r}",
                        fixable=True,
                        line=line,
                        column=column,
                        code=_CODES[path],
                    )
                )
    return findings


def _top_key_index(lines: list[str], key: str) -> int | None:
    for index, line in enumerate(lines):
        parsed = _key_value(line)
        if parsed is not None and parsed[0] == 0 and parsed[1] == key:
            return index
    return None


def _child_indentation(lines: list[str], start: int, end: int, parent: int) -> int:
    indents = [
        parsed[0]
        for line in lines[start + 1 : end]
        if (parsed := _key_value(line)) is not None and parsed[0] > parent
    ]
    return min(indents) if indents else parent + 2


def _child_index(lines: list[str], start: int, end: int, parent: int, key: str) -> int | None:
    for index in range(start + 1, end):
        parsed = _key_value(lines[index])
        if parsed is not None and parsed[0] > parent and parsed[1] == key:
            return index
    return None


def _insert_top_block(lines: list[str], block: list[str]) -> None:
    jobs = _top_key_index(lines, "jobs")
    if jobs is None:
        if lines and lines[-1].strip():
            lines.append("")
        lines.extend(block)
        return
    lines[jobs:jobs] = [*block, ""]


def _ensure_concurrency(lines: list[str]) -> bool:
    index = _top_key_index(lines, "concurrency")
    if index is None:
        _insert_top_block(lines, ["concurrency:", f"  group: {_GROUP_VALUE}", "  cancel-in-progress: true"])
        return True
    changed = False
    parsed = _key_value(lines[index])
    assert parsed is not None
    if parsed[2]:
        lines[index] = "concurrency:"
        changed = True
    end = _block_end(lines, index, 0)
    group = _child_index(lines, index, end, 0, "group")
    if group is None:
        lines.insert(end, f"{' ' * _child_indentation(lines, index, end, 0)}group: {_GROUP_VALUE}")
        changed = True
    else:
        current = _key_value(lines[group])
        assert current is not None
        if current[2] != _GROUP:
            lines[group] = f"{' ' * current[0]}group: {_GROUP_VALUE}"
            changed = True
    end = _block_end(lines, index, 0)
    cancel = _child_index(lines, index, end, 0, "cancel-in-progress")
    if cancel is None:
        lines.insert(end, f"{' ' * _child_indentation(lines, index, end, 0)}cancel-in-progress: true")
        changed = True
    else:
        current = _key_value(lines[cancel])
        assert current is not None
        if current[2] != "true":
            lines[cancel] = f"{' ' * current[0]}cancel-in-progress: true"
            changed = True
    return changed


def _ensure_defaults(lines: list[str]) -> bool:
    index = _top_key_index(lines, "defaults")
    if index is None:
        _insert_top_block(lines, ["defaults:", "  run:", f"    shell: {_SHELL}"])
        return True
    changed = False
    parsed = _key_value(lines[index])
    assert parsed is not None
    if parsed[2]:
        lines[index] = "defaults:"
        changed = True
    end = _block_end(lines, index, 0)
    first = _child_indentation(lines, index, end, 0)
    run = _child_index(lines, index, end, 0, "run")
    if run is None:
        lines.insert(end, f"{' ' * first}run:")
        lines.insert(end + 1, f"{' ' * (first + 2)}shell: {_SHELL}")
        return True
    current = _key_value(lines[run])
    assert current is not None
    if current[2]:
        lines[run] = f"{' ' * current[0]}run:"
        changed = True
    run_end = _block_end(lines, run, first)
    shell = _child_index(lines, run, run_end, first, "shell")
    if shell is None:
        lines.insert(run_end, f"{' ' * _child_indentation(lines, run, run_end, first)}shell: {_SHELL}")
        changed = True
    else:
        current = _key_value(lines[shell])
        assert current is not None
        if current[2] != _SHELL:
            lines[shell] = f"{' ' * current[0]}shell: {_SHELL}"
            changed = True
    return changed


def _enforce_source(source: str) -> str:
    newline = "\r\n" if "\r\n" in source else "\n"
    trailing = source.endswith(("\n", "\r"))
    lines = source.splitlines()
    _ensure_concurrency(lines)
    _ensure_defaults(lines)
    result = newline.join(lines)
    if trailing:
        result += newline
    return result


def _fix_workflow_settings(root: Path, repository: Repository) -> dict[str, str]:
    files, problems = workflow_files(root, repository)
    if problems:
        raise GitError("; ".join(f"{path}: {message}" for path, message in problems))
    plan = {}
    for path in files:
        relative = path.relative_to(root).as_posix()
        try:
            with path.open(encoding="utf-8", newline="") as source_file:
                source = source_file.read()
        except (OSError, UnicodeError) as error:
            raise GitError(f"cannot read {relative}: {error}") from error
        updated = _enforce_source(source)
        if updated != source:
            if not updated.endswith(("\n", "\r")):
                updated += "\r\n" if "\r\n" in source else "\n"
            plan[relative] = updated
    return plan


def workflow_settings_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            "gha-settings",
            "Require concurrency cancellation and bash run shell defaults",
            TagPredicate(),
            _workflow_settings,
            _fix_workflow_settings,
            "chore(repomod): enforce workflow settings",
            (".github/workflows/*.yml", ".github/workflows/*.yaml"),
            _validate_workflows,
        ),
    )
