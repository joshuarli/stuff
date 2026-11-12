from __future__ import annotations

import re
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

from repomod.git import GitError
from repomod.rules.actions import _validate_workflows
from repomod.rules.workflows import read_workflow_sources, workflow_files
from repomod.types import Finding, Repository, Rule, TagPredicate, operational, violation

_MACOS_RUNNER = re.compile(
    r"(?<![\w.-])macos-(?P<version>latest|\d+(?:\.\d+)?)"
    r"(?P<suffix>(?:[-.][0-9A-Za-z._]+)*)(?![\w.-])"
)
_UBUNTU_RUNNER = re.compile(
    r"(?<![\w.-])ubuntu-(?P<version>latest|\d+(?:\.\d+)?)"
    r"(?P<arm>-arm)?(?![\w.-])"
)
_UBUNTU_ARM_RUNNER = "ubuntu-26.04-arm"
_UBUNTU_X86_RUNNER = "ubuntu-26.04"
_RUNNER_DECLARATION = re.compile(
    r"(?:^|[\s{,])(?:runs-on|os|macos|ubuntu|runner)\s*:"
)
_LIST_ITEM = re.compile(r"^\s*-\s+(?P<value>.*?)\s*$")
_AMD64 = "linux/amd64"


def _code_portion(line: str) -> str:
    return line.partition("#")[0]


def _runner_line(pattern: re.Pattern[str], code: str) -> bool:
    if _RUNNER_DECLARATION.search(code):
        return True
    item = _LIST_ITEM.match(code)
    if item is None:
        return False
    return pattern.fullmatch(item.group("value").strip("'\"")) is not None


def _macos_accepted(match: re.Match[str]) -> bool:
    suffix = str(match.group("suffix"))
    if suffix.endswith("-intel"):
        return True
    return str(match.group("version")) == "latest"


def _macos_repaired(match: re.Match[str], _amd64: bool) -> str:
    if _macos_accepted(match):
        return str(match.group(0))
    return f"macos-latest{match.group('suffix')}"


def _ubuntu_accepted(match: re.Match[str]) -> bool:
    return str(match.group("version")) == "26.04"


def _ubuntu_repaired(match: re.Match[str], amd64: bool) -> str:
    if _ubuntu_accepted(match):
        return str(match.group(0))
    if match.group("arm") is not None or (str(match.group("version")) == "latest" and not amd64):
        return _UBUNTU_ARM_RUNNER
    return _UBUNTU_X86_RUNNER


def _indent(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


def _block(lines: list[str], index: int) -> list[str]:
    """The list item a runner line opens, or else the job whose body holds it."""
    line = lines[index]
    if line.lstrip().startswith("- "):
        start, floor = index, _indent(line)
    else:
        start = next(
            (row for row in range(index - 1, -1, -1) if lines[row].strip() and _indent(lines[row]) < _indent(line)),
            index,
        )
        floor = _indent(lines[start])
    end = start + 1
    while end < len(lines) and (not lines[end].strip() or _indent(lines[end]) > floor):
        end += 1
    return lines[start:end]


def _runs_amd64(lines: list[str], index: int) -> bool:
    return any(_AMD64 in _code_portion(line) for line in _block(lines, index))


@dataclass(frozen=True, slots=True)
class RunnerPolicy:
    rule_id: str
    description: str
    commit_message: str
    family: str
    pattern: re.Pattern[str]
    guidance: str
    accepted: Callable[[re.Match[str]], bool]
    repaired: Callable[[re.Match[str], bool], str]

    def findings(self, root: Path, repository: Repository) -> list[Finding]:
        sources, problems = read_workflow_sources(root, repository)
        findings = [
            operational(repository, self.rule_id, path, message)
            for path, message in problems
        ]
        for relative, source in sources:
            for line_number, line in enumerate(source.splitlines(), start=1):
                code = _code_portion(line)
                if not _runner_line(self.pattern, code):
                    continue
                for match in self.pattern.finditer(code):
                    if self.accepted(match):
                        continue
                    findings.append(
                        violation(
                            repository,
                            self.rule_id,
                            relative,
                            f"{self.family} runner {match.group(0)!r} is not supported; "
                            f"{self.guidance}",
                            fixable=True,
                            line=line_number,
                            column=match.start() + 1,
                        )
                    )
        return findings

    def fix(self, root: Path, repository: Repository) -> dict[str, str]:
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
            updated = self._repair_source(source)
            if updated != source:
                plan[relative] = updated
        return plan

    def _repair_source(self, source: str) -> str:
        """Rewrite runner labels, keeping x86 for a job or matrix entry that runs linux/amd64 images."""
        rows = source.splitlines(keepends=True)
        bodies = [row.rstrip("\r\n") for row in rows]
        lines = []
        for index, line in enumerate(rows):
            body = bodies[index]
            ending = line[len(body) :]
            code, marker, comment = body.partition("#")
            if _runner_line(self.pattern, code):
                amd64 = _runs_amd64(bodies, index)
                code = self.pattern.sub(lambda match, amd64=amd64: self.repaired(match, amd64), code)
            lines.append(code + marker + comment + ending)
        return "".join(lines)


_MACOS_POLICY = RunnerPolicy(
    "gha-macos",
    "Require macos-latest GitHub Actions runners",
    "chore(repomod): update macos runners",
    "macOS",
    _MACOS_RUNNER,
    "use the macos-latest alias",
    _macos_accepted,
    _macos_repaired,
)
_UBUNTU_POLICY = RunnerPolicy(
    "gha-ubuntu",
    "Require Ubuntu 26.04 runners and prefer the ARM image",
    "chore(repomod): update ubuntu runners",
    "Ubuntu",
    _UBUNTU_RUNNER,
    f"use {_UBUNTU_ARM_RUNNER}, or {_UBUNTU_X86_RUNNER} for an x86-only job",
    _ubuntu_accepted,
    _ubuntu_repaired,
)


def runner_rules() -> tuple[Rule, ...]:
    return tuple(
        Rule(
            policy.rule_id,
            policy.description,
            TagPredicate(),
            policy.findings,
            policy.fix,
            policy.commit_message,
            (".github/workflows/*.yml", ".github/workflows/*.yaml"),
            _validate_workflows,
        )
        for policy in (_MACOS_POLICY, _UBUNTU_POLICY)
    )
