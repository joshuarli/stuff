import os
import sys
from collections.abc import Iterator
from pathlib import Path
from unittest.mock import patch

import pytest
from gh_harness import GH_SCRIPT, FakeGh

from repomod.utils import github_rate_limiter, pr_create_limiter


@pytest.fixture(autouse=True)
def _offline_preflight() -> Iterator[None]:
    """Tests must not depend on gh auth or actionlint being installed."""
    with patch("repomod.main.preflight"):
        yield


@pytest.fixture
def fake_gh(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> FakeGh:
    """A recording `gh` stub first on PATH; see gh_harness."""
    root = tmp_path / "fake-gh"
    root.mkdir()
    script = root / "gh"
    script.write_text(GH_SCRIPT.replace("__PYTHON__", sys.executable))
    script.chmod(0o755)
    monkeypatch.setenv("PATH", f"{root}:{os.environ['PATH']}")
    return FakeGh(root)


@pytest.fixture(autouse=True)
def _no_github_rate_limit(monkeypatch: pytest.MonkeyPatch) -> None:
    """The real spacing would only slow tests down."""
    monkeypatch.setattr(github_rate_limiter, "_min_delay", 0)
    monkeypatch.setattr(pr_create_limiter, "_min_delay", 0)

