from __future__ import annotations

import re
from pathlib import Path

from repomod.git import GitError
from repomod.rules.actions import _validate_workflows
from repomod.rules.workflows import read_workflow_sources, workflow_files
from repomod.types import Finding, Repository, Rule, TagPredicate, operational, violation

_MACOS_RUNNER = re.compile(
    r"(?<![\w.-])macos-(?P<version>latest|\d+(?:\.\d+)?)"
    r"(?P<suffix>(?:[-.][0-9A-Za-z._]+)*)(?![\w.-])"
)
_ACCEPTED_MAJOR_VERSIONS = frozenset({26, 27})
_REPAIRED_MAJOR_VERSION = "26"
_RUNNER_DECLARATION = re.compile(r"(?:^|[\s{,])(?:runs-on|os|macos|runner)\s*:")


def _major_version(match: re.Match[str]) -> int | None:
    value = match.group("version")
    if value == "latest":
        return None
    return int(value.split(".", 1)[0])


def _accepted(match: re.Match[str]) -> bool:
    return _major_version(match) in _ACCEPTED_MAJOR_VERSIONS


def _code_portion(line: str) -> str:
    return line.partition("#")[0]


def _runner_line(code: str) -> bool:
    return bool(_RUNNER_DECLARATION.search(code)) or code.lstrip().startswith("- ")


def _macos_runner_findings(root: Path, repository: Repository) -> list[Finding]:
    sources, problems = read_workflow_sources(root, repository)
    findings = [
        operational(repository, "gha-macos", path, message)
        for path, message in problems
    ]
    for relative, source in sources:
        for line_number, line in enumerate(source.splitlines(), start=1):
            code = _code_portion(line)
            if not _runner_line(code):
                continue
            for match in _MACOS_RUNNER.finditer(code):
                if _accepted(match):
                    continue
                findings.append(
                    violation(
                        repository,
                        "gha-macos",
                        relative,
                        f"macOS runner {match.group(0)!r} is not supported; "
                        "use macos-26 or macos-27",
                        fixable=True,
                        line=line_number,
                        column=match.start() + 1,
                    )
                )
    return findings


def _replace_runner(match: re.Match[str]) -> str:
    if _accepted(match):
        return match.group(0)
    return f"macos-{_REPAIRED_MAJOR_VERSION}{match.group('suffix')}"


def _repair_source(source: str) -> str:
    lines = []
    for line in source.splitlines(keepends=True):
        body = line.rstrip("\r\n")
        ending = line[len(body) :]
        code, marker, comment = body.partition("#")
        repaired = (
            _MACOS_RUNNER.sub(_replace_runner, code) if _runner_line(code) else code
        )
        lines.append(repaired + marker + comment + ending)
    return "".join(lines)


def _fix_macos_runners(root: Path, repository: Repository) -> dict[str, str]:
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
        updated = _repair_source(source)
        if updated != source:
            plan[relative] = updated
    return plan


def macos_runner_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            "gha-macos",
            "Require macOS 26 or 27 GitHub Actions runners",
            TagPredicate(),
            _macos_runner_findings,
            _fix_macos_runners,
            "chore(repomod): update macos runners",
            (".github/workflows/*.yml", ".github/workflows/*.yaml"),
            _validate_workflows,
        ),
    )
