from __future__ import annotations

from pathlib import Path

from repomod.types import Finding, Repository, Rule, TagPredicate, violation

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


def _has_extension_manifest(root: Path) -> bool:
    candidates = [root / "manifest.json"]
    for project in root.iterdir():
        if project.is_symlink() or not project.is_dir():
            continue
        candidates.extend((project / "manifest.json", project / "src" / "manifest.json"))
    return any(path.is_file() and not path.is_symlink() for path in candidates)


def _template_findings(root: Path, repository: Repository) -> list[Finding]:
    tags = set(repository.tags)
    readme = _has_any(root, ("README.md", "README.rst", "README", "readme.md"))
    has_extension_manifest = "extension" in tags and _has_extension_manifest(root)
    metadata = _has_any(root, _METADATA) or has_extension_manifest
    findings: list[Finding] = []
    if not readme and tags & {"tool", "library", "app", "extension"}:
        findings.append(
            violation(repository, "repo-template", None, "add a human-written README", fixable=False)
        )
    if not metadata and tags & {"tool", "library", "app", "extension", "system", "build"}:
        findings.append(
            violation(
                repository,
                "repo-template",
                None,
                "add project metadata or an understandable build entry point",
                fixable=False,
            )
        )
    if "extension" in tags and not has_extension_manifest:
        findings.append(
            violation(repository, "repo-template", None, "add the browser or editor extension manifest")
        )
    if "library" in tags and readme and not metadata:
        findings.append(
            violation(repository, "repo-template", None, "document package metadata and public API")
        )
    return findings


def _integrity_findings(root: Path, repository: Repository) -> list[Finding]:
    findings: list[Finding] = []
    git_path = root / ".git"
    if not git_path.exists():
        findings.append(violation(repository, "repo-integrity", ".git", "checkout has no .git metadata"))
    findings.extend(
        [
            violation(
                repository,
                "repo-integrity",
                candidate.name,
                "top-level policy file is a symlink",
            )
            for candidate in (
                root / "README.md",
                root / "Cargo.toml",
                root / "pyproject.toml",
                root / "package.json",
            )
            if candidate.is_symlink() and candidate.parent == root
        ]
    )
    return findings


def template_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            "repo-integrity",
            "Check basic repository integrity",
            TagPredicate(),
            _integrity_findings,
            None,
            None,
            (),
        ),
        Rule(
            "repo-template",
            "Check semantic documentation and metadata expectations from repository tags",
            TagPredicate(
                any_of=frozenset(
                    {"tool", "library", "app", "extension", "system", "build", "config", "data"}
                )
            ),
            _template_findings,
            None,
            None,
            (),
        ),
    )
