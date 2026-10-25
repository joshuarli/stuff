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
from repomod.rules.workflows import workflow_files
from repomod.types import Finding, Repository, Rule, Severity, TagPredicate, operational, violation

_SHA = re.compile(r"[0-9a-f]{40}\Z", re.IGNORECASE)
_VERSION = re.compile(r"v([0-9]+)(?:\.([0-9]+))?(?:\.([0-9]+))?\Z")
_VERSION_CHANNEL = re.compile(r"v([0-9]+)\Z")


@dataclass(frozen=True, slots=True)
class Pin:
    channel: str
    version: str
    sha: str


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
                "--profile=default",
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
    )
