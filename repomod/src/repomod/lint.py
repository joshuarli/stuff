"""Validation of a fix's output: actionlint on workflows, plus fix-defined checks."""

import json
import re
import subprocess
from pathlib import Path
from typing import Any

from repomod.git import changed_files, stashed_changes
from repomod.types import FixModule, LintFinding
from repomod.utils import SUBPROCESS_TIMEOUT

WORKFLOW_RE = re.compile(r"^\.github/workflows/.+\.ya?ml$")


def run_actionlint(repo_dir: Path, files: list[str]) -> set[LintFinding]:
    """Lint repo-relative workflow files. actionlint exits 1 when it finds errors."""
    result = subprocess.run(
        ["actionlint", "-format", "{{json .}}", *files],
        cwd=repo_dir,
        capture_output=True,
        text=True,
        timeout=SUBPROCESS_TIMEOUT,
    )
    if result.returncode not in (0, 1):
        raise subprocess.CalledProcessError(
            result.returncode, result.args, result.stdout, result.stderr
        )
    return {
        LintFinding(f["filepath"], f["message"])
        for f in json.loads(result.stdout) or []
    }


def new_workflow_findings(repo_dir: Path) -> set[LintFinding]:
    """actionlint errors in modified workflows that weren't there before the fix.

    Pre-existing errors are ignored: a fix shouldn't be blocked by problems it
    didn't cause. The baseline is linted in place (changes stashed) rather than
    from a copy, so actionlint sees the same project context, e.g. local actions.
    """
    files = [
        f
        for f in changed_files(repo_dir)
        if WORKFLOW_RE.match(f) and (repo_dir / f).exists()
    ]
    if not files:
        return set()
    after = run_actionlint(repo_dir, files)
    if not after:
        return set()
    with stashed_changes(repo_dir):
        present = [f for f in files if (repo_dir / f).exists()]
        before = run_actionlint(repo_dir, present) if present else set()
    return after - before


def validate_changes(
    fix: FixModule, repo_dir: Path, config: dict[str, Any]
) -> list[str]:
    """Problems the fix introduced; empty means it is safe to propose.

    Workflow files are always checked with actionlint. A fix can add its own
    checks by defining an optional `validate(repo, config) -> list[str]`.
    """
    errors = [f"{f.path}: {f.message}" for f in sorted(new_workflow_findings(repo_dir))]
    custom = getattr(fix, "validate", None)
    if custom is not None:
        errors += custom(repo_dir, config)
    return errors
