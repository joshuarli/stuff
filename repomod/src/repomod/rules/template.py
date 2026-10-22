from __future__ import annotations

from pathlib import Path

from repomod.types import Finding, Repository, Rule

_METADATA = (
    "Cargo.toml",
    "pyproject.toml",
    "package.json",
    "go.mod",
    "Package.swift",
    "Makefile",
    "justfile",
)


def _has_any(root: Path, names: tuple[str, ...]) -> bool:
    return any((root / name).is_file() and not (root / name).is_symlink() for name in names)


def _template_findings(root: Path, repository: Repository) -> list[Finding]:
    tags = set(repository.tags)
    readme = _has_any(root, ("README.md", "README.rst", "README", "readme.md"))
    metadata = _has_any(root, _METADATA)
    findings: list[Finding] = []
    if not readme and tags & {"tool", "library", "app", "extension"}:
        findings.append(
            Finding(str(repository.id), "repo-template", None, "add a human-written README", fixable=False)
        )
    if not metadata and tags & {"tool", "library", "app", "extension", "system", "build"}:
        findings.append(
            Finding(
                str(repository.id),
                "repo-template",
                None,
                "add project metadata or an understandable build entry point",
                fixable=False,
            )
        )
    if "extension" in tags and not _has_any(root, ("manifest.json",)):
        findings.append(
            Finding(str(repository.id), "repo-template", None, "add the browser or editor extension manifest")
        )
    if "library" in tags and readme and not metadata:
        findings.append(
            Finding(str(repository.id), "repo-template", None, "document package metadata and public API")
        )
    return findings


def _integrity_findings(root: Path, repository: Repository) -> list[Finding]:
    findings: list[Finding] = []
    git_path = root / ".git"
    if not git_path.exists():
        findings.append(Finding(str(repository.id), "repo-integrity", ".git", "checkout has no .git metadata"))
    for candidate in (root / "README.md", root / "Cargo.toml", root / "pyproject.toml", root / "package.json"):
        if candidate.is_symlink() and candidate.parent == root:
            findings.append(
                Finding(str(repository.id), "repo-integrity", candidate.name, "top-level policy file is a symlink")
            )
    return findings


def template_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            "repo-integrity",
            "Check basic repository integrity",
            frozenset(),
            _integrity_findings,
            None,
            None,
            (),
        ),
        Rule(
            "repo-template",
            "Check semantic documentation and metadata expectations from repository tags",
            frozenset({"tool", "library", "app", "extension", "system", "build", "config", "data"}),
            _template_findings,
            None,
            None,
            (),
        ),
    )
