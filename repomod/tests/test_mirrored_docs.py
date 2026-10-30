from __future__ import annotations

from pathlib import Path

from repomod.rules.mirrored_docs import _mirrored_findings
from repomod.types import RepoId, Repository


def repository(*tags: str) -> Repository:
    return Repository(RepoId("acme", "demo"), tuple(tags))


def write(root: Path, relative: str, text: str) -> None:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def test_identical_copies_are_flagged_and_differing_files_are_not(tmp_path: Path) -> None:
    write(tmp_path, ".agents/skills/perf/SKILL.md", "same\n")
    write(tmp_path, ".claude/skills/perf/SKILL.md", "same\n")
    write(tmp_path, ".agents/skills/perf/notes.md", "one\n")
    write(tmp_path, ".claude/skills/perf/notes.md", "two\n")
    write(tmp_path, ".agents/skills/only/SKILL.md", "same\n")
    write(tmp_path, ".claude/agents/a.md", "agent\n")
    write(tmp_path, ".agents/agents/a.md", "agent\n")

    findings = _mirrored_findings(tmp_path, repository())

    assert [(item.rule, item.file, item.fixable) for item in findings] == [
        ("mirrored-docs", ".claude/skills/perf/SKILL.md", False),
        ("mirrored-docs", ".claude/agents/a.md", False),
    ]
    assert "make .claude/skills a symlink to .agents/skills" in findings[0].message


def test_symlinked_trees_and_files_are_clean(tmp_path: Path) -> None:
    write(tmp_path, ".agents/skills/perf/SKILL.md", "same\n")
    (tmp_path / ".claude").mkdir()
    (tmp_path / ".claude/skills").symlink_to("../.agents/skills")
    write(tmp_path, ".agents/agents/a.md", "agent\n")
    (tmp_path / ".claude/agents").mkdir()
    (tmp_path / ".claude/agents/a.md").symlink_to("../../.agents/agents/a.md")

    assert _mirrored_findings(tmp_path, repository()) == []


def test_fork_repositories_are_skipped(tmp_path: Path) -> None:
    write(tmp_path, ".agents/skills/perf/SKILL.md", "same\n")
    write(tmp_path, ".claude/skills/perf/SKILL.md", "same\n")

    assert _mirrored_findings(tmp_path, repository("fork")) == []
