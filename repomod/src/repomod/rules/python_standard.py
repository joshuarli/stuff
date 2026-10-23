from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import tempfile
import tomllib
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any

from repomod.catalog import atomic_write
from repomod.rules.workflows import workflow_files
from repomod.types import Finding, Repository, Rule, TagPredicate

_RULE_ID = "python-standard"
_TOOL_NAMES = ("uv", "ruff", "ty")
_PIN = re.compile(r"^(uv|ruff|ty)==([0-9]+(?:\.[0-9]+){1,3})$")
_VERSION = re.compile(r"[0-9]+(?:\.[0-9]+){1,3}\Z")
_PYTHON_FLOOR = re.compile(r">=\s*3\.15(?:\.0+)?\Z")
_SECTION = re.compile(r"^\[([^]]+)\][ \t]*(?:#.*)?(?:\r?\n)?$")
_PACKAGE_NAME = re.compile(r"^([A-Za-z0-9][A-Za-z0-9._-]*)(?:\[[^]]*\])?(.*)$")
_DEPENDENCY_LINE = re.compile(
    r"^(?P<indent>[ \t]*)(?P<quote>['\"])(?P<value>[^'\"]*)(?P=quote)"
    r"(?P<tail>[ \t]*,?[ \t]*(?:#.*)?(?:\r?\n)?)$"
)
_PYTHON_VERSION_FIELD = re.compile(
    r"^(?P<indent> *)(?:- +)?python-version[ \t]*:[ \t]*(?P<value>.*?)(?:\r?\n)?$",
    re.IGNORECASE,
)
_PYTHON_VERSION_LITERAL = re.compile(r"(?<![0-9.])3\.(?P<minor>[0-9]+)(?:\.(?P<patch>[0-9]+))?(?![0-9.])")
_IGNORED_COPY_PATHS = {
    ".git",
    ".work",
    ".venv",
    ".pytest_cache",
    ".ruff_cache",
    "__pycache__",
    "node_modules",
    "target",
}


def _finding(
    repository: Repository,
    path: str | None,
    message: str,
    *,
    fixable: bool = True,
    operational: bool = False,
) -> Finding:
    return Finding(
        str(repository.id),
        _RULE_ID,
        path,
        message,
        fixable=fixable and not operational,
        kind="operational" if operational else "violation",
    )


def _workflow_comment(value: str) -> tuple[str, str]:
    quote = ""
    for index, character in enumerate(value):
        if character in {"'", '"'}:
            if not quote:
                quote = character
            elif quote == character:
                quote = ""
        elif character == "#" and not quote and (index == 0 or value[index - 1].isspace()):
            return value[:index].rstrip(), value[index:]
    return value.rstrip(), ""


def _python_version_order(version: str) -> tuple[int, int, int]:
    parts = version.split(".")
    return 3, int(parts[1]), int(parts[2]) if len(parts) == 3 else 0


def _legacy_versions(value: str) -> tuple[str, ...]:
    versions = []
    for match in _PYTHON_VERSION_LITERAL.finditer(value):
        version = match.group(0)
        if _python_version_order(version) < (3, 15, 0) and version not in versions:
            versions.append(version)
    return tuple(versions)


def _version_scalar(value: str) -> tuple[str, str] | None:
    match = re.fullmatch(r"[ \t]*(?P<quote>['\"]?)(?P<version>3\.[0-9]+(?:\.[0-9]+)?)(?P=quote)[ \t]*", value)
    if match is None:
        return None
    return str(match.group("version")), str(match.group("quote"))


def _rewrite_ci_value(value: str) -> tuple[str | None, tuple[str, ...], bool]:
    body, comment = _workflow_comment(value)
    flow = re.fullmatch(r"(?P<prefix>[ \t]*)\[(?P<items>[^]]*)\](?P<suffix>[ \t]*)", body)
    if flow is not None:
        values = []
        for item in flow.group("items").split(","):
            scalar = _version_scalar(item)
            if scalar is None:
                versions = _legacy_versions(body)
                return None, versions, not bool(versions)
            values.append(scalar)
        legacy = tuple(
            version
            for version, _ in values
            if _python_version_order(version) < (3, 15, 0)
        )
        if not legacy:
            return None, (), True
        rewritten = []
        seen = set()
        for version, quote in values:
            updated = "3.15" if _python_version_order(version) < (3, 15, 0) else version
            order = _python_version_order(updated)
            if order in seen:
                continue
            seen.add(order)
            rewritten.append(f"{quote}{updated}{quote}")
        result = (
            f"{flow.group('prefix')}[{', '.join(rewritten)}]{flow.group('suffix')}"
            f"{(' ' + comment) if comment else ''}"
        )
        return result, tuple(dict.fromkeys(legacy)), True

    scalar = _version_scalar(body)
    if scalar is not None:
        version, quote = scalar
        if _python_version_order(version) >= (3, 15, 0):
            return None, (), True
        leading = body[: len(body) - len(body.lstrip())]
        trailing = body[len(body.rstrip()) :]
        suffix = f" {comment}" if comment else ""
        return f"{leading}{quote}3.15{quote}{trailing}{suffix}", (version,), True

    legacy = _legacy_versions(body)
    return None, legacy, not bool(legacy)


def _block_python_versions(lines: list[str], index: int) -> tuple[str, ...]:
    match = _PYTHON_VERSION_FIELD.fullmatch(lines[index])
    if match is None or match.group("value").strip():
        return ()
    key_indent = len(match.group("indent"))
    versions = []
    for line in lines[index + 1 :]:
        if line.strip() and len(line) - len(line.lstrip(" ")) <= key_indent:
            break
        item = re.match(r"^[ ]+-[ \t]*(?P<value>[^\r\n]+)", line)
        if item is not None:
            body, _ = _workflow_comment(item.group("value"))
            versions.extend(_legacy_versions(body))
    return tuple(dict.fromkeys(versions))


def _matrix_include_groups(lines: list[str]) -> list[list[tuple[int, int]]]:
    groups = []
    for matrix_index, line in enumerate(lines):
        matrix = re.match(r"^(?P<indent> *)matrix[ \t]*:[ \t]*(?:#.*)?(?:\r?\n)?$", line)
        if matrix is None:
            continue
        matrix_indent = len(matrix.group("indent"))
        include_index = None
        include_indent = 0
        for index in range(matrix_index + 1, len(lines)):
            candidate = lines[index]
            if candidate.strip() and len(candidate) - len(candidate.lstrip(" ")) <= matrix_indent:
                break
            include = re.match(r"^(?P<indent> +)include[ \t]*:[ \t]*(?:#.*)?(?:\r?\n)?$", candidate)
            if include is not None:
                include_index = index
                include_indent = len(include.group("indent"))
                break
        if include_index is None:
            continue
        rows = []
        row_start = None
        row_indent = None
        for index in range(include_index + 1, len(lines)):
            candidate = lines[index]
            indent = len(candidate) - len(candidate.lstrip(" "))
            if candidate.strip() and indent <= include_indent:
                if row_start is not None:
                    rows.append((row_start, index))
                    row_start = None
                break
            item = re.match(r"^(?P<indent> +)-[ \t]+", candidate)
            if item is None or len(item.group("indent")) <= include_indent:
                continue
            current_indent = len(item.group("indent"))
            if row_start is None:
                row_start = index
                row_indent = current_indent
            elif current_indent == row_indent:
                rows.append((row_start, index))
                row_start = index
        if row_start is not None:
            rows.append((row_start, len(lines)))
        if rows:
            groups.append(rows)
    return groups


def _deduplicate_matrix_rows(lines: list[str], changed_lines: set[int]) -> list[str]:
    removals = set()
    for rows in _matrix_include_groups(lines):
        seen: dict[tuple[str, ...], tuple[bool, tuple[int, int]]] = {}
        for start, end in rows:
            signature = tuple(line.strip() for line in lines[start:end] if line.strip())
            changed = any(index in changed_lines for index in range(start, end))
            previous = seen.get(signature)
            if previous is None:
                seen[signature] = changed, (start, end)
            elif changed or previous[0]:
                removals.update(range(start, end))
    return [line for index, line in enumerate(lines) if index not in removals]


def _updated_workflow_source(source: str) -> str:
    lines = source.splitlines(keepends=True)
    changed_lines = set()
    for index, line in enumerate(lines):
        match = _PYTHON_VERSION_FIELD.fullmatch(line)
        if match is None:
            continue
        updated, versions, fixable = _rewrite_ci_value(match.group("value"))
        if versions and not fixable:
            continue
        if updated is None:
            continue
        newline = "\r\n" if line.endswith("\r\n") else "\n" if line.endswith("\n") else ""
        lines[index] = f"{line[:match.start('value')]}{updated}{newline}"
        changed_lines.add(index)
    return "".join(_deduplicate_matrix_rows(lines, changed_lines))


def _workflow_plan(root: Path, repository: Repository) -> dict[str, str]:
    files, _ = workflow_files(root, repository)
    plans = {}
    for path in files:
        relative = path.relative_to(root).as_posix()
        try:
            source = path.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as error:
            raise RuntimeError(f"cannot read workflow {relative}: {error}") from error
        updated = _updated_workflow_source(source)
        if updated != source:
            plans[relative] = updated
    return plans


def _python_ci_findings(root: Path, repository: Repository) -> list[Finding]:
    files, problems = workflow_files(root, repository)
    findings = [
        _finding(repository, path, message, operational=True)
        for path, message in problems
    ]
    for path in files:
        relative = path.relative_to(root).as_posix()
        try:
            source = path.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as error:
            findings.append(_finding(repository, relative, f"cannot read workflow: {error}", operational=True))
            continue
        lines = source.splitlines(keepends=True)
        for index, line in enumerate(lines):
            match = _PYTHON_VERSION_FIELD.fullmatch(line)
            if match is None:
                continue
            versions, fixable = _rewrite_ci_value(match.group("value"))[1:]
            if not versions:
                versions = _block_python_versions(lines, index)
                fixable = False
            if versions:
                detail = ", ".join(versions)
                message = f"line {index + 1}: Python CI versions below 3.15: {detail}"
                if not fixable:
                    message += "; update this workflow manually"
                findings.append(
                    _finding(repository, relative, message, fixable=fixable)
                )
    return findings


def _read_toml(path: Path) -> tuple[dict[str, Any] | None, str | None]:
    try:
        document = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as error:
        return None, str(error)
    return document, None


def python_tools_file() -> Path:
    return Path(__file__).resolve().parents[3] / "python-tools.toml"


def _parse_approved_versions(source: str) -> dict[str, str]:
    try:
        document = tomllib.loads(source)
    except tomllib.TOMLDecodeError as error:
        raise ValueError(f"approved Python tool versions are invalid TOML: {error}") from error
    tools = document.get("tools")
    if set(document) != {"tools"} or not isinstance(tools, dict) or set(tools) != set(_TOOL_NAMES):
        raise ValueError("python-tools.toml must contain only [tools] with uv, ruff, and ty")
    versions: dict[str, str] = {}
    for name in _TOOL_NAMES:
        version = tools.get(name)
        if not isinstance(version, str) or _VERSION.fullmatch(version) is None:
            raise ValueError(f"python-tools.toml contains an invalid approved version for {name}")
        versions[name] = version
    return versions


def load_approved_python_tool_versions(path: Path | None = None) -> dict[str, str]:
    target = python_tools_file() if path is None else path
    if target.is_symlink():
        raise ValueError(f"approved Python tool versions are a symlink: {target}")
    try:
        source = target.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        raise ValueError(f"cannot read approved Python tool versions at {target}: {error}") from error
    return _parse_approved_versions(source)


def _version_key(version: str) -> tuple[int, int, int, int]:
    parts = [int(part) for part in version.split(".")]
    parts.extend(0 for _ in range(4 - len(parts)))
    return parts[0], parts[1], parts[2], parts[3]


def _render_approved_versions(versions: dict[str, str]) -> str:
    if set(versions) != set(_TOOL_NAMES):
        raise ValueError("approved Python tool versions must contain uv, ruff, and ty")
    if any(_VERSION.fullmatch(versions[name]) is None for name in _TOOL_NAMES):
        raise ValueError("approved Python tool versions must be stable numeric releases")
    source = "[tools]\n" + "".join(f'{name} = "{versions[name]}"\n' for name in _TOOL_NAMES)
    if _parse_approved_versions(source) != versions:
        raise ValueError("rendered Python tool versions do not match the requested approvals")
    return source


def _python_tool_refresh_plan(path: Path | None = None) -> tuple[Path, str | None]:
    target = python_tools_file() if path is None else path
    if target.is_symlink():
        raise ValueError(f"approved Python tool versions are a symlink: {target}")
    current = load_approved_python_tool_versions(target)
    latest = {name: _latest_version(name) for name in _TOOL_NAMES}
    for name in _TOOL_NAMES:
        if _version_key(latest[name]) < _version_key(current[name]):
            raise ValueError(
                f"PyPI reported only an older version of {name}; refusing to replace "
                f"{current[name]} with {latest[name]}"
            )
    source = _render_approved_versions(latest)
    if source == target.read_text(encoding="utf-8"):
        return target, None
    return target, source


def refresh_python_tool_versions(path: Path | None = None) -> bool:
    target, source = _python_tool_refresh_plan(path)
    if source is None:
        return False
    atomic_write(target, source)
    return True


def _normalize_name(value: str) -> str:
    return re.sub(r"[-_.]+", "-", value).casefold()


def _dependency_pins(document: dict[str, Any]) -> dict[str, list[str]] | None:
    groups = document.get("dependency-groups")
    if not isinstance(groups, dict):
        return {}
    dev = groups.get("dev", [])
    if not isinstance(dev, list) or any(not isinstance(item, str) for item in dev):
        return None
    pins: dict[str, list[str]] = {name: [] for name in _TOOL_NAMES}
    for item in dev:
        match = _PACKAGE_NAME.fullmatch(item)
        if match is None:
            continue
        name = _normalize_name(match.group(1))
        if name in pins:
            pins[name].append(item)
    return pins


def _exact_pin(value: str) -> str | None:
    match = _PIN.fullmatch(value)
    if match is None:
        return None
    return str(match.group(2))


def _has_python_floor(value: str) -> bool:
    return any(_PYTHON_FLOOR.fullmatch(specifier.strip()) for specifier in value.split(","))


def _python_specifier_bounds(
    value: str,
) -> tuple[tuple[tuple[int, int, int], bool] | None, tuple[tuple[int, int, int], bool] | None] | None:
    lower: tuple[tuple[int, int, int], bool] | None = None
    upper: tuple[tuple[int, int, int], bool] | None = None
    for specifier in value.split(","):
        match = re.fullmatch(
            r"(?P<operator>>=|>|<=|<|==)\s*(?P<version>3\.[0-9]+(?:\.[0-9]+)?)(?P<wildcard>\.\*)?",
            specifier.strip(),
        )
        if match is None:
            return None
        operator = str(match.group("operator"))
        version_text = str(match.group("version"))
        version = _python_version_order(version_text)
        wildcard = match.group("wildcard") is not None
        if wildcard:
            if operator != "==":
                return None
            components = [int(component) for component in version_text.split(".")]
            components[-1] += 1
            components.extend(0 for _ in range(3 - len(components)))
            upper_version = (components[0], components[1], components[2])
            if lower is None or version > lower[0] or (version == lower[0] and not lower[1]):
                lower = version, True
            if upper is None or upper_version < upper[0] or (upper_version == upper[0] and upper[1]):
                upper = upper_version, False
            continue
        if operator == "==":
            return None
        if operator in {">=", ">"}:
            inclusive = operator == ">="
            if lower is None or version > lower[0] or (version == lower[0] and lower[1] and not inclusive):
                lower = version, inclusive
        else:
            inclusive = operator == "<="
            if upper is None or version < upper[0] or (version == upper[0] and upper[1] and not inclusive):
                upper = version, inclusive
    return lower, upper


def _same_python_range(left: str, right: str) -> bool:
    if left == right:
        return True
    left_bounds = _python_specifier_bounds(left)
    return left_bounds is not None and left_bounds == _python_specifier_bounds(right)


def _updated_python_requirement(value: str | None) -> str:
    if isinstance(value, str) and _has_python_floor(value):
        return value
    if isinstance(value, str):
        specifiers = value.split(",")
        for index, specifier in enumerate(specifiers):
            if re.match(r"^(?:>=|>)\s*[0-9]+(?:\.[0-9]+){1,2}$", specifier.strip()):
                specifiers[index] = ">=3.15"
                return ",".join(specifiers)
    return ">=3.15"


def _lock_versions(document: dict[str, Any]) -> dict[str, list[str]]:
    packages = document.get("package")
    versions: dict[str, list[str]] = {name: [] for name in _TOOL_NAMES}
    if not isinstance(packages, list):
        return versions
    for package in packages:
        if not isinstance(package, dict):
            continue
        name = package.get("name")
        version = package.get("version")
        if (
            isinstance(name, str)
            and _normalize_name(name) in versions
            and isinstance(version, str)
        ):
            versions[_normalize_name(name)].append(version)
    return versions


def _python_version_pin(
    root: Path, repository: Repository
) -> tuple[Finding | None, str | None]:
    path = root / ".python-version"
    if path.is_symlink():
        return (
            _finding(repository, path.name, ".python-version is a symlink and is not followed", operational=True),
            None,
        )
    if not path.exists():
        return None, None
    if not path.is_file():
        return _finding(repository, path.name, ".python-version is not a regular file", operational=True), None
    try:
        source = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        return _finding(repository, path.name, f"cannot read .python-version: {error}", operational=True), None
    match = re.fullmatch(r"(?P<version>3\.[0-9]+(?:\.[0-9]+)?)(?P<newline>\r?\n?)", source)
    if match is None:
        return (
            _finding(repository, path.name, ".python-version must be one explicit Python 3 version", fixable=False),
            None,
        )
    version = str(match.group("version"))
    if _python_version_order(version) >= (3, 15, 0):
        return None, None
    updated = f"3.15{match.group('newline')}"
    return _finding(repository, path.name, f".python-version must be at least 3.15 (found {version})"), updated


def _python_standard_findings(root: Path, repository: Repository) -> list[Finding]:
    project_path = root / "pyproject.toml"
    if project_path.is_symlink():
        return [
            _finding(
                repository,
                project_path.name,
                "pyproject.toml is a symlink and is not followed",
                operational=True,
            )
        ]
    if not project_path.is_file():
        return [
            _finding(
                repository,
                project_path.name,
                "Python standards require a root pyproject.toml",
                fixable=False,
            )
        ]
    project, error = _read_toml(project_path)
    if project is None:
        return [_finding(repository, project_path.name, f"pyproject.toml is invalid TOML: {error}", operational=True)]

    findings = _python_ci_findings(root, repository)
    python_version_finding, _ = _python_version_pin(root, repository)
    if python_version_finding is not None:
        findings.append(python_version_finding)
    metadata = project.get("project")
    requires_python = metadata.get("requires-python") if isinstance(metadata, dict) else None
    if not isinstance(requires_python, str) or not _has_python_floor(requires_python):
        findings.append(_finding(repository, "pyproject.toml", "project.requires-python must be >=3.15"))

    tool = project.get("tool")
    ruff = tool.get("ruff") if isinstance(tool, dict) else None
    ruff_target = ruff.get("target-version") if isinstance(ruff, dict) else None
    if ruff_target != "py315":
        findings.append(_finding(repository, "pyproject.toml", "tool.ruff.target-version must be py315"))

    ty = tool.get("ty") if isinstance(tool, dict) else None
    ty_environment = ty.get("environment") if isinstance(ty, dict) else None
    ty_python = ty_environment.get("python-version") if isinstance(ty_environment, dict) else None
    if ty_python != "3.15":
        findings.append(_finding(repository, "pyproject.toml", "tool.ty.environment.python-version must be 3.15"))

    try:
        approved_versions = load_approved_python_tool_versions()
    except ValueError as error:
        approved_versions = None
        findings.append(
            _finding(
                repository,
                "python-tools.toml",
                str(error),
                operational=True,
            )
        )

    pins = _dependency_pins(project)
    if pins is None:
        findings.append(
            _finding(
                repository,
                "pyproject.toml",
                "dependency-groups.dev must be an array of string requirements",
                fixable=False,
            )
        )
        pins = {name: [] for name in _TOOL_NAMES}
    pin_versions: dict[str, str] = {}
    for name in _TOOL_NAMES:
        entries = pins[name]
        version = _exact_pin(entries[0]) if len(entries) == 1 else None
        if version is None:
            expected = f"{name}=={approved_versions[name]}" if approved_versions is not None else f"{name}==version"
            findings.append(
                _finding(
                    repository,
                    "pyproject.toml",
                    f"dev dependency {name} must be pinned as {expected}",
                )
            )
        else:
            pin_versions[name] = version
            if approved_versions is not None and version != approved_versions[name]:
                findings.append(
                    _finding(
                        repository,
                        "pyproject.toml",
                        f"dev dependency {name} must match approved {name}=={approved_versions[name]}",
                    )
                )

    lock_path = root / "uv.lock"
    if lock_path.is_symlink():
        findings.append(
            _finding(
                repository,
                lock_path.name,
                "uv.lock is a symlink and is not followed",
                operational=True,
            )
        )
        return findings
    if not lock_path.is_file():
        findings.append(_finding(repository, lock_path.name, "uv.lock is required for Python standards"))
        return findings
    lock, error = _read_toml(lock_path)
    if lock is None:
        findings.append(_finding(repository, lock_path.name, f"uv.lock is invalid TOML: {error}", operational=True))
        return findings
    lock_python = lock.get("requires-python")
    if isinstance(requires_python, str) and (
        not isinstance(lock_python, str) or not _same_python_range(requires_python, lock_python)
    ):
        findings.append(
            _finding(
                repository,
                lock_path.name,
                "uv.lock requires-python must match project.requires-python",
            )
        )
    lock_versions = _lock_versions(lock)
    for name in _TOOL_NAMES:
        expected = (
            approved_versions[name]
            if approved_versions is not None
            else pin_versions.get(name)
        )
        if expected is None:
            continue
        versions = lock_versions[name]
        if versions != [expected]:
            findings.append(
                _finding(
                    repository,
                    lock_path.name,
                    f"uv.lock {name} version must match approved {name}=={expected}",
                )
            )
    return findings


def _set_string(source: str, section: str, key: str, value: str) -> str:
    lines = source.splitlines(keepends=True)
    headers = [(index, match.group(1)) for index, line in enumerate(lines) if (match := _SECTION.fullmatch(line))]
    section_index = next((index for index, name in headers if name == section), None)
    if section_index is None:
        suffix = "" if not source or source.endswith("\n") else "\n"
        return f'{source}{suffix}\n[{section}]\n{key} = {json.dumps(value)}\n'
    section_end = next((index for index, name in headers if index > section_index), len(lines))
    assignment = re.compile(rf"^(?P<prefix>[ \t]*{re.escape(key)}[ \t]*=[ \t]*)(?P<value>.*?)(?P<newline>\r?\n)?$")
    for index in range(section_index + 1, section_end):
        match = assignment.fullmatch(lines[index])
        if match is not None:
            remainder = match.group("value")
            comment = re.search(r"[ \t]+#.*$", remainder)
            suffix = f" {comment.group(0).lstrip()}" if comment is not None else ""
            newline = match.group("newline") or ""
            lines[index] = f"{match.group('prefix')}{json.dumps(value)}{suffix}{newline}"
            return "".join(lines)
    lines.insert(section_end, f'{key} = {json.dumps(value)}\n')
    return "".join(lines)


def _dependency_entry_name(value: str) -> str | None:
    match = _PACKAGE_NAME.fullmatch(value)
    return _normalize_name(match.group(1)) if match is not None else None


def _replace_dev_group(source: str, document: dict[str, Any], versions: dict[str, str]) -> str:
    groups = document.get("dependency-groups")
    if groups is None:
        suffix = "" if not source or source.endswith("\n") else "\n"
        return (
            f"{source}{suffix}\n[dependency-groups]\ndev = [\n"
            + "".join(f'    "{name}=={versions[name]}",\n' for name in _TOOL_NAMES)
            + "]\n"
        )
    if not isinstance(groups, dict):
        raise RuntimeError("dependency-groups must be a TOML table to refresh Python tools")
    dev = groups.get("dev")
    if dev is not None and (not isinstance(dev, list) or any(not isinstance(item, str) for item in dev)):
        raise RuntimeError("dependency-groups.dev must be an array of string requirements to refresh Python tools")

    lines = source.splitlines(keepends=True)
    headers = [(index, match.group(1)) for index, line in enumerate(lines) if (match := _SECTION.fullmatch(line))]
    group_index = next((index for index, name in headers if name == "dependency-groups"), None)
    if group_index is None:
        raise RuntimeError("dependency-groups uses an unsupported TOML representation")
    group_end = next((index for index, name in headers if index > group_index), len(lines))
    assignment_index = next(
        (
            index
            for index in range(group_index + 1, group_end)
            if re.match(r"^[ \t]*dev[ \t]*=[ \t]*\[", lines[index])
        ),
        None,
    )
    if assignment_index is None:
        lines[group_end:group_end] = [
            "dev = [\n",
            *(f'    "{name}=={versions[name]}",\n' for name in _TOOL_NAMES),
            "]\n",
        ]
        return "".join(lines)

    if not re.match(r"^[ \t]*dev[ \t]*=[ \t]*\[[ \t]*(?:#.*)?(?:\r?\n)?$", lines[assignment_index]):
        inline_line = lines[assignment_index]
        inline = re.match(r"^(?P<prefix>[ \t]*dev[ \t]*=[ \t]*)\[", inline_line)
        quote = ""
        escaped = False
        comment_start = len(inline_line)
        for index, character in enumerate(inline_line):
            if quote == '"' and character == "\\" and not escaped:
                escaped = True
                continue
            if quote and character == quote and not escaped:
                quote = ""
            elif not quote and character in {'"', "'"}:
                quote = character
            elif not quote and character == "#":
                comment_start = index
                break
            escaped = False
        close = inline_line.rfind("]", 0, comment_start)
        tail = inline_line[close + 1 :] if close >= 0 else ""
        if (
            inline is None
            or close < inline.end() - 1
            or re.fullmatch(r"[ \t]*(?:#.*)?(?:\r?\n)?", tail) is None
            or not isinstance(dev, list)
        ):
            raise RuntimeError("dependency-groups.dev must be an array of string requirements to refresh Python tools")
        found: set[str] = set()
        rewritten_values = []
        for value in dev:
            name = _dependency_entry_name(value)
            if name is None or name not in versions:
                rewritten_values.append(value)
                continue
            if name in found:
                continue
            found.add(name)
            rewritten_values.append(f"{name}=={versions[name]}")
        rewritten_values.extend(
            f"{name}=={versions[name]}" for name in _TOOL_NAMES if name not in found
        )
        rendered = ", ".join(json.dumps(value, ensure_ascii=False) for value in rewritten_values)
        lines[assignment_index] = f"{inline.group('prefix')}[{rendered}]{tail}"
        return "".join(lines)
    closing_index = next(
        (index for index in range(assignment_index + 1, group_end) if re.match(r"^[ \t]*\]", lines[index])),
        None,
    )
    if closing_index is None:
        raise RuntimeError("dependency-groups.dev array is unterminated")

    found: set[str] = set()
    rewritten = []
    for line in lines[assignment_index + 1 : closing_index]:
        match = _DEPENDENCY_LINE.fullmatch(line)
        if match is None or not match.group("value"):
            rewritten.append(line)
            continue
        name = _dependency_entry_name(match.group("value"))
        if name is None or name not in versions:
            rewritten.append(line)
            continue
        comment = re.search(r"#.*", match.group("tail"))
        suffix = "," if comment is None else f", {comment.group(0)}"
        if name in found:
            if comment is not None:
                rewritten.append(f"{match.group('indent')}{comment.group(0)}\n")
            continue
        found.add(name)
        rewritten.append(
            f"{match.group('indent')}{match.group('quote')}{name}=={versions[name]}"
            f"{match.group('quote')}{suffix}\n"
        )
    rewritten.extend(
        f'    "{name}=={versions[name]}",\n' for name in _TOOL_NAMES if name not in found
    )
    lines[assignment_index + 1 : closing_index] = rewritten
    return "".join(lines)


def _updated_pyproject(source: str, versions: dict[str, str]) -> str:
    try:
        document = tomllib.loads(source)
    except tomllib.TOMLDecodeError as error:
        raise RuntimeError(f"pyproject.toml is invalid TOML: {error}") from error
    metadata = document.get("project")
    requires_python = metadata.get("requires-python") if isinstance(metadata, dict) else None
    updated = _set_string(
        source,
        "project",
        "requires-python",
        _updated_python_requirement(requires_python if isinstance(requires_python, str) else None),
    )
    updated = _set_string(updated, "tool.ruff", "target-version", "py315")
    updated = _set_string(updated, "tool.ty.environment", "python-version", "3.15")
    return _replace_dev_group(updated, document, versions)


def _latest_version(package: str) -> str:
    encoded = urllib.parse.quote(package, safe="")
    url = f"https://pypi.org/pypi/{encoded}/json"
    try:
        with urllib.request.urlopen(url, timeout=20) as response:
            metadata = json.loads(response.read())
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot refresh {package} from PyPI: {error}") from error
    info = metadata.get("info") if isinstance(metadata, dict) else None
    name = info.get("name") if isinstance(info, dict) else None
    version = info.get("version") if isinstance(info, dict) else None
    if (
        not isinstance(name, str)
        or _normalize_name(name) != _normalize_name(package)
        or not isinstance(version, str)
        or _VERSION.fullmatch(version) is None
    ):
        raise ValueError(f"PyPI returned invalid stable version metadata for {package}")
    return version


def _refreshed_lock(root: Path, pyproject: str, python_version: str | None = None) -> str:
    work_root = root / ".work"
    temporary_root = work_root / "tmp"
    for directory in (work_root, temporary_root):
        if directory.is_symlink():
            raise RuntimeError(f"temporary path is a symlink: {directory}")
    try:
        work_root.mkdir(exist_ok=True)
        if work_root.is_symlink() or not work_root.is_dir():
            raise RuntimeError(f"temporary path is not a regular directory: {work_root}")
        temporary_root.mkdir(exist_ok=True)
        if temporary_root.is_symlink() or not temporary_root.is_dir():
            raise RuntimeError(f"temporary path is not a regular directory: {temporary_root}")
    except OSError as error:
        raise RuntimeError(f"cannot create Python standards scratch path {temporary_root}: {error}") from error
    executable = shutil.which("uv")
    if executable is None:
        raise RuntimeError("uv is required to refresh the Python lockfile")
    with tempfile.TemporaryDirectory(prefix="python-standard-", dir=temporary_root) as temporary:
        work = Path(temporary)
        project_root = work / "project"
        shutil.copytree(
            root,
            project_root,
            symlinks=True,
            ignore=lambda directory, names: {name for name in names if name in _IGNORED_COPY_PATHS},
        )
        target = project_root / "pyproject.toml"
        if target.is_symlink():
            raise RuntimeError("pyproject.toml is a symlink and is not followed")
        target.write_text(pyproject, encoding="utf-8")
        if python_version is not None:
            python_version_path = project_root / ".python-version"
            if python_version_path.is_symlink():
                raise RuntimeError(".python-version is a symlink and is not followed")
            python_version_path.write_text(python_version, encoding="utf-8")
        cache = work / "cache"
        cache.mkdir()
        environment = os.environ.copy()
        environment["TMPDIR"] = str(temporary_root)
        environment["UV_CACHE_DIR"] = str(cache)
        try:
            result = subprocess.run(
                [executable, "lock", "--upgrade-package", "uv", "--upgrade-package", "ruff", "--upgrade-package", "ty"],
                cwd=project_root,
                env=environment,
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
                timeout=180,
                check=False,
            )
        except subprocess.TimeoutExpired as error:
            raise RuntimeError("uv lock timed out while refreshing Python standards") from error
        except OSError as error:
            raise RuntimeError(f"cannot run uv lock: {error}") from error
        if result.returncode:
            detail = result.stderr.strip() or result.stdout.strip() or f"exited with status {result.returncode}"
            raise RuntimeError(f"uv lock failed while refreshing Python standards: {detail}")
        lock = project_root / "uv.lock"
        if lock.is_symlink() or not lock.is_file():
            raise RuntimeError("uv lock did not produce a regular uv.lock file")
        try:
            return lock.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as error:
            raise RuntimeError(f"cannot read refreshed uv.lock: {error}") from error


def _validate_refreshed_lock(
    source: str, versions: dict[str, str], requires_python: str
) -> None:
    try:
        lock = tomllib.loads(source)
    except tomllib.TOMLDecodeError as error:
        raise RuntimeError(f"uv lock produced invalid TOML: {error}") from error
    lock_requires_python = lock.get("requires-python")
    if not isinstance(lock_requires_python, str) or not _same_python_range(
        requires_python, lock_requires_python
    ):
        raise RuntimeError("uv lock produced a lockfile with incompatible Python metadata")
    locked = _lock_versions(lock)
    for name, version in versions.items():
        if locked[name] != [version]:
            raise RuntimeError(f"uv lock did not lock the refreshed {name} version {version}")


def refresh_python_standard(root: Path, repository: Repository) -> dict[str, str]:
    project_path = root / "pyproject.toml"
    lock_path = root / "uv.lock"
    if project_path.is_symlink() or not project_path.is_file():
        raise RuntimeError("cannot refresh Python standards without a regular pyproject.toml")
    if lock_path.is_symlink():
        raise RuntimeError("uv.lock is a symlink and is not followed")
    try:
        source = project_path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        raise RuntimeError(f"cannot read pyproject.toml: {error}") from error
    python_version_finding, python_version = _python_version_pin(root, repository)
    if python_version_finding is not None and not python_version_finding.fixable:
        raise RuntimeError(python_version_finding.message)
    versions = load_approved_python_tool_versions()
    updated = _updated_pyproject(source, versions)
    lock = _refreshed_lock(root, updated, python_version)
    updated_document = tomllib.loads(updated)
    updated_metadata = updated_document.get("project")
    requires_python = updated_metadata.get("requires-python") if isinstance(updated_metadata, dict) else None
    if not isinstance(requires_python, str):
        raise RuntimeError("refreshed pyproject.toml has no Python requirement")
    _validate_refreshed_lock(lock, versions, requires_python)
    plan = {
        "pyproject.toml": updated,
        "uv.lock": lock,
        **_workflow_plan(root, repository),
    }
    if python_version is not None:
        plan[".python-version"] = python_version
    return plan


def _validate(root: Path, repository: Repository, changed: tuple[str, ...]) -> list[str]:
    return [
        finding.message
        for finding in _python_standard_findings(root, repository)
        if finding.fixable or finding.kind == "operational"
    ]


def python_standard_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            _RULE_ID,
            "Require Python 3.15 and refreshed uv, Ruff, and ty versions",
            TagPredicate(all_of=frozenset({"python"})),
            _python_standard_findings,
            refresh_python_standard,
            "chore(repomod): refresh python standards",
            (
                "pyproject.toml",
                "uv.lock",
                ".python-version",
                ".github/workflows/*.yml",
                ".github/workflows/*.yaml",
            ),
            _validate,
        ),
    )
