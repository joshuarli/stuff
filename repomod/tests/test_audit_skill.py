from __future__ import annotations

import re
from pathlib import Path

import pytest

from repomod.cli import _parser

ROOT = Path(__file__).resolve().parents[1]
SKILL = ROOT / ".agents/skills/audit-dependencies/SKILL.md"


def _frontmatter(text: str) -> str:
    assert text.startswith("---\n")
    end = text.index("\n---\n", 4)
    return text[4:end]


def test_frontmatter_and_description() -> None:
    text = SKILL.read_text(encoding="utf-8")
    front = _frontmatter(text)
    assert "name: audit-dependencies" in front
    assert "description:" in front
    assert "dependency inventory" in front
    assert len(front.splitlines()) == 2


def test_workflow_uses_every_required_command() -> None:
    text = SKILL.read_text(encoding="utf-8")
    commands = set(re.findall(r"uv run repomod deps ([a-z]+)", text))
    assert {"check", "render", "why", "report", "diff", "enrich"} <= commands
    assert "snapshot" in commands


def test_each_referenced_command_exists_in_help(
    capsys: pytest.CaptureFixture[str],
) -> None:
    text = SKILL.read_text(encoding="utf-8")
    commands = set(re.findall(r"uv run repomod deps ([a-z]+)", text))
    with pytest.raises(SystemExit):
        _parser().parse_args(["deps", "--help"])
    output = capsys.readouterr().out
    for command in sorted(commands):
        assert command in output, command


def test_links_to_the_operator_docs() -> None:
    text = SKILL.read_text(encoding="utf-8")
    for relative in (
        "../../../docs/dependency-inventory.md",
        "../../../docs/dependency-dashboard.md",
        "../../../docs/tagging.md",
    ):
        assert relative in text
        assert (SKILL.parent / relative).resolve().is_file()


def test_states_the_musts_and_the_workflow() -> None:
    text = SKILL.read_text(encoding="utf-8")
    for heading in ("## Always", "## Ask first", "## Never", "## Dashboard"):
        assert heading in text
    for term in (
        "overview",
        "reverse consumers",
        "divergence",
        "transitive",
        "freshness",
        "first-party",
        "apply",
        "not checked",
        "no baseline",
        "fetched_at",
    ):
        assert term in text
    assert "packages.html#pkg=" in text
    assert ".work/dependencies/dashboard" in text


def test_does_not_duplicate_the_command_reference() -> None:
    text = SKILL.read_text(encoding="utf-8")
    assert "--output" not in text
    assert "--workers" not in text
    assert "data/dependencies/v1" in text
