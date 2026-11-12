from __future__ import annotations

from pathlib import Path

import pytest


@pytest.fixture(autouse=True)
def public_repositories(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(
        "repomod.github.repository_visibility",
        lambda repositories, **_: {str(repository.id): False for repository in repositories},
    )


@pytest.fixture(autouse=True)
def unpaced_crates_io(monkeypatch: pytest.MonkeyPatch) -> None:
    from repomod import audit

    monkeypatch.setattr(audit, "_CRATES_PACER", audit._Pacer(0.0))


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
