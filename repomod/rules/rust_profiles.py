from __future__ import annotations

import re
import tomllib
from pathlib import Path
from typing import Any

from repomod.rules.binary_release import _cargo_has_executable, has_binary_release_contract
from repomod.rules.workflows import read_workflow_sources
from repomod.types import Finding, Repository, Rule, TagPredicate
from repomod.types import operational as _operational
from repomod.types import violation as _violation

_RULE_ID = "rust-release-profiles"

_MANIFEST = "Cargo.toml"
_MAKEFILE = "Makefile"

_RELEASE_TABLE = "profile.release"
_DIST_TABLE = "profile.dist"


def _is_int(value: object, expected: int) -> bool:
    return value == expected or (isinstance(value, str) and value.strip() == str(expected))


def _release_status(table: Any) -> str:
    if table is None:
        return "absent"
    if not isinstance(table, dict):
        return "deviates"
    if not _is_int(table.get("opt-level"), 3):
        return "deviates"
    if table.get("lto") != "thin":
        return "deviates"
    if table.get("panic") != "abort":
        return "deviates"
    strip = table.get("strip")
    if strip is not None and strip is not False and strip != "none":
        return "deviates"
    units = table.get("codegen-units")
    if units is not None and _is_int(units, 1):
        return "deviates"
    return "conformant"


def _dist_status(table: Any) -> str:
    if table is None:
        return "absent"
    if not isinstance(table, dict):
        return "deviates"
    if table.get("inherits") != "release":
        return "deviates"
    if table.get("lto") != "fat":
        return "deviates"
    if not _is_int(table.get("codegen-units"), 1):
        return "deviates"
    if table.get("strip") is not True:
        return "deviates"
    return "conformant"


def _read_manifest(root: Path) -> tuple[dict[str, Any] | None, str | None, str | None]:
    manifest = root / _MANIFEST
    if manifest.is_symlink() or not manifest.is_file():
        return None, None, None
    try:
        source = manifest.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        return None, None, f"cannot read {_MANIFEST}: {error}"
    try:
        return tomllib.loads(source), source, None
    except tomllib.TOMLDecodeError as error:
        return None, None, f"{_MANIFEST} is invalid TOML: {error}"


def _needs_dist(root: Path, repository: Repository) -> bool:
    return _cargo_has_executable(root) and has_binary_release_contract(root, repository)


def _profile_findings(
    root: Path, repository: Repository, document: dict[str, Any]
) -> list[Finding]:
    findings: list[Finding] = []
    profile = document.get("profile")
    tables = profile if isinstance(profile, dict) else {}
    if _release_status(tables.get("release")) == "deviates":
        findings.append(
            _violation(
                repository,
                _RULE_ID,
                _MANIFEST,
                "release profile must be the thin CI profile "
                '(opt-level 3, lto = "thin", panic = "abort", no codegen-units cap, unstripped)',
                fixable=True,
            )
        )
    if _needs_dist(root, repository) and _dist_status(tables.get("dist")) != "conformant":
        findings.append(
            _violation(
                repository,
                _RULE_ID,
                _MANIFEST,
                "dist profile must extend release with fat LTO, a single codegen unit, "
                "and stripped binaries for prereleases",
                fixable=True,
            )
        )
    return findings


_PRERELEASE_COMMAND = re.compile(
    r"\bgh\s+release\s+create\b[\s\S]*?(?:^|\s)--prerelease\b",
    re.IGNORECASE | re.MULTILINE,
)
_PRERELEASE_INPUT = re.compile(r"(?m)^\s*prerelease\s*:\s*true\b", re.IGNORECASE)
_GH_RELEASE_CREATE = re.compile(r"\bgh\s+release\s+create\b", re.IGNORECASE)


def _prerelease_sources(root: Path, repository: Repository) -> tuple[list[tuple[str, str]], list[Finding]]:
    sources, problems = read_workflow_sources(root, repository)
    findings = [
        _operational(repository, _RULE_ID, path, message) for path, message in problems
    ]
    prerelease = [
        (path, source)
        for path, source in sources
        if _PRERELEASE_COMMAND.search(source) is not None
        or (
            _GH_RELEASE_CREATE.search(source) is not None
            and _PRERELEASE_INPUT.search(source) is not None
        )
    ]
    return prerelease, findings


def _workflow_findings(
    root: Path, repository: Repository
) -> tuple[list[Finding], list[Finding]]:
    prerelease, operational = _prerelease_sources(root, repository)
    findings: list[Finding] = []
    for path, source in prerelease:
        for line in source.splitlines():
            if "cargo build" not in line or "--release" not in line:
                continue
            if "--profile" in line:
                continue
            findings.append(
                _violation(
                    repository,
                    _RULE_ID,
                    path,
                    "prerelease builds must use --profile dist, not --release",
                )
            )
            break
    return findings, operational


def _makefile_findings(root: Path, repository: Repository) -> list[Finding]:
    makefile = root / _MAKEFILE
    if makefile.is_symlink() or not makefile.is_file():
        return []
    try:
        lines = makefile.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError):
        return []
    findings: list[Finding] = []
    current: str | None = None
    for line in lines:
        if line and not line[0].isspace():
            match = re.match(r"^(release(?:-[A-Za-z0-9_-]+)?|dist)\s*:", line)
            if match is None:
                current = None
            else:
                name = match.group(1)
                current = name if isinstance(name, str) else None
            continue
        if current is None or not line.strip():
            continue
        if "--release" in line and "--profile" not in line:
            findings.append(
                _violation(
                    repository,
                    _RULE_ID,
                    _MAKEFILE,
                    f"release packaging target {current!r} must build with "
                    "--profile dist, not --release",
                )
            )
            current = None
    return findings


def _check(root: Path, repository: Repository) -> list[Finding]:
    if "fork" in set(repository.tags):
        return []
    document, _, problem = _read_manifest(root)
    if problem is not None:
        return [_operational(repository, _RULE_ID, _MANIFEST, problem)]
    if document is None:
        return []
    findings = _profile_findings(root, repository, document)
    workflow_findings, operational = _workflow_findings(root, repository)
    findings.extend(operational)
    findings.extend(workflow_findings)
    findings.extend(_makefile_findings(root, repository))
    return findings


def _render_table(header: str, fields: list[tuple[str, str]]) -> str:
    lines = [f"[{header}]"]
    lines.extend(f"{key} = {value}" for key, value in fields)
    return "\n".join(lines) + "\n"


def _replace_table(source: str, header: str, block: str) -> str:
    lines = source.splitlines(keepends=True)
    start: int | None = None
    end = len(lines)
    for index, line in enumerate(lines):
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if re.fullmatch(r"\[\s*" + re.escape(header) + r"\s*\]", stripped):
            start = index
            continue
        if start is not None and re.fullmatch(r"\[.*\]", stripped):
            end = index
            break
    if start is None:
        separator = "" if source.endswith("\n") or not source else "\n"
        return source + separator + block
    return "".join(lines[:start]) + block + "".join(lines[end:])


def _fixed_source(source: str, document: dict[str, Any], root: Path, repository: Repository) -> str:
    profile = document.get("profile")
    tables = profile if isinstance(profile, dict) else {}
    result = source
    if _release_status(tables.get("release")) == "deviates":
        release_block = _render_table(
            _RELEASE_TABLE,
            [
                ("opt-level", "3"),
                ("lto", '"thin"'),
                ("panic", '"abort"'),
                ("strip", "false"),
            ],
        )
        result = _replace_table(result, _RELEASE_TABLE, release_block)
    if _needs_dist(root, repository) and _dist_status(tables.get("dist")) != "conformant":
        dist_block = _render_table(
            _DIST_TABLE,
            [
                ("inherits", '"release"'),
                ("lto", '"fat"'),
                ("codegen-units", "1"),
                ("strip", "true"),
            ],
        )
        result = _replace_table(result, _DIST_TABLE, dist_block)
    return result


def _fix(root: Path, repository: Repository) -> dict[str, str]:
    if "fork" in set(repository.tags):
        return {}
    document, source, problem = _read_manifest(root)
    if problem is not None:
        raise ValueError(problem)
    if document is None or source is None:
        return {}
    updated = _fixed_source(source, document, root, repository)
    if updated == source:
        return {}
    return {_MANIFEST: updated}


def _validate(root: Path, repository: Repository, changed: tuple[str, ...]) -> list[str]:
    if _MANIFEST not in changed:
        return []
    document, _, problem = _read_manifest(root)
    if problem is not None:
        return [problem]
    if document is None:
        return [f"{_MANIFEST} is missing"]
    return [
        finding.message
        for finding in _profile_findings(root, repository, document)
        if finding.fixable
    ]


def rust_release_profiles_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            "rust-release-profiles",
            "Require thin release profiles for CI and fat dist profiles for prereleases",
            TagPredicate(),
            _check,
            _fix,
            "chore(repomod): enforce release and dist profiles",
            ("Cargo.toml",),
            _validate,
        ),
    )
