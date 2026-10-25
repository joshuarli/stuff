from __future__ import annotations

import json
import re
from pathlib import Path

from repomod.git import GitError, run_git
from repomod.rules.actions import _validate_workflows
from repomod.rules.workflows import read_workflow_sources, workflow_files
from repomod.types import Finding, Repository, Rule, TagPredicate, operational, violation

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
    sources, problems = read_workflow_sources(root, repository)
    findings = [
        operational(repository, "gha-settings", path, message) for path, message in problems
        for path, message in problems
    ]
    for relative, source in sources:
        lines = source.splitlines()
        required = (
            (("concurrency", "group"), _GROUP),
            (("concurrency", "cancel-in-progress"), "true"),
            (("defaults", "run", "shell"), _SHELL),
        )
        for path, expected in required:
            if _mapping_value(lines, path) != expected:
                line, column = _mapping_location(lines, path)
                findings.append(
                    violation(
                        repository,
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


def _local_default_branch(root: Path) -> str | None:
    result = run_git(["symbolic-ref", "refs/remotes/origin/HEAD"], cwd=root, check=False)
    if result.returncode != 0:
        return None
    reference = result.stdout.strip()
    prefix = "refs/remotes/origin/"
    if not reference.startswith(prefix):
        return None
    branch = reference[len(prefix):]
    if not branch or any(character.isspace() for character in branch):
        return None
    return branch


_ON = re.compile(r"""^( *)(['"]?)on\2\s*:(?:[ \t]*(.*?))?$""")
_PUSH = re.compile(r"^( *)push\s*:(?:[ \t]*(.*?))?$")
_PULL_REQUEST = re.compile(r"^( *)pull_request\s*:(?:[ \t]*(.*?))?$")


def _flow_events(value: str) -> list[str] | None:
    text = value.strip()
    if not text.startswith("[") or not text.endswith("]"):
        return None
    return [item.strip().strip("'\"") for item in text[1:-1].split(",") if item.strip()]


def _trigger_violation(lines: list[str]) -> tuple[int, int, bool, bool] | None:
    for index, line in enumerate(lines):
        if not line.strip() or line.strip().startswith("#"):
            continue
        match = _ON.fullmatch(line.rstrip())
        if match is None or len(match.group(1)) != 0:
            continue
        rest = (match.group(3) or "").strip()
        if rest and not rest.startswith("#"):
            events = _flow_events(rest.split("#", 1)[0])
            if events is not None and "push" in events and "pull_request" in events:
                return index, 0, False, True
            return None
        end = _block_end(lines, index, 0)
        push = pull_request = None
        for child in range(index + 1, end):
            parsed = lines[child]
            if not parsed.strip() or parsed.lstrip().startswith("#"):
                continue
            push_match = _PUSH.fullmatch(parsed.rstrip())
            if push_match is not None and len(push_match.group(1)) > 0:
                push = child
                continue
            pull_match = _PULL_REQUEST.fullmatch(parsed.rstrip())
            if pull_match is not None and len(pull_match.group(1)) > 0:
                pull_request = child
                continue
        if push is None or pull_request is None:
            return None
        value = lines[push].split(":", 1)[1].split("#", 1)[0].strip()
        if value in {"{}", "[]"}:
            return push, len(lines[push]) - len(lines[push].lstrip()), True, False
        push_indent = len(lines[push]) - len(lines[push].lstrip())
        for child in range(push + 1, _block_end(lines, push, push_indent)):
            child_parsed = _key_value(lines[child])
            if child_parsed is not None and child_parsed[0] > push_indent and child_parsed[1] == "branches":
                return None
        return push, push_indent, True, False
    return None


def _trigger_findings(root: Path, repository: Repository) -> list[Finding]:
    sources, problems = read_workflow_sources(root, repository)
    findings = [
        operational(repository, "gha-triggers", path, message) for path, message in problems
        for path, message in problems
    ]
    default_branch = _local_default_branch(root)
    for relative, source in sources:
        lines = source.splitlines()
        trigger = _trigger_violation(lines)
        if trigger is None:
            continue
        push, indentation, mapping, _ = trigger
        if not mapping:
            findings.append(
                violation(
                    repository,
                    "gha-triggers",
                    relative,
                    "on.push shares unfiltered runs with on.pull_request; "
                    "convert on to a mapping with push scoped to the default branch",
                    line=push + 1,
                    column=indentation + 1,
                )
            )
            continue
        if default_branch is None:
            findings.append(
                violation(
                    repository,
                    "gha-triggers",
                    relative,
                    "on.push has no branch filter while on.pull_request is present; "
                    "limit push to the default branch",
                    line=push + 1,
                    column=indentation + 1,
                )
            )
            continue
        findings.append(
            violation(
                repository,
                "gha-triggers",
                relative,
                f"on.push must limit branches to [{default_branch}] "
                "while on.pull_request is present",
                fixable=True,
                line=push + 1,
                column=indentation + 1,
            )
        )
    return findings


def _scope_triggers(lines: list[str], default_branch: str) -> bool:
    trigger = _trigger_violation(lines)
    if trigger is None:
        return False
    push, push_indent, mapping, _ = trigger
    if not mapping:
        return False
    end = _block_end(lines, push, push_indent)
    for child in range(push + 1, end):
        parsed = _key_value(lines[child])
        if parsed is not None and parsed[0] > push_indent:
            child_indent = parsed[0]
            break
    else:
        child_indent = push_indent + 2
    lines[push] = f"{' ' * push_indent}push:"
    lines.insert(push + 1, f"{' ' * child_indent}branches: [{default_branch}]")
    return True


def _fix_triggers(root: Path, repository: Repository) -> dict[str, str]:
    files, problems = workflow_files(root, repository)
    if problems:
        raise GitError("; ".join(f"{path}: {message}" for path, message in problems))
    default_branch = _local_default_branch(root)
    if default_branch is None:
        return {}
    plan = {}
    for path in files:
        relative = path.relative_to(root).as_posix()
        try:
            with path.open(encoding="utf-8", newline="") as source_file:
                source = source_file.read()
        except (OSError, UnicodeError) as error:
            raise GitError(f"cannot read {relative}: {error}") from error
        lines = source.splitlines()
        if not _scope_triggers(lines, default_branch):
            continue
        newline = "\r\n" if "\r\n" in source else "\n"
        updated = newline.join(lines)
        if source.endswith(("\n", "\r")):
            updated += newline
        if updated != source:
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
        Rule(
            "gha-triggers",
            "Require scoped push triggers alongside pull_request",
            TagPredicate(),
            _trigger_findings,
            _fix_triggers,
            "chore(repomod): scope workflow push triggers",
            (".github/workflows/*.yml", ".github/workflows/*.yaml"),
            _validate_workflows,
        ),
    )
