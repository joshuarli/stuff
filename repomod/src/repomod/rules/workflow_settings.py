from __future__ import annotations

import json
import re
from pathlib import Path

from repomod.rules.workflows import workflow_files
from repomod.types import Finding, Repository, Rule, TagPredicate

_KEY = re.compile(r"^( *)([A-Za-z0-9_-]+):(?:[ \t]*(.*))?$")
_GROUP = "${{ github.workflow }}-${{ github.head_ref || github.run_id }}"
_SHELL = "bash -euo pipefail {0}"

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
                        line=line,
                        column=column,
                        code=_CODES[path],
                    )
                )
    return findings


def workflow_settings_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            "gha-settings",
            "Require concurrency cancellation and bash run shell defaults",
            TagPredicate(),
            _workflow_settings,
            None,
            None,
            (),
        ),
    )
