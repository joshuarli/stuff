from __future__ import annotations

import json
import re
import shlex
from pathlib import Path

from repomod.git import root_ignore_matches, tracked_paths
from repomod.rules.languages import _language_sources
from repomod.rules.workflows import read_workflow_sources
from repomod.types import Finding, Repository, Rule, TagPredicate, operational, violation

_RUN = re.compile(r"^(?P<indent> *)(?:-\s*)?run\s*:\s*(?P<value>.*)$")
_GITHUB_EXPRESSION = re.compile(r"\$\{\{.*?\}\}")
_MAKE_TARGET = re.compile(r"^[A-Za-z0-9_./%-]+\Z")
_MAKE_OPTIONS_WITH_VALUE = {"-C", "-f", "-I", "-j", "-l", "-o", "-W", "--directory", "--file"}
_TRACKED_ARTIFACTS = {
    ".DS_Store": (":(glob)**/.DS_Store", None),
    "Thumbs.db": (":(glob)**/Thumbs.db", None),
    "__pycache__": (":(glob)**/__pycache__/**", "python"),
    ".pytest_cache": (":(glob)**/.pytest_cache/**", "python"),
    ".ruff_cache": (":(glob)**/.ruff_cache/**", "python"),
    ".mypy_cache": (":(glob)**/.mypy_cache/**", "python"),
    ".tox": (":(glob)**/.tox/**", "python"),
    ".venv": (":(glob)**/.venv/**", "python"),
    "node_modules": (":(glob)**/node_modules/**", "javascript"),
    "target": (":(glob)**/target/**", "rust"),
    ".build": (":(glob)**/.build/**", "swift"),
    ".zig-cache": (":(glob)**/.zig-cache/**", "zig"),
    "zig-out": (":(glob)**/zig-out/**", "zig"),
    ".pyc": (":(glob)**/*.pyc", "python"),
    ".pyo": (":(glob)**/*.pyo", "python"),
}
_FIXTURE_PARTS = {"example", "examples", "fixture", "fixtures", "test", "tests", "third_party", "vendor"}
_COMPOSED_IGNORE_PROBES = {
    "common": (
        (".DS_Store", ".DS_Store"),
        ("Thumbs.db", "Thumbs.db"),
    ),
    "python": (
        (".venv/repomod-ignore-probe", ".venv/"),
        ("__pycache__/repomod-ignore-probe.pyc", "__pycache__/"),
        (".pytest_cache/repomod-ignore-probe", ".pytest_cache/"),
        (".ruff_cache/repomod-ignore-probe", ".ruff_cache/"),
        (".mypy_cache/repomod-ignore-probe", ".mypy_cache/"),
        (".tox/repomod-ignore-probe", ".tox/"),
    ),
    "rust": (("target/repomod-ignore-probe", "target/"),),
    "javascript": (("node_modules/repomod-ignore-probe", "node_modules/"),),
    "typescript": (("node_modules/repomod-ignore-probe", "node_modules/"),),
    "swift": ((".build/repomod-ignore-probe", ".build/"),),
    "zig": (
        (".zig-cache/repomod-ignore-probe", ".zig-cache/"),
        ("zig-out/repomod-ignore-probe", "zig-out/"),
    ),
}


def _run_commands(source: str) -> list[str]:
    lines = source.splitlines()
    commands = []
    index = 0
    while index < len(lines):
        match = _RUN.match(lines[index])
        index += 1
        if match is None:
            continue
        value = match.group("value").strip()
        assert isinstance(value, str)
        if len(value) >= 2 and value[0] == value[-1] == '"':
            try:
                decoded = json.loads(value)
            except json.JSONDecodeError:
                decoded = value
            if isinstance(decoded, str):
                value = decoded
        elif len(value) >= 2 and value[0] == value[-1] == "'":
            value = value[1:-1].replace("''", "'")
        if value.startswith(("|", ">")):
            indent = len(match.group("indent"))
            body = []
            while index < len(lines):
                line = lines[index]
                index += 1
                if line.strip() and len(line) - len(line.lstrip(" ")) <= indent:
                    index -= 1
                    break
                body.append(line.strip())
            commands.extend(body)
        elif value:
            commands.append(value)
    return commands


def _make_targets(command: str) -> list[str]:
    try:
        tokens = shlex.split(_GITHUB_EXPRESSION.sub(" ", command), comments=True, posix=True)
    except ValueError:
        return []
    operators = {";", "&&", "||", "|"}
    targets = []
    index = 0
    while index < len(tokens):
        if tokens[index] != "make" or (index and tokens[index - 1] not in operators):
            index += 1
            continue
        index += 1
        invocation = []
        while index < len(tokens) and tokens[index] not in operators:
            invocation.append(tokens[index])
            index += 1
        if any(option in _MAKE_OPTIONS_WITH_VALUE for option in invocation):
            continue
        for token in invocation:
            if token.startswith("-") or "=" in token or not _MAKE_TARGET.fullmatch(token):
                continue
            targets.append(token)
    return targets


def _declared_make_targets(makefile: Path) -> set[str] | None:
    if makefile.is_symlink():
        return None
    source = makefile.read_text(encoding="utf-8")
    if re.search(r"(?m)^\s*-?include\s|\$\(eval\b|^\s*(?:ifeq|ifneq|ifdef|ifndef)\b", source):
        return None
    targets = set()
    for line in source.splitlines():
        if not line or line[0].isspace() or line.lstrip().startswith("#"):
            continue
        left, separator, right = line.partition(":")
        if not separator or right.startswith("=") or "=" in left:
            continue
        if left == ".PHONY":
            targets.update(target for target in right.split() if _MAKE_TARGET.fullmatch(target))
            continue
        for target in left.split():
            if "%" not in target and "$" not in target:
                targets.add(target)
    return targets


def _make_target_findings(root: Path, repository: Repository) -> list[Finding]:
    sources, problems = read_workflow_sources(root, repository)
    findings = []
    requested: list[tuple[str, str]] = []
    findings.extend(
        operational(repository, "workflow-make-targets", path, message)
        for path, message in problems
    )
    for relative, source in sources:
        if re.search(r"(?m)^\s*working-directory\s*:", source):
            continue
        requested.extend((relative, target) for command in _run_commands(source) for target in _make_targets(command))
    if not requested:
        return findings
    makefile_candidates = tuple(root / name for name in ("GNUmakefile", "makefile", "Makefile"))
    makefile = next(
        (candidate for candidate in makefile_candidates if candidate.exists() or candidate.is_symlink()),
        None,
    )
    if makefile is None:
        findings.extend(
            violation(
                repository,
                "workflow-make-targets",
                workflow,
                f"workflow runs make target {target!r}, but the repository has no root Makefile",
            )
            for workflow, target in requested
        )
        return findings
    if makefile.is_symlink():
        findings.append(
            operational(
                repository,
                "workflow-make-targets",
                makefile.name,
                "root makefile is a symlink and was not inspected",
            )
        )
        return findings
    targets = _declared_make_targets(makefile)
    if targets is None:
        return findings
    findings.extend(
        violation(
            repository,
            "workflow-make-targets",
            workflow,
            f"workflow runs make target {target!r}, but the root Makefile does not define it",
        )
        for workflow, target in requested
        if target not in targets
    )
    return findings


def _artifact_pathspecs(languages: set[str]) -> tuple[str, ...]:
    return tuple(
        pathspec
        for pathspec, language in _TRACKED_ARTIFACTS.values()
        if language is None or language in languages
    )


def _artifact_name(path: str, languages: set[str]) -> str | None:
    parts = Path(path).parts
    if any(part.casefold() in _FIXTURE_PARTS for part in parts):
        return None
    for name, (_, language) in _TRACKED_ARTIFACTS.items():
        if language is not None and language not in languages:
            continue
        if name in {".pyc", ".pyo"}:
            if path.endswith(name):
                return name
        elif name in parts:
            return name
    return None


def _tracked_artifact_findings(root: Path, repository: Repository) -> list[Finding]:
    languages = _language_sources(root)
    tracked = tracked_paths(root, _artifact_pathspecs(languages))
    groups: dict[str, list[str]] = {}
    for path in tracked:
        name = _artifact_name(path, languages)
        if name is not None:
            groups.setdefault(name, []).append(path)
    findings = [
        violation(
            repository,
            "generated-artifacts",
            paths[0],
            f"{len(paths)} tracked generated artifact path(s) match {name}; remove them from Git",
        )
        for name, paths in sorted(groups.items())
    ]
    required = list(_COMPOSED_IGNORE_PROBES["common"])
    required.extend(
        probe
        for language in sorted(languages)
        for probe in _COMPOSED_IGNORE_PROBES.get(language, ())
    )
    ignored = root_ignore_matches(root, tuple(probe for probe, _ in required))
    missing = [
        pattern
        for probe, pattern in required
        if probe not in ignored
    ]
    if missing:
        findings.append(
            violation(
                repository,
                "generated-artifacts",
                ".gitignore",
                "composed ignore policy is missing in-repository patterns for " + ", ".join(sorted(set(missing))),
                severity="warning",
            )
        )
    return findings


def consistency_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            "generated-artifacts",
            "Check tracked generated files and composed ignore coverage",
            TagPredicate(),
            _tracked_artifact_findings,
            None,
            None,
            (),
        ),
        Rule(
            "workflow-make-targets",
            "Check literal workflow make targets against the root Makefile",
            TagPredicate(),
            _make_target_findings,
            None,
            None,
            (),
        ),
    )
