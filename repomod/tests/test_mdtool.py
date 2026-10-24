from __future__ import annotations

from pathlib import PurePosixPath

from repomod.rules.mdtool import _is_excluded


def test_excluded_fixture_directories() -> None:
    assert _is_excluded(PurePosixPath("tests/upstream/defuddle/expected/content.md"))
    assert _is_excluded(PurePosixPath("internal/attachments/testdata/references.md"))
    assert _is_excluded(PurePosixPath("tests/fixtures/demo-vault/note.md"))
    assert _is_excluded(PurePosixPath("tests/snapshots/output.md"))


def test_excluded_tracking_files() -> None:
    assert _is_excluded(PurePosixPath("TODO.md"))
    assert _is_excluded(PurePosixPath("docs/TODO.md"))


def test_maintained_documentation_is_checked() -> None:
    assert not _is_excluded(PurePosixPath("AGENTS.md"))
    assert not _is_excluded(PurePosixPath("README.md"))
    assert not _is_excluded(PurePosixPath("docs/rfc/PAR-1-cooperative-worker-shutdown.md"))
    assert not _is_excluded(PurePosixPath("grab/docs/architecture.md"))
    assert not _is_excluded(PurePosixPath(".github/workflows/ci.yml"))


def test_exclusion_boundaries() -> None:
    assert not _is_excluded(PurePosixPath("todo.md"))
    assert not _is_excluded(PurePosixPath("docs/fixtures.md"))
    assert not _is_excluded(PurePosixPath("expected"))
