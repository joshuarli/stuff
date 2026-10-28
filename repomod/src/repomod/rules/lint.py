from __future__ import annotations

import re
from pathlib import Path

from repomod.git import GitError
from repomod.rules.actions import _validate_workflows, load_pins
from repomod.rules.consistency import _declared_make_targets
from repomod.types import Finding, Repository, Rule, TagPredicate, operational, violation

_LINT_WORKFLOW_NAMES = ("lint.yml", "lint.yaml")
_MAKE_LINT = re.compile(r"(?m)\bmake\s+(?:[^\n#]*\s)?lint\b")
_MAKEFILE_NAMES = ("GNUmakefile", "makefile", "Makefile")


def _lint_workflow_path(root: Path) -> Path | None:
    for name in _LINT_WORKFLOW_NAMES:
        candidate = root / ".github" / "workflows" / name
        if candidate.is_symlink() or candidate.is_file():
            return candidate
    return None


def _workflow_without_comments(source: str) -> str:
    return "\n".join(
        "" if line.lstrip().startswith("#") else line for line in source.splitlines()
    )


def _runs_make_lint(source: str) -> bool:
    return _MAKE_LINT.search(_workflow_without_comments(source)) is not None


def _gha_lint_workflow_findings(root: Path, repository: Repository) -> list[Finding]:
    path = _lint_workflow_path(root)
    if path is None:
        return [
            violation(
                repository,
                "gha-lint-workflow",
                ".github/workflows/lint.yml",
                "add a lint workflow that runs make lint",
                fixable=True,
            )
        ]
    relative = path.relative_to(root).as_posix()
    if path.is_symlink():
        return [
            operational(
                repository,
                "gha-lint-workflow",
                relative,
                "lint workflow is a symlink and was not inspected",
            )
        ]
    try:
        source = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        return [
            operational(
                repository,
                "gha-lint-workflow",
                relative,
                f"cannot read lint workflow: {error}",
            )
        ]
    if not _runs_make_lint(source):
        return [
            violation(
                repository,
                "gha-lint-workflow",
                relative,
                "lint workflow must run make lint",
            )
        ]
    return []


def _lint_workflow_source() -> str:
    try:
        pins = load_pins()
    except ValueError as error:
        raise GitError(f"cannot template the lint workflow: {error}") from error
    checkout = pins.get("actions/checkout")
    if checkout is None:
        raise GitError("cannot template the lint workflow: actions.toml has no actions/checkout pin")
    return (
        "name: lint\n"
        "\n"
        "on:\n"
        "  pull_request:\n"
        "  push:\n"
        "    branches: [main, master]\n"
        "\n"
        "permissions: {}\n"
        "\n"
        "concurrency:\n"
        '  group: "${{ github.workflow }}-${{ github.head_ref || github.run_id }}"\n'
        "  cancel-in-progress: true\n"
        "\n"
        "defaults:\n"
        "  run:\n"
        "    shell: bash -euo pipefail {0}\n"
        "\n"
        "jobs:\n"
        "  lint:\n"
        "    name: lint\n"
        "    runs-on: ubuntu-latest\n"
        "    permissions:\n"
        "      contents: read\n"
        "    steps:\n"
        f"      - uses: actions/checkout@{checkout.sha} # {checkout.version}\n"
        "        with:\n"
        "          persist-credentials: false\n"
        "      - name: lint\n"
        "        run: make lint\n"
    )


def _lint_workflow_fix(root: Path, repository: Repository) -> dict[str, str]:
    if _lint_workflow_path(root) is not None:
        return {}
    return {".github/workflows/lint.yml": _lint_workflow_source()}


def _root_makefile(root: Path) -> Path | None:
    for name in _MAKEFILE_NAMES:
        candidate = root / name
        if candidate.is_symlink() or candidate.exists():
            return candidate
    return None


def _make_lint_target_findings(root: Path, repository: Repository) -> list[Finding]:
    makefile = _root_makefile(root)
    if makefile is None:
        return [
            violation(
                repository,
                "make-lint-target",
                "Makefile",
                "add a root Makefile with a lint target so the lint workflow can run make lint",
            )
        ]
    relative = makefile.relative_to(root).as_posix()
    if makefile.is_symlink():
        return [
            operational(
                repository,
                "make-lint-target",
                relative,
                "root makefile is a symlink and was not inspected",
            )
        ]
    try:
        source = makefile.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        return [
            operational(
                repository,
                "make-lint-target",
                relative,
                f"cannot read root Makefile: {error}",
            )
        ]
    targets = _declared_make_targets(makefile)
    if targets is not None:
        if "lint" in targets:
            return []
    elif re.search(r"(?m)^lint\s*:", source) is not None:
        return []
    return [
        violation(
            repository,
            "make-lint-target",
            relative,
            "root Makefile does not define a lint target; "
            "add one so the lint workflow can run make lint",
        )
    ]


def lint_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            "gha-lint-workflow",
            "Require a lint workflow that runs make lint",
            TagPredicate(),
            _gha_lint_workflow_findings,
            _lint_workflow_fix,
            "chore(repomod): add lint workflow",
            (".github/workflows/lint.yml",),
            _validate_workflows,
        ),
        Rule(
            "make-lint-target",
            "Require a root Makefile lint target",
            TagPredicate(),
            _make_lint_target_findings,
            None,
            None,
            (),
        ),
    )
