from __future__ import annotations

import json
import re
import shutil
import subprocess
import tomllib
from dataclasses import dataclass
from pathlib import Path

from repomod.catalog import atomic_write
from repomod.github import action_tags, resolve_tag
from repomod.rules.workflows import workflow_files
from repomod.types import Finding, Repository, Rule, TagPredicate

_USES = re.compile(r"^(\s*(?:-\s*)?uses\s*:\s*)(.*?)(\r?\n)?$")
_SHA = re.compile(r"[0-9a-f]{40}\Z", re.IGNORECASE)
_VERSION = re.compile(r"v([0-9]+)\.([0-9]+)\.([0-9]+)\Z")
_VERSION_CHANNEL = re.compile(r"v([0-9]+)\Z")
_VERSION_REFERENCE = re.compile(r"v?([0-9]+)(?:\.[0-9]+){0,2}\Z")
_VERSION_COMMENT = re.compile(r"(?<![A-Za-z0-9])(v[0-9]+(?:\.[0-9]+){0,2})(?![A-Za-z0-9])")


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


def refresh_pins(path: Path | None = None, *, allow_major: bool = False) -> bool:
    target = actions_file() if path is None else path
    pins = load_pins(target)
    updated: dict[str, Pin] = {}
    for action, pin in sorted(pins.items()):
        channel = pin.channel
        tags = action_tags(action, "v")
        candidates: list[tuple[tuple[int, int, int], str]] = []
        for tag in tags:
            match = _VERSION.fullmatch(tag)
            if match is None:
                continue
            major, minor, patch = match.groups()
            parts = (int(major), int(minor), int(patch))
            if allow_major or tag.startswith(f"{pin.channel}."):
                candidates.append((parts, tag))
        if not candidates:
            raise ValueError(f"GitHub did not report a stable version for {action} in {channel}")
        selected_version, version = max(candidates)
        current_match = _VERSION.fullmatch(pin.version)
        if current_match is None:
            raise ValueError(f"invalid current approved version for {action}: {pin.version}")
        current_version = tuple(int(part) for part in current_match.groups())
        if selected_version < current_version:
            raise ValueError(
                f"GitHub reported only older versions for {action}; refusing to replace "
                f"{pin.version} with {version}"
            )
        if allow_major:
            channel = version.split(".", 1)[0]
        updated[action] = Pin(channel, version, resolve_tag(action, version))
    serialized = _render_pins(updated)
    if target.read_text(encoding="utf-8") == serialized:
        return False
    atomic_write(target, serialized)
    return True


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


def _scan_file(path: Path, repository_root: Path) -> list[ActionUse]:
    relative = path.relative_to(repository_root).as_posix()
    source = path.read_text(encoding="utf-8")
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


def _uses(root: Path) -> tuple[list[ActionUse], list[tuple[str, str]]]:
    files, problems = workflow_files(root)
    entries: list[ActionUse] = []
    for path in files:
        try:
            entries.extend(_scan_file(path, root))
        except (OSError, UnicodeError) as error:
            problems.append((path.relative_to(root).as_posix(), f"cannot read workflow: {error}"))
    return entries, problems


def _replacement(use: ActionUse, value: str, comment: str | None = None) -> str:
    tail = value
    if use.quote:
        escaped = json.dumps(value, ensure_ascii=False)[1:-1] if use.quote == '"' else value.replace("'", "''")
        tail = f"{use.quote}{escaped}{use.quote}"
    suffix = use.comment if comment is None else comment
    return f"{use.prefix}{tail}{(' ' + suffix) if suffix else ''}{use.newline}"


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
    declared_major = reference_major if reference_major is not None else comment_major
    if declared_major is not None and declared_major != pin.major:
        return (
            f"action major v{declared_major} differs from approved {pin.channel}; "
            "review the major-version change before updating the workflow"
        )
    if use.reference is None:
        return "action reference is missing; identify its intended major before pinning"
    if _SHA.fullmatch(use.reference):
        if use.reference.casefold() == pin.sha:
            return None
        if comment_major == pin.major:
            return None
        return "cannot identify the major version of this SHA; add a # vX.Y.Z comment before updating it"
    if reference_major is not None:
        return None
    return "cannot resolve this action reference to an approved major; review it before updating"


def _findings(root: Path, repository: Repository, *, version: bool) -> list[Finding]:
    pins = load_pins()
    uses, problems = _uses(root)
    rule_id = "gha-version" if version else "gha-pin"
    findings = [Finding(str(repository.id), rule_id, path, message) for path, message in problems]
    for use in uses:
        if use.action is None and use.error is None:
            continue
        if use.error is not None:
            findings.append(
                Finding(str(repository.id), rule_id, use.path, f"line {use.line_number}: {use.error}")
            )
            continue
        assert use.action is not None
        pin = pins.get(use.action)
        if pin is None:
            findings.append(
                Finding(
                    str(repository.id),
                    rule_id,
                    use.path,
                    f"line {use.line_number}: {use.action} has no approved pin in actions.toml",
                )
            )
            continue
        major_problem = _major_problem(use, pin)
        if major_problem is not None:
            findings.append(
                Finding(
                    str(repository.id),
                    rule_id,
                    use.path,
                    f"line {use.line_number}: {major_problem}",
                )
            )
            continue
        if version:
            version_comments = list(_VERSION_COMMENT.finditer(use.comment))
            if not version_comments or version_comments[-1].group(1) != pin.version:
                findings.append(
                    Finding(
                        str(repository.id),
                        rule_id,
                        use.path,
                        f"line {use.line_number}: expected approved version comment {pin.version}",
                        fixable=True,
                    )
                )
        elif use.reference is None or _SHA.fullmatch(use.reference) is None or use.reference.casefold() != pin.sha:
            findings.append(
                Finding(
                    str(repository.id),
                    rule_id,
                    use.path,
                    f"line {use.line_number}: expected {use.action}@{pin.sha} for {pin.version}",
                    fixable=True,
                )
            )
    return findings


def _fix(root: Path, repository: Repository, *, version: bool) -> dict[str, str]:
    pins = load_pins()
    uses, problems = _uses(root)
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
                    comment = use.comment
                    if version_comments:
                        match = version_comments[-1]
                        comment = comment[: match.start(1)] + pin.version + comment[match.end(1) :]
                    elif comment:
                        comment = f"{comment} # {pin.version}"
                    else:
                        comment = f"# {pin.version}"
                    lines[use.line_number - 1] = _replacement(use, use.value, comment)
            elif use.reference is None or _SHA.fullmatch(use.reference) is None or use.reference.casefold() != pin.sha:
                value = f"{use.action}@{pin.sha}"
                original_path = use.value.split("@", 1)[0]
                action_parts = original_path.split("/")
                if len(action_parts) > 2:
                    value = f"{'/'.join(action_parts)}@{pin.sha}"
                lines[use.line_number - 1] = _replacement(use, value)
        updated = "".join(lines)
        if updated != path.read_text(encoding="utf-8"):
            changes[relative] = updated
    return changes


def _validate_workflows(
    root: Path, repository: Repository, changed: tuple[str, ...]
) -> list[str]:
    if not changed:
        return []
    executable = shutil.which("actionlint")
    if executable is None:
        return [
            "actionlint is required to validate modified GitHub workflows; "
            "install actionlint"
        ]
    try:
        result = subprocess.run(
            [executable, *[str(root / path) for path in changed]],
            cwd=root,
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return ["actionlint timed out while validating modified workflows"]
    except OSError as error:
        return [f"cannot run actionlint: {error}"]
    if result.returncode == 0:
        return []
    return [
        result.stderr.strip()
        or result.stdout.strip()
        or "actionlint rejected modified workflows"
    ]


def _syntax_findings(root: Path, repository: Repository) -> list[Finding]:
    files, problems = workflow_files(root)
    findings = [Finding(str(repository.id), "gha-syntax", path, message) for path, message in problems]
    if not files:
        return findings
    executable = shutil.which("actionlint")
    if executable is None:
        return [
            *findings,
            Finding(
                str(repository.id),
                "gha-syntax",
                ".github/workflows",
                "actionlint is required to validate workflow syntax; install actionlint",
                kind="operational",
            ),
        ]
    try:
        result = subprocess.run(
            [executable, *map(str, files)],
            cwd=root,
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )
    except subprocess.TimeoutExpired:
        findings.append(
            Finding(
                str(repository.id),
                "gha-syntax",
                ".github/workflows",
                "actionlint timed out while validating workflows",
                kind="operational",
            )
        )
        return findings
    except OSError as error:
        findings.append(
            Finding(
                str(repository.id),
                "gha-syntax",
                ".github/workflows",
                f"cannot run actionlint: {error}",
                kind="operational",
            )
        )
        return findings
    if result.returncode:
        output = (
            result.stderr.strip()
            or result.stdout.strip()
            or "actionlint found invalid workflow syntax"
        )
        findings.append(Finding(str(repository.id), "gha-syntax", ".github/workflows", output))
    return findings


def action_rules() -> tuple[Rule, ...]:
    return (
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
        Rule(
            "gha-syntax",
            "Validate GitHub workflow syntax with actionlint",
            TagPredicate(),
            _syntax_findings,
            None,
            None,
            (),
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
    )
