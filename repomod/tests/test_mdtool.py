from __future__ import annotations

import json
import stat
from pathlib import Path, PurePosixPath

import pytest

from repomod.policy import PolicyError, validate_ignored_rules
from repomod.rules.mdtool import _is_excluded, _mdtool_findings, is_mdtool_code_ignore
from repomod.rules.workspace import is_workspace_excluded, workspace_excluded
from repomod.types import RepoId, Repository


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


def test_advisories_and_project_titles_are_not_findings(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    records = [
        {"code": code, "severity": "warn", "file": "src/main.rs", "line": 1, "col": 1,
         "message": "advisory", "target": "README.md"}
        for code in ("C001", "C002", "C003")
    ] + [
        {"code": "T003", "severity": "warn", "file": "README.md", "line": 1, "col": 1,
         "message": "README has a developer-topic heading", "target": "# demo"},
        {"code": "T003", "severity": "warn", "file": "README.md", "line": 5, "col": 1,
         "message": "README has a developer-topic heading", "target": "## Build"},
        {"code": "L001", "severity": "error", "file": "README.md", "line": 3, "col": 1,
         "message": "destination does not exist", "target": "missing.md"}
    ]
    executable = tmp_path / ".local" / "bin" / "mdtool"
    executable.parent.mkdir(parents=True)
    output = "\n".join(json.dumps(record) for record in records)
    executable.write_text(f"#!/bin/sh\ncat <<'EOF'\n{output}\nEOF\nexit 1\n", encoding="utf-8")
    executable.chmod(executable.stat().st_mode | stat.S_IXUSR)
    monkeypatch.setattr(Path, "home", lambda: tmp_path)

    findings = _mdtool_findings(tmp_path, Repository(RepoId("acme", "demo"), ()))

    assert [finding.message for finding in findings] == [
        "line 5:1: T003 README has a developer-topic heading: ## Build",
        "line 3:1: L001 destination does not exist: missing.md",
    ]


def test_per_code_ignore_drops_only_that_code(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    records = [
        {"code": code, "severity": "warn", "file": "README.md", "line": 1, "col": 1,
         "message": "finding", "target": "x"}
        for code in ("R005", "L001")
    ]
    executable = tmp_path / ".local" / "bin" / "mdtool"
    executable.parent.mkdir(parents=True)
    output = "\n".join(json.dumps(record) for record in records)
    executable.write_text(f"#!/bin/sh\ncat <<'EOF'\n{output}\nEOF\n", encoding="utf-8")
    executable.chmod(executable.stat().st_mode | stat.S_IXUSR)
    monkeypatch.setattr(Path, "home", lambda: tmp_path)
    repository = Repository(RepoId("acme", "demo"), (), ignored_rules=frozenset({"mdtool:R005"}))

    findings = _mdtool_findings(tmp_path, repository)

    assert [finding.message for finding in findings] == ["line 1:1: L001 finding: x"]


def test_only_well_formed_mdtool_code_ignores_validate() -> None:
    assert is_mdtool_code_ignore("mdtool:R005")
    assert not is_mdtool_code_ignore("mdtool:r5")
    assert not is_mdtool_code_ignore("mdtool")
    with pytest.raises(PolicyError):
        validate_ignored_rules((Repository(RepoId("acme", "demo"), (), ignored_rules=frozenset({"mdtool:nope"})),))
