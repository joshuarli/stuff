from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import tomllib
import uuid
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import unquote

from repomod.catalog import write_if_changed
from repomod.git import GitError, run_git
from repomod.github import action_tags_many, resolve_tags_many
from repomod.rules.workflows import read_workflow_sources, workflow_files
from repomod.types import Finding, Repository, Rule, Severity, TagPredicate, operational, violation

_USES = re.compile(r"^(\s*(?:-\s*)?uses\s*:\s*)(.*?)(\r?\n)?$")
_SHA = re.compile(r"[0-9a-f]{40}\Z", re.IGNORECASE)
_VERSION = re.compile(r"v([0-9]+)(?:\.([0-9]+))?(?:\.([0-9]+))?\Z")
_VERSION_CHANNEL = re.compile(r"v([0-9]+)\Z")
_VERSION_REFERENCE = re.compile(r"v?([0-9]+)(?:\.[0-9]+){0,2}\Z")
_VERSION_COMMENT = re.compile(r"(?<![A-Za-z0-9])(v[0-9]+(?:\.[0-9]+){0,2})(?![A-Za-z0-9])")
_PINACT_CHECK_LINE = re.compile(r"^(?P<path>.+):(?P<line>[0-9]+):\s*(?P<reference>\S+)\s*$")
_PINACT_FILE_ERROR = re.compile(r"^(?P<path>.+?):(?P<line>[0-9]+):\s*(?P<message>.*\S)\s*$")
_PINACT_PATH_ERROR = re.compile(r"^(?P<path>.+?):\s*(?P<message>.*\S)\s*$")


@dataclass(frozen=True, slots=True)
class Pin:
    channel: str
    version: str
    sha: str

    @property
    def major(self) -> int:
        match = _VERSION_CHANNEL.fullmatch(self.channel)
        if match is None:
            raise ValueError(f"invalid approved action channel {self.channel}")
        return int(match.group(1))


@dataclass(frozen=True, slots=True)
class ActionUse:
    path: str
    line_number: int
    line: str
    prefix: str
    value: str
    quote: str
    comment: str
    newline: str
    action: str | None
    reference: str | None
    error: str | None


def actions_file() -> Path:
    return Path(__file__).resolve().parents[3] / "actions.toml"


def load_pins(path: Path | None = None) -> dict[str, Pin]:
    target = actions_file() if path is None else path
    try:
        parsed = tomllib.loads(target.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise ValueError(f"cannot read approved action pins at {target}: {error}") from error
    raw = parsed.get("actions")
    if not isinstance(raw, dict):
        raise ValueError("actions.toml must contain an [actions] table")
    pins: dict[str, Pin] = {}
    for action, details in raw.items():
        if not isinstance(action, str) or not isinstance(details, dict):
            raise ValueError("actions.toml contains an invalid action entry")
        channel = details.get("channel")
        version = details.get("version")
        sha = details.get("sha")
        channel_match = _VERSION_CHANNEL.fullmatch(channel) if isinstance(channel, str) else None
        version_match = _VERSION.fullmatch(version) if isinstance(version, str) else None
        if (
            not isinstance(channel, str)
            or not isinstance(version, str)
            or not isinstance(sha, str)
            or not _SHA.fullmatch(sha)
            or channel_match is None
            or version_match is None
            or channel_match.group(1) != version_match.group(1)
        ):
            raise ValueError(f"invalid approved pin for {action}")
        pins[action.casefold()] = Pin(channel, version, sha.lower())
    return pins


def refresh_pins(path: Path | None = None) -> bool:
    target = actions_file() if path is None else path
    pins = load_pins(target)
    versions = action_tags_many(tuple(sorted(pins)), "v")
    selected_versions: dict[str, str] = {}
    channels: dict[str, str] = {}
    for action, pin in sorted(pins.items()):
        channel = pin.channel
        candidates: list[tuple[tuple[int, int, int], str]] = []
        for tag in versions.get(action, ()):
            match = _VERSION.fullmatch(tag)
            if match is None:
                continue
            major, minor, patch = match.groups()
            parts = (int(major), int(minor or 0), int(patch or 0))
            candidates.append((parts, tag))
        if not candidates:
            raise ValueError(f"GitHub did not report a stable version for {action} in {channel}")
        selected_version, version = max(candidates)
        current_match = _VERSION.fullmatch(pin.version)
        if current_match is None:
            raise ValueError(f"invalid current approved version for {action}: {pin.version}")
        current_version = tuple(
            int(part or 0) for part in current_match.groups()
        )
        if selected_version < current_version:
            raise ValueError(
                f"GitHub reported only older versions for {action}; refusing to replace "
                f"{pin.version} with {version}"
            )
        channel = version.split(".", 1)[0]
        selected_versions[action] = version
        channels[action] = channel
    resolved = resolve_tags_many(selected_versions)
    updated = {
        action: Pin(channels[action], version, resolved[action])
        for action, version in selected_versions.items()
    }
    serialized = _render_pins(updated)
    return write_if_changed(target, serialized)


def _render_pins(pins: dict[str, Pin]) -> str:
    lines: list[str] = []
    for action, pin in sorted(pins.items()):
        lines.extend(
            [
                f'[actions."{action}"]',
                f'channel = "{pin.channel}"',
                f'version = "{pin.version}"',
                f'sha = "{pin.sha}"',
                "",
            ]
        )
    return "\n".join(lines)


def _split_comment(value: str) -> tuple[str, str]:
    quote = ""
    index = 0
    while index < len(value):
        character = value[index]
        if quote == "'" and character == "'" and index + 1 < len(value) and value[index + 1] == "'":
            index += 2
            continue
        if quote == '"' and character == "\\":
            index += 2
            continue
        if character in {"'", '"'}:
            if not quote:
                quote = character
            elif quote == character:
                quote = ""
        elif character == "#" and not quote and (index == 0 or value[index - 1].isspace()):
            return value[:index].rstrip(), value[index:]
        index += 1
    return value.rstrip(), ""


def _decode_scalar(value: str) -> tuple[str, str, str | None]:
    scalar = value.strip()
    if not scalar:
        return "", "", "uses value is empty"
    if scalar[0] in {"'", '"'}:
        quote = scalar[0]
        if len(scalar) < 2 or scalar[-1] != quote:
            return "", quote, "uses value has an unterminated quote"
        raw = scalar[1:-1]
        if quote == "'":
            raw = raw.replace("''", "'")
        else:
            try:
                decoded = json.loads(scalar)
            except json.JSONDecodeError:
                return "", quote, "uses value has invalid quoted syntax"
            if not isinstance(decoded, str):
                return "", quote, "uses value must be a quoted string"
            raw = decoded
        return raw, quote, None
    if any(character.isspace() for character in scalar):
        return "", "", "uses value must be a single YAML scalar"
    return scalar, "", None


def _action_reference(value: str) -> tuple[str | None, str | None, str | None]:
    if value.startswith(("./", "docker://")):
        return None, None, None
    if "${{" in value or "}}" in value:
        return None, None, "dynamic uses references cannot be safely resolved"
    action_path, separator, reference = value.rpartition("@")
    if not separator:
        action_path = value
        reference = None
    parts = action_path.split("/")
    if len(parts) < 2 or not parts[0] or not parts[1]:
        return None, reference, "external action must use OWNER/REPOSITORY@REF"
    action = "/".join(parts[:2])
    return action.casefold(), reference, None


def _scan_source(relative: str, source: str) -> list[ActionUse]:
    uses: list[ActionUse] = []
    block_indent: int | None = None
    for line_number, line in enumerate(source.splitlines(keepends=True), 1):
        body = line.rstrip("\r\n")
        indent = len(body) - len(body.lstrip(" "))
        if block_indent is not None:
            if body.strip() and indent > block_indent:
                continue
            block_indent = None
        match = _USES.match(line)
        if not match:
            if re.match(r"^\s*(?:-\s*)?(?:run|script)\s*:\s*[|>][+-]?[0-9]*\s*(?:#.*)?(?:\r?\n)?$", line):
                block_indent = indent
            continue
        prefix, tail, newline = match.groups()
        newline = newline or ""
        scalar, comment = _split_comment(tail)
        value, quote, error = _decode_scalar(scalar)
        action, reference, reference_error = _action_reference(value) if error is None else (None, None, None)
        uses.append(
            ActionUse(
                relative,
                line_number,
                line,
                prefix,
                value,
                quote,
                comment,
                newline,
                action,
                reference,
                error or reference_error,
            )
        )
    return uses


def _scan_file(path: Path, repository_root: Path) -> list[ActionUse]:
    return _scan_source(
        path.relative_to(repository_root).as_posix(), path.read_text(encoding="utf-8")
    )


def _uses(root: Path, repository: Repository) -> tuple[list[ActionUse], list[tuple[str, str]]]:
    sources, problems = read_workflow_sources(root, repository)
    entries: list[ActionUse] = []
    for relative, source in sources:
        entries.extend(_scan_source(relative, source))
    return entries, problems


def _replacement(use: ActionUse, value: str, comment: str | None = None) -> str:
    tail = value
    if use.quote:
        escaped = json.dumps(value, ensure_ascii=False)[1:-1] if use.quote == '"' else value.replace("'", "''")
        tail = f"{use.quote}{escaped}{use.quote}"
    suffix = use.comment if comment is None else comment
    return f"{use.prefix}{tail}{(' ' + suffix) if suffix else ''}{use.newline}"


def _versioned_comment(comment: str, version: str) -> str:
    version_comments = list(_VERSION_COMMENT.finditer(comment))
    if version_comments:
        match = version_comments[-1]
        return comment[: match.start(1)] + version + comment[match.end(1) :]
    return f"{comment} # {version}" if comment else f"# {version}"


def _comment_major(use: ActionUse) -> int | None:
    comments = list(_VERSION_COMMENT.finditer(use.comment))
    if len(comments) > 1:
        return None
    if comments:
        match = _VERSION_REFERENCE.fullmatch(comments[-1].group(1))
        if match is not None:
            return int(match.group(1))
    return None


def _reference_major(use: ActionUse) -> int | None:
    if use.reference is None or _SHA.fullmatch(use.reference) is not None:
        return None
    match = _VERSION_REFERENCE.fullmatch(use.reference)
    return int(match.group(1)) if match is not None else None


def _major_problem(use: ActionUse, pin: Pin) -> str | None:
    if len(_VERSION_COMMENT.findall(use.comment)) > 1:
        return "multiple version labels make the action's major version ambiguous"
    comment_major = _comment_major(use)
    reference_major = _reference_major(use)
    if comment_major is not None and reference_major is not None and comment_major != reference_major:
        return (
            f"version comment v{comment_major} conflicts with action reference "
            f"v{reference_major}; review the major-version change"
        )
    if use.reference is None:
        return "action reference is missing; identify its intended major before pinning"
    if _SHA.fullmatch(use.reference):
        if use.reference.casefold() == pin.sha:
            return None
        if comment_major is not None:
            return None
        return "cannot identify the major version of this SHA; add a # vX.Y.Z comment before updating it"
    if reference_major is not None:
        return None
    return "cannot resolve this action reference to an approved major; review it before updating"


def _findings(root: Path, repository: Repository, *, version: bool) -> list[Finding]:
    pins = load_pins()
    uses, problems = _uses(root, repository)
    rule_id = "gha-version" if version else "gha-pin"
    findings = [violation(repository, rule_id, path, message) for path, message in problems]
    for use in uses:
        if use.action is None and use.error is None:
            continue
        if use.error is not None:
            findings.append(
                violation(repository, rule_id, use.path, f"line {use.line_number}: {use.error}")
            )
            continue
        assert use.action is not None
        pin = pins.get(use.action)
        if pin is None:
            findings.append(
                violation(
                    repository,
                    rule_id,
                    use.path,
                    f"line {use.line_number}: {use.action} has no approved pin in actions.toml",
                )
            )
            continue
        major_problem = _major_problem(use, pin)
        if major_problem is not None:
            findings.append(
                violation(
                    repository,
                    rule_id,
                    use.path,
                    f"line {use.line_number}: {use.action}: {major_problem}",
                )
            )
            continue
        if version:
            version_comments = list(_VERSION_COMMENT.finditer(use.comment))
            if not version_comments or version_comments[-1].group(1) != pin.version:
                findings.append(
                    violation(
                        repository,
                        rule_id,
                        use.path,
                        f"line {use.line_number}: {use.action}: expected approved version comment "
                        f"{pin.version}",
                        fixable=True,
                    )
                )
        elif use.reference is None or _SHA.fullmatch(use.reference) is None or use.reference.casefold() != pin.sha:
            findings.append(
                violation(
                    repository,
                    rule_id,
                    use.path,
                    f"line {use.line_number}: {use.action}: expected "
                    f"{use.action}@{pin.sha} for {pin.version}",
                    fixable=True,
                )
            )
    return findings


def _fix(root: Path, repository: Repository, *, version: bool) -> dict[str, str]:
    pins = load_pins()
    uses, problems = _uses(root, repository)
    if problems:
        return {}
    grouped: dict[str, list[ActionUse]] = {}
    for use in uses:
        if use.action is not None:
            grouped.setdefault(use.path, []).append(use)
    changes: dict[str, str] = {}
    for relative, entries in grouped.items():
        path = root / relative
        lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
        for use in entries:
            pin = pins.get(use.action or "")
            if pin is None or use.error is not None or _major_problem(use, pin) is not None:
                continue
            if version:
                version_comments = list(_VERSION_COMMENT.finditer(use.comment))
                if not version_comments or version_comments[-1].group(1) != pin.version:
                    lines[use.line_number - 1] = _replacement(
                        use, use.value, _versioned_comment(use.comment, pin.version)
                    )
            elif use.reference is None or _SHA.fullmatch(use.reference) is None or use.reference.casefold() != pin.sha:
                value = f"{use.action}@{pin.sha}"
                original_path = use.value.split("@", 1)[0]
                action_parts = original_path.split("/")
                if len(action_parts) > 2:
                    value = f"{'/'.join(action_parts)}@{pin.sha}"
                lines[use.line_number - 1] = _replacement(
                    use, value, _versioned_comment(use.comment, pin.version)
                )
        updated = "".join(lines)
        if updated != path.read_text(encoding="utf-8"):
            changes[relative] = updated
    return changes


def _validate_workflows(
    root: Path, repository: Repository, changed: tuple[str, ...]
) -> list[str]:
    workflow_paths = tuple(
        relative
        for relative in changed
        if Path(relative).name.casefold() in {"action.yml", "action.yaml"}
        or (
            Path(relative).parent == Path(".github") / "workflows"
            and Path(relative).suffix.casefold() in {".yml", ".yaml"}
        )
    )
    if not workflow_paths:
        return []
    executable = shutil.which("pinact")
    if executable is None:
        return [
            "pinact is required to validate modified GitHub Actions files; "
            "install pinact"
        ]
    try:
        result = subprocess.run(
            [
                executable,
                "lint",
                "--profile=correctness",
                "--format=json",
                "--catalog",
                str(actions_file()),
                "--",
                *[str(root / path) for path in workflow_paths],
            ],
            cwd=root,
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return ["pinact timed out while validating modified GitHub Actions files"]
    except OSError as error:
        return [f"cannot run pinact: {error}"]
    if result.returncode == 0:
        return []
    if result.returncode != 1:
        return [result.stderr.strip() or result.stdout.strip() or "pinact rejected modified files"]
    try:
        findings = json.loads(result.stdout)
    except json.JSONDecodeError:
        findings = None
    if isinstance(findings, list):
        messages = [
            f"{finding.get('filepath', 'file')}:{finding.get('line', 1)}: "
            f"{finding.get('id', 'pinact')}: {finding.get('message', 'invalid file')}"
            for finding in findings
            if isinstance(finding, dict) and _finding_severity(finding) == "error"
        ]
        if messages:
            return messages
        return []
    return [result.stderr.strip() or result.stdout.strip() or "pinact rejected modified files"]


def _finding_severity(finding: dict[str, object]) -> Severity:
    severity = finding.get("severity")
    if severity == "warning":
        return "warning"
    if severity == "info":
        return "info"
    return "error"


def _action_tool_files(
    root: Path, repository: Repository
) -> tuple[list[Path], list[tuple[str, str]]]:
    workflows, problems = workflow_files(root, repository)
    files = set(workflows)
    if "fork" in repository.tags:
        return sorted(files, key=lambda path: path.relative_to(root).as_posix().casefold()), problems
    skipped_directories = {
        ".cache",
        ".git",
        ".repomod",
        ".tox",
        ".venv",
        ".work",
        "__pycache__",
        "build",
        "dist",
        "node_modules",
        "target",
        "testdata",
        "vendor",
    }
    for directory, names, filenames in os.walk(root, followlinks=False):
        current = Path(directory)
        names[:] = [
            name
            for name in names
            if name not in skipped_directories and not (current / name).is_symlink()
        ]
        for filename in filenames:
            if filename.casefold() in {"action.yml", "action.yaml"}:
                path = current / filename
                if not path.is_symlink() and path.is_file():
                    files.add(path)
    for relative in (".github/dependabot.yml", ".github/dependabot.yaml", ".github/workflows/actions.lock"):
        path = root / relative
        if path.is_symlink():
            problems.append((relative, "GitHub Actions configuration symlinks are not followed"))
        elif path.is_file():
            files.add(path)
    return sorted(files, key=lambda path: path.relative_to(root).as_posix().casefold()), problems


def _tool_file_arguments(root: Path, files: list[Path]) -> list[str]:
    return [str(path.relative_to(root)) for path in files]


def _tool_relative_path(root: Path, value: str) -> str | None:
    value = unquote(value)
    path = Path(value)
    candidate = path if path.is_absolute() else root / path
    try:
        resolved = candidate.resolve(strict=False)
        if not resolved.is_relative_to(root.resolve()):
            return None
        return candidate.relative_to(root).as_posix()
    except (OSError, ValueError):
        return None


def _pinact_lint_findings(
    root: Path, repository: Repository, files: list[Path]
) -> list[Finding]:
    rule_id = "gha-syntax"
    workflow_and_action_files = [
        path
        for path in files
        if path.name.casefold() in {"action.yml", "action.yaml"}
        or (
            path.parent == root / ".github" / "workflows"
            and path.suffix.casefold() in {".yml", ".yaml"}
        )
    ]
    if not workflow_and_action_files:
        return []
    executable = shutil.which("pinact")
    if executable is None:
        return [
            operational(
                repository,
                rule_id,
                ".github/workflows",
                "pinact is required for GitHub Actions checks; install pinact",
            )
        ]
    try:
        result = subprocess.run(
            [
                executable,
                "lint",
                "--profile=correctness",
                "--format=json",
                "--catalog",
                str(actions_file()),
                "--",
                *_tool_file_arguments(root, workflow_and_action_files),
            ],
            cwd=root,
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return [
            operational(
                repository,
                rule_id,
                ".github/workflows",
                "pinact timed out while checking GitHub Actions files",
            )
        ]
    except OSError as error:
        return [
            operational(
                repository,
                rule_id,
                ".github/workflows",
                f"cannot run pinact: {error}",
            )
        ]
    if result.returncode not in {0, 1}:
        return [
            operational(
                repository,
                rule_id,
                ".github/workflows",
                result.stderr.strip() or result.stdout.strip() or f"pinact exited {result.returncode}",
            )
        ]
    try:
        parsed = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        return [
            operational(
                repository,
                rule_id,
                ".github/workflows",
                f"pinact returned invalid JSON: {error}",
            )
        ]
    if not isinstance(parsed, list):
        return [
            operational(
                repository,
                rule_id,
                ".github/workflows",
                "pinact returned an invalid finding list",
            )
        ]
    findings = []
    for item in parsed:
        if not isinstance(item, dict):
            return [
                operational(
                    repository,
                    rule_id,
                    ".github/workflows",
                    "pinact returned an invalid finding record",
                )
            ]
        item_rule = item.get("id")
        if item_rule == "concurrency-limits":
            continue
        file_value = item.get("filepath")
        relative = _tool_relative_path(root, file_value) if isinstance(file_value, str) else None
        if relative is None:
            return [
                operational(
                    repository,
                    rule_id,
                    ".github/workflows",
                    "pinact reported a path outside the repository",
                )
            ]
        message = item.get("message")
        line_number = item.get("line")
        location = f"line {line_number}: " if isinstance(line_number, int) else ""
        findings.append(
            violation(
                repository,
                rule_id,
                relative,
                f"{location}pinact {item_rule}: {message if isinstance(message, str) else 'finding'}",
                severity=_finding_severity(item),
                fixable=isinstance(item_rule, str) and item.get("fixable") is True,
            )
        )
    return findings


def _pinact_findings(
    root: Path, repository: Repository, files: list[Path]
) -> list[Finding]:
    rule_id = "gha-syntax"
    workflow_and_action_files = [
        path
        for path in files
        if path.name.casefold() in {"action.yml", "action.yaml"}
        or path.parent == root / ".github" / "workflows"
    ]
    if not workflow_and_action_files:
        return []
    executable = shutil.which("pinact")
    if executable is None:
        return [
            operational(
                repository,
                rule_id,
                ".github/workflows",
                "pinact is required for GitHub Actions checks; install pinact",
            )
        ]
    try:
        result = subprocess.run(
            [
                executable,
                "run",
                "--check",
                "--",
                *_tool_file_arguments(root, workflow_and_action_files),
            ],
            cwd=root,
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return [
            operational(
                repository,
                rule_id,
                ".github/workflows",
                "pinact timed out while checking action pins",
            )
        ]
    except OSError as error:
        return [
            operational(
                repository,
                rule_id,
                ".github/workflows",
                f"cannot run pinact: {error}",
            )
        ]
    if result.returncode == 0:
        return []
    if result.returncode == 1:
        output = result.stderr.strip() or result.stdout.strip()
        if not output:
            return [
                operational(
                    repository,
                    rule_id,
                    ".github/workflows",
                    "pinact reported unpinned actions without details",
                )
            ]
        findings = []
        for line in output.splitlines():
            match = _PINACT_CHECK_LINE.match(line.strip())
            if match is None:
                return [
                    operational(
                        repository,
                        rule_id,
                        ".github/workflows",
                        f"pinact returned invalid check output: {line.strip()}",
                    )
                ]
            relative = _tool_relative_path(root, match.group("path"))
            if relative is None:
                return [
                    operational(
                        repository,
                        rule_id,
                        ".github/workflows",
                        "pinact reported a path outside the repository",
                    )
                ]
            line_number = int(match.group("line"))
            reference = match.group("reference")
            findings.append(
                violation(
                    repository,
                    rule_id,
                    relative,
                    f"line {line_number}: pinact unpinned-action: {reference} is not pinned",
                    fixable=True,
                )
            )
        return findings
    if result.returncode == 2:
        output = result.stderr.strip() or result.stdout.strip()
        if not output:
            return [
                operational(
                    repository,
                    rule_id,
                    ".github/workflows",
                    "pinact exited 2 without details",
                )
            ]
        findings = []
        for line in output.splitlines():
            detail = line.strip().removeprefix("pinact: ").strip()
            match = _PINACT_FILE_ERROR.match(detail)
            if match is not None:
                relative = _tool_relative_path(root, match.group("path"))
                if relative is None:
                    return [
                        operational(
                            repository,
                            rule_id,
                            ".github/workflows",
                            "pinact reported a path outside the repository",
                        )
                    ]
                findings.append(
                    violation(
                        repository,
                        rule_id,
                        relative,
                        f"line {match.group('line')}: pinact: {match.group('message')}",
                    )
                )
                continue
            path_match = _PINACT_PATH_ERROR.match(detail)
            if path_match is not None:
                relative = _tool_relative_path(root, path_match.group("path"))
                if relative is not None and (root / relative).exists():
                    findings.append(
                        violation(
                            repository,
                            rule_id,
                            relative,
                            f"pinact: {path_match.group('message')}",
                        )
                    )
                    continue
            return [
                operational(
                    repository,
                    rule_id,
                    ".github/workflows",
                    detail or line.strip(),
                )
            ]
        return findings
    return [
        operational(
            repository,
            rule_id,
            ".github/workflows",
            result.stderr.strip() or result.stdout.strip() or f"pinact exited {result.returncode}",
        )
    ]


def _changed_tool_paths(root: Path, allowed: set[str]) -> dict[str, str]:
    output = run_git(
        ["status", "--porcelain=v1", "--untracked-files=all", "-z"], cwd=root
    ).stdout
    changed: set[str] = set()
    records = output.split("\0")
    index = 0
    while index < len(records):
        record = records[index]
        index += 1
        if not record:
            continue
        if len(record) < 4:
            raise GitError("Git returned malformed status for the GitHub Actions fixer")
        state = record[:2]
        relative = record[3:]
        if "D" in state or "R" in state or "C" in state:
            raise GitError(f"GitHub Actions fixer removed or renamed {relative}")
        if relative not in allowed:
            raise GitError(f"GitHub Actions fixer changed an undeclared file: {relative}")
        changed.add(relative)
        if "R" in state or "C" in state:
            index += 1
    plan = {}
    for relative in sorted(changed, key=str.casefold):
        path = root / relative
        if path.is_symlink() or not path.is_file():
            raise GitError(f"GitHub Actions fixer changed a non-regular file: {relative}")
        try:
            plan[relative] = path.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as error:
            raise GitError(f"cannot read GitHub Actions fixer output {relative}: {error}") from error
    return plan


def _general_actions_fix(root: Path, repository: Repository) -> dict[str, str]:
    files, problems = _action_tool_files(root, repository)
    if problems:
        raise GitError("; ".join(f"{path}: {message}" for path, message in problems))
    pinact_files = [
        path
        for path in files
        if path.name.casefold() in {"action.yml", "action.yaml"}
        or (
            path.parent == root / ".github" / "workflows"
            and path.suffix.casefold() in {".yml", ".yaml"}
        )
    ]
    if not pinact_files:
        return {}
    pinact = shutil.which("pinact")
    if pinact is None:
        raise GitError("pinact is required to fix GitHub Actions files")
    scratch_parent = Path(os.environ.get(
        "TMPDIR", str(Path(__file__).resolve().parents[3] / ".work" / "tmp")
    ))
    scratch_parent.mkdir(parents=True, exist_ok=True)
    scratch = scratch_parent / f"gha-{uuid.uuid4().hex}"
    if scratch.exists() or scratch.is_symlink():
        raise GitError(f"GitHub Actions scratch worktree already exists: {scratch}")
    added = False
    failure: BaseException | None = None
    plan: dict[str, str] = {}
    try:
        run_git(["worktree", "add", "--detach", str(scratch), "HEAD"], cwd=root)
        added = True
        relative_files = _tool_file_arguments(root, pinact_files)
        result = subprocess.run(
            [
                pinact,
                "fix",
                "--catalog",
                str(actions_file()),
                "--",
                *relative_files,
            ],
            cwd=scratch,
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
        if result.returncode not in {0, 1}:
            raise GitError(
                result.stderr.strip() or result.stdout.strip() or f"pinact exited {result.returncode}"
            )
        allowed = {path.relative_to(root).as_posix() for path in files}
        plan = _changed_tool_paths(scratch, allowed)
    except BaseException as error:
        failure = error
    cleanup_error = None
    if added or scratch.exists():
        try:
            result = run_git(
                ["worktree", "remove", "--force", str(scratch)], cwd=root, check=False
            )
            if result.returncode:
                cleanup_error = GitError(
                    result.stderr.strip() or "could not remove GitHub Actions scratch worktree"
                )
        except BaseException as error:
            cleanup_error = error
    if failure is not None:
        if cleanup_error is not None:
            raise GitError(f"{failure}; scratch cleanup failed: {cleanup_error}") from failure
        raise failure
    if cleanup_error is not None:
        raise GitError(f"scratch cleanup failed: {cleanup_error}")
    return plan


def _syntax_findings(root: Path, repository: Repository) -> list[Finding]:
    files, problems = _action_tool_files(root, repository)
    findings = [
        operational(repository, "gha-syntax", path, message) for path, message in problems
    ]
    if not files:
        return findings
    findings.extend(_pinact_lint_findings(root, repository, files))
    return findings


def action_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            "gha-syntax",
            "Check and repair GitHub Actions files with pinact",
            TagPredicate(),
            _syntax_findings,
            _general_actions_fix,
            "chore(repomod): fix github actions",
            (
                ".github/workflows/*.yml",
                ".github/workflows/*.yaml",
                ".github/workflows/actions.lock",
                ".github/dependabot.yml",
                ".github/dependabot.yaml",
                "**/action.yml",
                "**/action.yaml",
                "action.yml",
                "action.yaml",
            ),
            _validate_workflows,
        ),
        Rule(
            "gha-version",
            "Keep GitHub Action version comments aligned with approved pins",
            TagPredicate(),
            lambda root, repo: _findings(root, repo, version=True),
            lambda root, repo: _fix(root, repo, version=True),
            "chore(repomod): update github action versions",
            (".github/workflows/*.yml", ".github/workflows/*.yaml"),
            _validate_workflows,
        ),
        Rule(
            "gha-pin",
            "Pin external GitHub Actions to approved full commit SHAs",
            TagPredicate(),
            lambda root, repo: _findings(root, repo, version=False),
            lambda root, repo: _fix(root, repo, version=False),
            "chore(repomod): pin github actions",
            (".github/workflows/*.yml", ".github/workflows/*.yaml"),
            _validate_workflows,
        ),
    )
