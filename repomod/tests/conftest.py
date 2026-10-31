from __future__ import annotations

from pathlib import Path

import pytest


@pytest.fixture
def git_environment(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    global_config = tmp_path / "gitconfig"
    global_config.write_text(
        "[user]\n\tname = Test User\n\temail = test@example.invalid\n",
        encoding="utf-8",
    )
    home = tmp_path / "home"
    home.mkdir()
    monkeypatch.setenv("GIT_CONFIG_GLOBAL", str(global_config))
    monkeypatch.setenv("GIT_CONFIG_NOSYSTEM", "1")
    monkeypatch.setenv("GIT_TERMINAL_PROMPT", "0")
    monkeypatch.setenv("HOME", str(home))
