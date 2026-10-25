from __future__ import annotations

import json
import os
import re
import shlex
import tomllib
from pathlib import Path, PurePosixPath
from typing import Any

from repomod.git import GitError, working_paths
from repomod.types import Finding, Repository, Rule, TagPredicate, operational, violation
from repomod.workflow_policy import is_fork_workflow

_PATHS = (
    ":(glob)**/.oxfmtrc.json",
    ":(glob)**/.oxfmtrc.jsonc",
    ":(glob)**/oxfmt.config.ts",
    ":(glob)**/oxfmt.config.mts",
    ":(glob)**/oxfmt.config.cts",
    ":(glob)**/oxfmt.config.js",
    ":(glob)**/oxfmt.config.mjs",
    ":(glob)**/oxfmt.config.cjs",
    ":(glob)**/oxfmt.config.json",
    ":(glob)**/oxfmt.config.jsonc",
    ":(glob)**/.prettierrc",
    ":(glob)**/.prettierrc.json",
    ":(glob)**/.prettierrc.json5",
    ":(glob)**/.prettierrc.yaml",
    ":(glob)**/.prettierrc.yml",
    ":(glob)**/.prettierrc.js",
    ":(glob)**/.prettierrc.cjs",
    ":(glob)**/.prettierrc.mjs",
    ":(glob)**/prettier.config.js",
    ":(glob)**/prettier.config.cjs",
    ":(glob)**/prettier.config.mjs",
    ":(glob)**/prettier.config.ts",
    ":(glob)**/prettier.config.mts",
    ":(glob)**/prettier.config.cts",
    ":(glob)**/.prettierignore",
    ":(glob)**/rustfmt.toml",
    ":(glob)**/.rustfmt.toml",
    ":(glob)**/.style.yapf",
    ":(glob)**/ruff.toml",
    ":(glob)**/.ruff.toml",
    ":(glob)**/black.toml",
    ":(glob)**/.black",
    ":(glob)**/dprint.json",
    ":(glob)**/dprint.jsonc",
    ":(glob)**/.dprint.json",
    ":(glob)**/.clang-format",
    ":(glob)**/_clang-format",
    ":(glob)**/.swift-format",
    ":(glob)**/package.json",
    ":(glob)**/pyproject.toml",
    ":(glob)**/requirements.txt",
    ":(glob)**/requirements-*.txt",
    ":(glob)**/setup.cfg",
    ":(glob)**/Makefile",
    ":(glob)**/GNUmakefile",
    ":(glob)**/makefile",
    ":(glob)**/justfile",
    ":(glob)**/Justfile",
    ":(glob)**/Taskfile.yml",
    ":(glob)**/Taskfile.yaml",
    ":(glob)**/.pre-commit-config.yaml",
    ":(glob)**/.pre-commit-config.yml",
    ":(glob)**/.gitlab-ci.yml",
    ":(glob)**/.circleci/config.yml",
    ":(glob)**/.circleci/config.yaml",
    ":(glob)**/.github/workflows/*.yml",
    ":(glob)**/.github/workflows/*.yaml",
    ":(glob)**/Dockerfile",
    ":(glob)**/*.sh",
    ":(glob)**/*.bash",
    ":(glob)**/*.zsh",
)
_FORMATTER_CONFIGS = {
    ".oxfmtrc.json",
    ".oxfmtrc.jsonc",
    "oxfmt.config.ts",
    "oxfmt.config.mts",
    "oxfmt.config.cts",
    "oxfmt.config.js",
    "oxfmt.config.mjs",
    "oxfmt.config.cjs",
    "oxfmt.config.json",
    "oxfmt.config.jsonc",
    ".prettierrc",
    ".prettierrc.json",
    ".prettierrc.json5",
    ".prettierrc.yaml",
    ".prettierrc.yml",
    ".prettierrc.js",
    ".prettierrc.cjs",
    ".prettierrc.mjs",
    "prettier.config.js",
    "prettier.config.cjs",
    "prettier.config.mjs",
    "prettier.config.ts",
    "prettier.config.mts",
    "prettier.config.cts",
    ".prettierignore",
    "rustfmt.toml",
    ".rustfmt.toml",
    "black.toml",
    ".black",
    "dprint.json",
    "dprint.jsonc",
    ".dprint.json",
    ".clang-format",
    "_clang-format",
    ".swift-format",
    ".style.yapf",
}
_DEPENDENCY = re.compile(r"^([A-Za-z0-9_.-]+)")
_BANNED_PACKAGES = {"black", "autopep8", "yapf", "prettier", "oxfmt", "dprint"}
_FORMATTER_CONFIG_NAMES = {name.casefold() for name in _FORMATTER_CONFIGS}
_SINGLE_COMMANDS = {
    "rustfmt": "rustfmt",
    "gofmt": "gofmt",
    "gofumpt": "gofumpt",
    "oxfmt": "oxfmt",
    "prettier": "prettier",
    "dprint": "dprint",
    "black": "black",
    "autopep8": "autopep8",
    "yapf": "yapf",
    "clang-format": "clang-format",
    "swift-format": "swift-format",
    "shfmt": "shfmt",
    "google-java-format": "google-java-format",
    "scalafmt": "scalafmt",
    "nixfmt": "nixfmt",
    "alejandra": "alejandra",
}
_PAIR_COMMANDS = {
    ("go", "fmt"): "go fmt",
    ("ruff", "format"): "ruff format",
    ("biome", "format"): "biome format",
    ("zig", "fmt"): "zig fmt",
    ("dart", "format"): "dart format",
    ("mix", "format"): "mix format",
    ("dotnet", "format"): "dotnet format",
    ("terraform", "fmt"): "terraform fmt",
}
_NON_EXECUTABLES = {
    "echo",
    "printf",
    "cat",
    "grep",
    "rg",
    "sed",
    "awk",
    "head",
    "tail",
    "test",
    "true",
    "false",
    "exit",
    "return",
    "cd",
    "pwd",
    "export",
}
_COMMAND_FILES = {
    "makefile",
    "gnumakefile",
    "justfile",
    "taskfile.yml",
    "taskfile.yaml",
    ".pre-commit-config.yaml",
    ".pre-commit-config.yml",
    ".gitlab-ci.yml",
    "requirements.txt",
    "setup.cfg",
    "dockerfile",
}
_WORKFLOW_RUN = re.compile(r"^(?P<indent> *)(?:-\s*)?run\s*:\s*(?P<value>.*)$")


def _read_file(root: Path, relative: str) -> str:
    path = PurePosixPath(relative)
    if path.is_absolute() or ".." in path.parts:
        raise OSError("path escapes the repository")
    current = root
    for part in path.parts:
        current = current / part
        if current.is_symlink():
            raise OSError("symbolic links are not followed")
    descriptor = os.open(current, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(descriptor, encoding="utf-8") as source:
        return source.read()


def _command_lines(relative: str, source: str) -> tuple[str, ...]:
    name = PurePosixPath(relative).name.casefold()
    suffix = PurePosixPath(relative).suffix.casefold()
    if "/.github/workflows/" in f"/{relative}/":
        return _workflow_run_lines(source)
    if name in {"makefile", "gnumakefile"}:
        return tuple(line.strip() for line in source.splitlines() if line.startswith("\t"))
    if suffix in {".sh", ".bash", ".zsh"} or name == "dockerfile":
        return tuple(source.splitlines())
    if name in _COMMAND_FILES:
        return tuple(source.splitlines())
    return ()


def _workflow_run_lines(source: str) -> tuple[str, ...]:
    lines = source.splitlines()
    commands = []
    index = 0
    while index < len(lines):
        match = _WORKFLOW_RUN.fullmatch(lines[index])
        index += 1
        if match is None:
            continue
        value = match.group("value").strip()
        assert isinstance(value, str)
        if value.startswith(("|", ">")):
            indentation = len(match.group("indent"))
            while index < len(lines):
                line = lines[index]
                current_indentation = len(line) - len(line.lstrip(" "))
                if line.strip() and current_indentation <= indentation:
                    break
                commands.append(line.strip())
                index += 1
        elif value:
            commands.append(value)
    return tuple(commands)


def _commands(source: str) -> tuple[str, ...]:
    found: set[str] = set()
    for line in source.splitlines():
        try:
            lexer = shlex.shlex(line, posix=True, punctuation_chars=";&|")
            lexer.whitespace_split = True
            lexer.commenters = "#"
            tokens = list(lexer)
        except ValueError:
            tokens = line.split()
        segment: list[str] = []
        for token in (*tokens, ";"):
            if token in {";", "&&", "||", "|"}:
                _command_segment(segment, found)
                segment = []
            else:
                segment.append(token)
    return tuple(sorted(found))


def _command_segment(tokens: list[str], found: set[str]) -> None:
    while tokens and (tokens[0] in {"-", "run:", "entry:", "script:", "command:"}):
        tokens = tokens[1:]
    if not tokens or Path(tokens[0].removeprefix("@")).name.casefold() in _NON_EXECUTABLES:
        return
    for index, token in enumerate(tokens):
        name = Path(token.removeprefix("@")).name.casefold()
        if name in _SINGLE_COMMANDS:
            found.add(_SINGLE_COMMANDS[name])
        if index + 1 < len(tokens):
            pair = (name, tokens[index + 1].casefold())
            command = _PAIR_COMMANDS.get(pair)
            if command is not None:
                found.add(command)
        if name == "cargo":
            next_index = index + 1
            if next_index < len(tokens) and tokens[next_index].startswith("+"):
                next_index += 1
            if next_index < len(tokens) and tokens[next_index].casefold() == "fmt":
                found.add("cargo fmt")
        if name == "ktlint" and "-f" in (value.casefold() for value in tokens[index + 1 :]):
            found.add("ktlint -F")


def _package_dependencies(document: dict[str, Any]) -> set[str]:
    names: set[str] = set()
    for field in ("dependencies", "devDependencies", "optionalDependencies", "peerDependencies"):
        entries = document.get(field)
        if isinstance(entries, dict):
            for name in entries:
                if isinstance(name, str) and name.casefold() in _BANNED_PACKAGES:
                    names.add(name.casefold())
    return names


def _python_dependencies(document: dict[str, Any]) -> set[str]:
    candidates: list[Any] = []
    project = document.get("project")
    if isinstance(project, dict):
        dependencies = project.get("dependencies")
        if isinstance(dependencies, list):
            candidates.extend(dependencies)
        optional = project.get("optional-dependencies")
        groups = document.get("dependency-groups")
        for mapping in (optional, groups):
            if isinstance(mapping, dict):
                candidates.extend(
                    value
                    for values in mapping.values()
                    if isinstance(values, list)
                    for value in values
                )
    poetry = document.get("tool", {}).get("poetry") if isinstance(document.get("tool"), dict) else None
    if isinstance(poetry, dict):
        for key in ("dependencies", "dev-dependencies"):
            mapping = poetry.get(key)
            if isinstance(mapping, dict):
                candidates.extend(mapping)
        groups = poetry.get("group")
        if isinstance(groups, dict):
            for group in groups.values():
                dependencies = group.get("dependencies") if isinstance(group, dict) else None
                if isinstance(dependencies, dict):
                    candidates.extend(dependencies)
    names: set[str] = set()
    for candidate in candidates:
        if not isinstance(candidate, str):
            continue
        match = _DEPENDENCY.match(candidate)
        if match is not None:
            name = match.group(1)
            assert isinstance(name, str)
            if name.casefold() in _BANNED_PACKAGES:
                names.add(name.casefold())
    return names


def _document_findings(repository: Repository, relative: str, source: str) -> list[Finding]:
    path = PurePosixPath(relative)
    findings = []
    if path.name.casefold() == "package.json":
        try:
            document = json.loads(source)
        except json.JSONDecodeError as error:
            findings.append(
                operational(
                    repository,
                    "no-formatters",
                    relative,
                    f"cannot inspect formatter dependencies in invalid package JSON: {error}",
                )
            )
            document = None
        if isinstance(document, dict):
            packages = _package_dependencies(document)
            scripts = document.get("scripts")
            script_commands = (
                "\n".join(value for value in scripts.values() if isinstance(value, str))
                if isinstance(scripts, dict)
                else ""
            )
            commands = _commands(script_commands)
            if packages or commands:
                offenders = sorted(packages | set(commands))
                findings.append(
                    violation(
                        repository,
                        "no-formatters",
                        relative,
                        "formatter dependencies or scripts are banned: " + ", ".join(offenders),
                    )
                )
    if path.name.casefold() in {"ruff.toml", ".ruff.toml"}:
        try:
            document = tomllib.loads(source)
        except tomllib.TOMLDecodeError as error:
            findings.append(
                operational(
                    repository,
                    "no-formatters",
                    relative,
                    f"cannot inspect Ruff formatter configuration in invalid TOML: {error}",
                )
            )
            document = None
        if isinstance(document, dict) and "format" in document:
            findings.append(
                violation(
                    repository,
                    "no-formatters",
                    relative,
                    "Ruff formatter configuration is banned: format",
                )
            )
    if path.name.casefold() == "pyproject.toml":
        try:
            document = tomllib.loads(source)
        except tomllib.TOMLDecodeError as error:
            findings.append(
                operational(
                    repository,
                    "no-formatters",
                    relative,
                    f"cannot inspect formatter configuration in invalid TOML: {error}",
                )
            )
            document = None
        if isinstance(document, dict):
            tool = document.get("tool")
            black = isinstance(tool, dict) and "black" in tool
            ruff = tool.get("ruff") if isinstance(tool, dict) else None
            ruff_format = isinstance(ruff, dict) and "format" in ruff
            packages = _python_dependencies(document)
            if black or ruff_format or packages:
                offenders = set(packages)
                if black:
                    offenders.add("tool.black")
                if ruff_format:
                    offenders.add("tool.ruff.format")
                findings.append(
                    violation(
                        repository,
                        "no-formatters",
                        relative,
                        "formatter configuration or dependencies are banned: " + ", ".join(sorted(offenders)),
                    )
                )
    if path.name.casefold() == "setup.cfg" and re.search(r"(?im)^\s*\[(?:black|yapf)\]\s*$", source):
        findings.append(
            violation(
                repository,
                "no-formatters",
                relative,
                "black and yapf configuration sections are banned",
            )
        )
    if path.name.casefold() == "requirements.txt" or path.name.casefold().startswith("requirements-"):
        packages = set()
        for line in source.splitlines():
            requirement = line.split("#", 1)[0].strip()
            match = _DEPENDENCY.match(requirement)
            if match is not None and match.group(1).casefold() in _BANNED_PACKAGES:
                packages.add(match.group(1).casefold())
        if packages:
            findings.append(
                violation(
                    repository,
                    "no-formatters",
                    relative,
                    "formatter dependencies are banned: " + ", ".join(sorted(packages)),
                )
            )
    return findings


def _formatting_findings(root: Path, repository: Repository) -> list[Finding]:
    try:
        paths = working_paths(root, _PATHS)
    except GitError as error:
        return [
            operational(
                repository, "no-formatters", None, f"cannot list formatter policy files: {error}"
            )
        ]
    findings = []
    for relative in paths:
        path = PurePosixPath(relative)
        if (
            "fork" in repository.tags
            and path.parts[:2] == (".github", "workflows")
            and not is_fork_workflow(path)
        ):
            continue
        if path.name.casefold() in _FORMATTER_CONFIG_NAMES:
            findings.append(
                violation(
                    repository,
                    "no-formatters",
                    relative,
                    "formatter-specific configuration files are banned",
                )
            )
            continue
        try:
            source = _read_file(root, relative)
        except (OSError, UnicodeError) as error:
            findings.append(
                operational(repository, "no-formatters", relative, f"cannot read policy file: {error}")
            )
            continue
        findings.extend(_document_findings(repository, relative, source))
        commands = _commands("\n".join(_command_lines(relative, source)))
        if commands:
            findings.append(
                violation(
                    repository,
                    "no-formatters",
                    relative,
                    "formatter commands are banned: " + ", ".join(commands),
                )
            )
    return findings


def formatter_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            "no-formatters",
            "Ban language formatters, formatter commands, and formatter configuration",
            TagPredicate(),
            _formatting_findings,
            None,
            None,
            (),
        ),
    )
