from __future__ import annotations

import re
from pathlib import Path

from repomod.rules.lint import _LINT_WORKFLOW_NAMES
from repomod.rules.workflow_settings import _flow_events
from repomod.rules.workflows import read_workflow_sources
from repomod.types import Finding, Repository, Rule, TagPredicate, operational, violation

_ON = re.compile(r"""^(['"]?)on\1\s*:[ \t]*(.*)$""")
_EVENT_KEY = re.compile(r"""^( +)['"]?([A-Za-z0-9_-]+)['"]?\s*:""")
_RUN = re.compile(r"^( *)(?:- +)?run\s*:[ \t]*(.*)$")
_MAKE_LINT_COMMAND = re.compile(r"(?:^|&&|\|\||;)\s*make\s+(?:-\S+\s+)*lint(?![\w-])")
_BLOCK_SCALAR = re.compile(r"^[|>][+-]?\d*$")


def _significant(line: str) -> bool:
    return bool(line.strip()) and not line.lstrip().startswith("#")


def _trigger_events(lines: list[str]) -> frozenset[str] | None:
    for index, line in enumerate(lines):
        match = _ON.fullmatch(line.rstrip())
        if match is None:
            continue
        rest = str(match.group(2)).split("#", 1)[0].strip()
        if rest:
            flow = _flow_events(rest)
            if flow is not None:
                return frozenset(str(event) for event in flow)
            if rest[0] in "{&*!|>":
                return None
            event: str = rest.strip("'\"")
            return frozenset({event})
        keys: list[tuple[int, str]] = []
        for child in lines[index + 1 :]:
            if _significant(child) and not child.startswith(" "):
                break
            key = _EVENT_KEY.match(child) if _significant(child) else None
            if key is not None:
                keys.append((len(key.group(1)), str(key.group(2))))
        if not keys:
            return None
        top = min(indent for indent, _ in keys)
        return frozenset(name for indent, name in keys if indent == top)
    return None


def _run_commands(lines: list[str]) -> list[str]:
    commands: list[str] = []
    index = 0
    while index < len(lines):
        match = _RUN.fullmatch(lines[index].rstrip()) if _significant(lines[index]) else None
        index += 1
        if match is None:
            continue
        indentation = len(match.group(1))
        value = match.group(2).strip()
        if _BLOCK_SCALAR.fullmatch(value):
            while index < len(lines) and (
                not lines[index].strip() or len(lines[index]) - len(lines[index].lstrip()) > indentation
            ):
                commands.extend(part for part in lines[index].strip().splitlines() if part)
                index += 1
        else:
            commands.append(re.sub(r"\s+#.*$", "", value).strip("'\""))
    return commands


def _runs_make_lint(lines: list[str]) -> bool:
    return any(_MAKE_LINT_COMMAND.search(command) for command in _run_commands(lines))


def _redundancy_findings(root: Path, repository: Repository) -> list[Finding]:
    sources, problems = read_workflow_sources(root, repository)
    findings = [
        operational(repository, "workflow-redundancy", path, message) for path, message in problems
    ]
    parsed = {path: source.splitlines() for path, source in sources}
    standard = next(
        (
            path
            for name in _LINT_WORKFLOW_NAMES
            if (path := f".github/workflows/{name}") in parsed
        ),
        None,
    )
    if standard is None or not _runs_make_lint(parsed[standard]):
        return findings
    standard_events = _trigger_events(parsed[standard])
    if standard_events is None:
        return findings
    for path, lines in parsed.items():
        if path == standard or not _runs_make_lint(lines):
            continue
        events = _trigger_events(lines)
        shared = standard_events & events if events is not None else frozenset()
        if shared:
            findings.append(
                violation(
                    repository,
                    "workflow-redundancy",
                    path,
                    f"runs make lint, which {standard} already runs on {', '.join(sorted(shared))}; "
                    "remove the make lint step from this workflow",
                )
            )
    return findings


def workflow_redundancy_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            "workflow-redundancy",
            "Reject workflows that repeat make lint already run by the lint workflow",
            TagPredicate(),
            _redundancy_findings,
            None,
            None,
            (),
        ),
    )
