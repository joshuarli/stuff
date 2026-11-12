from __future__ import annotations

import pytest

from repomod.rules.formatters import _commands


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
