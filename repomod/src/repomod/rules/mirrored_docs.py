from __future__ import annotations

from pathlib import Path

from repomod.types import Finding, Repository, Rule, TagPredicate, operational, violation

MIRRORED_TREES = (
    (Path(".agents/skills"), Path(".claude/skills")),
    (Path(".agents/agents"), Path(".claude/agents")),
)


def _regular_files(root: Path, tree: Path) -> dict[Path, Path]:
    base = root / tree
    if base.is_symlink() or not base.is_dir():
        return {}
    return {
        path.relative_to(base): path
        for path in sorted(base.rglob("*"))
        if path.is_file() and not path.is_symlink()
    }


def _mirrored_findings(root: Path, repository: Repository) -> list[Finding]:
    if "fork" in repository.tags:
        return []
    findings: list[Finding] = []
    for source, mirror in MIRRORED_TREES:
        sources = _regular_files(root, source)
        mirrors = _regular_files(root, mirror)
        for relative in sorted(sources.keys() & mirrors.keys()):
            try:
                identical = sources[relative].read_bytes() == mirrors[relative].read_bytes()
            except OSError as error:
                findings.append(
                    operational(
                        repository,
                        "mirrored-docs",
                        (mirror / relative).as_posix(),
                        f"cannot compare with {(source / relative).as_posix()}: {error}",
                    )
                )
                continue
            if identical:
                findings.append(
                    violation(
                        repository,
                        "mirrored-docs",
                        (mirror / relative).as_posix(),
                        f"identical copy of {(source / relative).as_posix()}; "
                        f"make {mirror.as_posix()} a symlink to {source.as_posix()} "
                        "or the reverse",
                    )
                )
    return findings


def mirrored_docs_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            "mirrored-docs",
            "Reject agent doc trees mirrored by copy instead of symlink",
            TagPredicate(),
            _mirrored_findings,
            None,
            None,
            (),
        ),
    )
