from __future__ import annotations

from pathlib import Path

import pytest

from repomod.git import run_git
from repomod.rules.formatters import _commands, _formatting_findings
from repomod.types import RepoId, Repository


@pytest.mark.parametrize(
    ("source", "expected"),
    [
        ("cargo fmt --all --check", ("cargo fmt",)),
        ("cargo +nightly fmt --all", ("cargo fmt",)),
        ("rustfmt --check src/lib.rs", ("rustfmt",)),
        ("uv run --locked ruff format src tests", ("ruff format",)),
        ("prettier --write .", ("prettier",)),
        ("go fmt ./...", ("go fmt",)),
        ("pip install black", ("black",)),
        ("echo black", ()),
        ("complete -W 'black white' rg", ()),
        (
            "_describe -t $tag 'color name or r,g,b' '(\n"
            "  black blue green red cyan magenta yellow white\n"
            ")'>/dev/null",
            (),
        ),
        (
            "colors=(\n  black\n  white\n)",
            (),
        ),
        (
            "build() {\n  cargo fmt --check\n}",
            ("cargo fmt",),
        ),
    ],
)
def test_formatter_command_detection(source: str, expected: tuple[str, ...]) -> None:
    assert _commands(source) == expected


def test_vendored_formatter_config_is_left_to_its_upstream(tmp_path: Path) -> None:
    run_git(["init", "--quiet"], cwd=tmp_path)
    hook = "repos:\n  - hooks:\n      - id: fmt\n        entry: cargo fmt --all\n"
    for relative in (".pre-commit-config.yaml", "backend/vendor/rtls/.pre-commit-config.yaml"):
        path = tmp_path / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(hook, encoding="utf-8")
    run_git(["add", "."], cwd=tmp_path)

    findings = _formatting_findings(tmp_path, Repository(RepoId("acme", "demo"), ("tool",)))

    assert [finding.file for finding in findings] == [".pre-commit-config.yaml"]
