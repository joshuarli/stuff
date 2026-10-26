from __future__ import annotations

from pathlib import Path, PurePosixPath

import pytest

from repomod.rules.mdtool import _is_excluded
from repomod.rules.workspace import is_workspace_excluded, workspace_excluded


@pytest.mark.parametrize(
    ("path", "excluded"),
    [
        ("tests/upstream/defuddle/expected/content.md", True),
        ("internal/attachments/testdata/references.md", True),
        ("tests/fixtures/demo-vault/note.md", True),
        ("tests/snapshots/output.md", True),
        ("TODO.md", True),
        ("docs/TODO.md", True),
        ("AGENTS.md", False),
        ("README.md", False),
        ("docs/rfc/PAR-1-cooperative-worker-shutdown.md", False),
        ("grab/docs/architecture.md", False),
        (".github/workflows/ci.yml", False),
        ("todo.md", False),
        ("docs/fixtures.md", False),
        ("expected", False),
    ],
)
def test_documentation_audit_exclusions(path: str, excluded: bool) -> None:
    assert _is_excluded(PurePosixPath(path)) is excluded


def test_workspace_excluded_dirs_skip_vendored_forks(tmp_path: Path) -> None:
    (tmp_path / "Cargo.toml").write_text(
        '[workspace]\nexclude = ["crates/laputa-h2-futures-lite"]\n', encoding="utf-8"
    )

    prefixes = workspace_excluded(tmp_path)

    assert is_workspace_excluded(prefixes, PurePosixPath("crates/laputa-h2-futures-lite/README.md"))
    assert not is_workspace_excluded(prefixes, PurePosixPath("crates/h12tiny-core/README.md"))
    assert not is_workspace_excluded(prefixes, PurePosixPath("README.md"))
    assert workspace_excluded(tmp_path / "missing") == frozenset()
