from __future__ import annotations

import re
from pathlib import Path
from typing import Any

from repomod.io import read_json_object
from repomod.rules.binary_release import (
    _CHECKSUMS_FILE,
    _DEFAULT_BRANCH_GUARD,
    _DEFAULT_BRANCH_REF,
    _VERIFY_TAG,
    _job_ranges,
    _release_notes_edits,
    _workflow_without_comments,
    build_release_assets,
)
from repomod.rules.lint import _makefile_parts, _root_makefile
from repomod.rules.release_notes import RELEASE_NOTES_FLAG, RELEASE_NOTES_STEP_NAME
from repomod.rules.workflow_redundancy import _trigger_events
from repomod.rules.workflows import read_workflow_sources
from repomod.types import Finding, Repository, Rule, TagPredicate, operational, violation

_REQUIRED_TARGETS = ("build", "verify", "lint", "clean")
_RELEASE_WORKFLOW = ".github/workflows/release.yml"
_SOURCES_LOCK = "sources.lock.json"
_BUDGETS = "release-budgets.json"
_BOOTSTRAP = "bootstrap.lock.json"
_SHA256 = re.compile(r"[0-9a-f]{64}")
_SHA_TAG = re.compile(r"release-\$\{\{\s*github\.sha\s*\}\}")
_CREATE = re.compile(r"\bgh\s+release\s+create\b")
_TARGET_FLAG = re.compile(r"--target\s+[\"']?(?:\$\{?GITHUB_SHA\}?|\$\{\{\s*github\.sha\s*\}\})[\"']?(?=\s|$)")
_CHECK = re.compile(r"\bsha256sum\s+(?:[^\n|;&]*\s)?(?:-c|--check)\b")
_PRERELEASE = re.compile(r"(?<!\S)--prerelease\b")
_BUILD_TAGS = frozenset({"build"})


def _is_int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _make_targets_findings(root: Path, repository: Repository) -> list[Finding]:
    rule = "build-make-targets"
    if repository.private:
        return []
    makefile = _root_makefile(root)
    if makefile is None:
        return [
            violation(
                repository,
                rule,
                "Makefile",
                "add a root Makefile that defines " + ", ".join(_REQUIRED_TARGETS),
            )
        ]
    relative = makefile.relative_to(root).as_posix()
    if makefile.is_symlink():
        return [operational(repository, rule, relative, "root makefile is a symlink and was not inspected")]
    try:
        source = makefile.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        return [operational(repository, rule, relative, f"cannot read root Makefile: {error}")]
    defined = set(_makefile_parts(source)[0])
    return [
        violation(
            repository,
            rule,
            relative,
            f"root Makefile does not define a {target} target required of build repositories",
        )
        for target in _REQUIRED_TARGETS
        if target not in defined
    ]


def _input_problems(index: int, entry: Any, seen: set[str]) -> list[str]:
    if not isinstance(entry, dict):
        return [f"inputs[{index}] must be an object"]
    name = entry.get("name")
    label = f"inputs[{index}]"
    problems = []
    if not isinstance(name, str) or not name:
        problems.append(f"{label} needs a non-empty string name")
    else:
        label = f"input {name}"
        if name in seen:
            problems.append(f"{label} duplicates an earlier input name")
        seen.add(name)
    if not isinstance(entry.get("version"), str):
        problems.append(f"{label} needs a string version")
    sha256 = entry.get("sha256")
    if not isinstance(sha256, str) or _SHA256.fullmatch(sha256) is None:
        problems.append(f"{label} needs a sha256 of 64 lowercase hex digits")
    url = entry.get("url")
    urls = entry.get("urls")
    if (url is None) == (urls is None):
        problems.append(f"{label} needs exactly one of url and urls")
    elif url is not None and not isinstance(url, str):
        problems.append(f"{label} url must be a string")
    elif urls is not None and (
        not isinstance(urls, list) or not urls or not all(isinstance(item, str) for item in urls)
    ):
        problems.append(f"{label} urls must be a non-empty list of strings")
    if "size" in entry and (not _is_int(entry["size"]) or entry["size"] <= 0):
        problems.append(f"{label} size must be a positive integer")
    return problems


def _lock_document(
    root: Path, repository: Repository, name: str, *, required: bool
) -> tuple[dict[str, Any] | None, list[Finding]]:
    rule = "build-locks"
    path = root / name
    if path.is_symlink():
        return None, [operational(repository, rule, name, f"{name} is a symlink and was not inspected")]
    if not path.exists():
        if required:
            return None, [violation(repository, rule, name, f"build repositories require {name}")]
        return None, []
    document, error = read_json_object(path)
    if document is None:
        return None, [violation(repository, rule, name, f"{name} is not a valid JSON object: {error}")]
    findings = []
    version = document.get("schema_version")
    if not _is_int(version) or version != 1:
        findings.append(violation(repository, rule, name, f"{name} schema_version must be the integer 1"))
    return document, findings


def _build_locks_findings(root: Path, repository: Repository) -> list[Finding]:
    rule = "build-locks"
    if repository.private:
        return []
    sources, findings = _lock_document(root, repository, _SOURCES_LOCK, required=True)
    if sources is not None:
        inputs = sources.get("inputs")
        if not isinstance(inputs, list) or not inputs:
            findings.append(
                violation(repository, rule, _SOURCES_LOCK, f"{_SOURCES_LOCK} inputs must be a non-empty list")
            )
        else:
            seen: set[str] = set()
            for index, entry in enumerate(inputs):
                findings.extend(
                    violation(repository, rule, _SOURCES_LOCK, f"{_SOURCES_LOCK}: {problem}")
                    for problem in _input_problems(index, entry, seen)
                )
    budgets, budget_findings = _lock_document(root, repository, _BUDGETS, required=False)
    findings.extend(budget_findings)
    if budgets is not None and not isinstance(budgets.get("targets"), dict):
        findings.append(violation(repository, rule, _BUDGETS, f"{_BUDGETS} needs a targets object"))
    _, bootstrap_findings = _lock_document(root, repository, _BOOTSTRAP, required=False)
    findings.extend(bootstrap_findings)
    return findings


def _logical_command(lines: list[str], start: int) -> str:
    parts = []
    for line in lines[start:]:
        stripped = line.rstrip()
        parts.append(stripped.removesuffix("\\"))
        if not stripped.endswith("\\"):
            break
    return " ".join(part.strip() for part in parts)


def _create_commands(clean: str) -> list[str]:
    lines = clean.splitlines()
    commands = []
    for index, line in enumerate(lines):
        if _CREATE.search(line) is None:
            continue
        command = _logical_command(lines, index)
        match = _CREATE.search(command)
        commands.append(command if match is None else command[match.start() :])
    return commands


def _release_findings(repository: Repository, path: str, source: str) -> list[Finding]:
    rule = "build-release-standard"
    clean = _workflow_without_comments(source)

    def problem(message: str) -> Finding:
        return violation(repository, rule, path, message)

    findings = []
    if _trigger_events(clean.splitlines()) != frozenset({"workflow_dispatch"}):
        findings.append(problem("build release must trigger only on workflow_dispatch"))
    lines = clean.splitlines()
    preflight = next(
        ("\n".join(lines[start:end]) for name, start, end in _job_ranges(clean) if name == "preflight"),
        None,
    )
    if preflight is None:
        findings.append(problem("build release needs a preflight job that rejects refs other than the default branch"))
    elif not (
        "github.ref" in preflight
        and _DEFAULT_BRANCH_GUARD.search(preflight) is not None
        and _DEFAULT_BRANCH_REF.search(preflight) is not None
    ):
        findings.append(problem("preflight job must compare github.ref with refs/heads/ plus the default branch"))
    if _SHA_TAG.search(clean) is None:
        findings.append(problem("build release must publish under the release-${{ github.sha }} tag"))
    if _release_notes_edits(source) != []:
        findings.append(
            problem(
                f"build release must run the shared '{RELEASE_NOTES_STEP_NAME}' step before gh release create "
                f"and pass {RELEASE_NOTES_FLAG}"
            )
        )
    commands = _create_commands(clean)
    if not commands:
        findings.append(problem("build release must publish with gh release create"))
    required = (
        (_PRERELEASE, "--prerelease"),
        (_VERIFY_TAG, "--verify-tag"),
        (_TARGET_FLAG, '--target "$GITHUB_SHA"'),
    )
    for command in commands:
        for pattern, flag in required:
            if pattern.search(command) is None:
                findings.append(problem(f"gh release create must pass {flag}"))
        if RELEASE_NOTES_FLAG not in command:
            findings.append(problem(f"gh release create must pass {RELEASE_NOTES_FLAG}"))
    if _CHECKSUMS_FILE.search(clean) is None:
        findings.append(problem("build release must publish a SHA256SUMS file"))
    if _CHECK.search(clean) is None:
        findings.append(problem("build release must check SHA256SUMS with sha256sum -c before publishing"))
    archives, manifests = build_release_assets(clean)
    if not archives:
        findings.append(problem("build release must publish .tar.xz archives"))
    if not manifests:
        findings.append(problem("build release must publish .manifest.json manifests"))
    return findings


def _release_standard_findings(root: Path, repository: Repository) -> list[Finding]:
    rule = "build-release-standard"
    if repository.private:
        return []
    sources, problems = read_workflow_sources(root, repository)
    findings = [operational(repository, rule, path, message) for path, message in problems]
    release = next((source for path, source in sources if path == _RELEASE_WORKFLOW), None)
    if release is None:
        if not any(path == _RELEASE_WORKFLOW for path, _ in problems):
            findings.append(
                violation(repository, rule, _RELEASE_WORKFLOW, f"build repositories require {_RELEASE_WORKFLOW}")
            )
        return findings
    findings.extend(_release_findings(repository, _RELEASE_WORKFLOW, release))
    return findings


def build_repo_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            "build-make-targets",
            "Require build, verify, lint, and clean targets in build repositories",
            TagPredicate(all_of=_BUILD_TAGS),
            _make_targets_findings,
            None,
            None,
            (),
        ),
        Rule(
            "build-locks",
            "Check the pinned-input lock files of build repositories",
            TagPredicate(all_of=_BUILD_TAGS),
            _build_locks_findings,
            None,
            None,
            (),
        ),
        Rule(
            "build-release-standard",
            "Check the standard release workflow of build repositories",
            TagPredicate(all_of=_BUILD_TAGS),
            _release_standard_findings,
            None,
            None,
            (),
        ),
    )
