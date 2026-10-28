from __future__ import annotations

import re
from pathlib import Path

from repomod.git import GitError
from repomod.rules.actions import _validate_workflows, load_pins
from repomod.rules.consistency import _declared_make_targets
from repomod.rules.languages import _language_sources
from repomod.rules.python_standard import load_approved_python_tool_versions
from repomod.rules.typescript_standard import load_typescript_tool_pins
from repomod.types import Finding, Repository, Rule, TagPredicate, operational, violation

_LINT_WORKFLOW_NAMES = ("lint.yml", "lint.yaml")
_MAKE_LINT = re.compile(r"(?m)\bmake\s+(?:[^\n#]*\s)?lint\b")
_MAKEFILE_NAMES = ("GNUmakefile", "makefile", "Makefile")
_TARGET = re.compile(r"^([A-Za-z0-9_./%-][^:]*?)\s*:(?!/=)([^=]*)$")
_PACKAGE_LINT = re.compile(
    r"\b(?:pnpm|npm|yarn)\b[^\n]*\brun\s+(?:lint|check)\b"
    r"|\bpnpm\s+(?:lint|check)\b"
    r"|\bnpm\s+run\s+(?:lint|check)\b"
)


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


def _setup_steps(languages: set[str]) -> list[str]:
    try:
        pins = load_pins()
    except ValueError as error:
        raise GitError(f"cannot template the lint workflow: {error}") from error
    steps: list[str] = []
    if "python" in languages:
        setup = pins.get("astral-sh/setup-uv")
        if setup is None:
            raise GitError("cannot template the lint workflow: actions.toml has no astral-sh/setup-uv pin")
        try:
            uv = load_approved_python_tool_versions()["uv"]
        except ValueError as error:
            raise GitError(f"cannot template the lint workflow: {error}") from error
        steps.extend(
            [
                f"      - uses: astral-sh/setup-uv@{setup.sha} # {setup.version}",
                "        with:",
                f"          version: {uv}",
                "      - run: uv python install 3.15",
            ]
        )
    if {"typescript", "javascript"} & languages:
        setup = pins.get("pnpm/setup")
        if setup is None:
            raise GitError("cannot template the lint workflow: actions.toml has no pnpm/setup pin")
        try:
            tools = load_typescript_tool_pins()
        except ValueError as error:
            raise GitError(f"cannot template the lint workflow: {error}") from error
        steps.extend(
            [
                f"      - uses: pnpm/setup@{setup.sha} # {setup.version}",
                "        with:",
                f"          version: {tools.pnpm}",
                f"          runtime: node@{tools.node}",
                "          install: false",
            ]
        )
    return steps


def _install_steps(root: Path, languages: set[str]) -> list[str]:
    if {"typescript", "javascript"} & languages:
        if (root / "pnpm-lock.yaml").is_file():
            return ["      - run: pnpm install --frozen-lockfile"]
        return ["      - run: pnpm install"]
    return []


def _lint_workflow_source(root: Path, languages: set[str]) -> str:
    try:
        pins = load_pins()
    except ValueError as error:
        raise GitError(f"cannot template the lint workflow: {error}") from error
    checkout = pins.get("actions/checkout")
    if checkout is None:
        raise GitError("cannot template the lint workflow: actions.toml has no actions/checkout pin")
    lines = [
        "name: lint",
        "",
        "on:",
        "  pull_request:",
        "  push:",
        "    branches: [main, master]",
        "",
        "permissions: {}",
        "",
        "concurrency:",
        '  group: "${{ github.workflow }}-${{ github.head_ref || github.run_id }}"',
        "  cancel-in-progress: true",
        "",
        "defaults:",
        "  run:",
        "    shell: bash -euo pipefail {0}",
        "",
        "jobs:",
        "  lint:",
        "    name: lint",
        "    runs-on: ubuntu-latest",
        "    permissions:",
        "      contents: read",
        "    steps:",
        f"      - uses: actions/checkout@{checkout.sha} # {checkout.version}",
        "        with:",
        "          persist-credentials: false",
        *_setup_steps(languages),
        *_install_steps(root, languages),
        "      - name: lint",
        "        run: make lint",
    ]
    return "\n".join(lines) + "\n"


def _lint_workflow_fix(root: Path, repository: Repository) -> dict[str, str]:
    if _lint_workflow_path(root) is not None:
        return {}
    return {".github/workflows/lint.yml": _lint_workflow_source(root, _language_sources(root))}


def _root_makefile(root: Path) -> Path | None:
    for name in _MAKEFILE_NAMES:
        candidate = root / name
        if candidate.is_symlink() or candidate.exists():
            return candidate
    return None


def _makefile_parts(source: str) -> tuple[dict[str, list[str]], dict[str, list[str]]]:
    recipes: dict[str, list[str]] = {}
    prerequisites: dict[str, list[str]] = {}
    current: str | None = None
    for line in source.splitlines():
        if not line or line[0] in (" ", "\t"):
            if current is not None:
                recipes[current].append(line.strip())
            continue
        if line.lstrip().startswith("#"):
            current = None
            continue
        match = _TARGET.match(line)
        if match is None or match.group(2).strip().startswith("="):
            current = None
            continue
        parts: list[str] = [part for part in match.group(1).split() if "%" not in part and "$" not in part]
        if not parts:
            current = None
            continue
        current = parts[0]
        recipes.setdefault(current, [])
        prerequisites[current] = [
            part
            for part in match.group(2).split()
            if "%" not in part and "$" not in part and "=" not in part
        ]
    return recipes, prerequisites


def _lint_recipe_text(source: str) -> str | None:
    recipes, prerequisites = _makefile_parts(source)
    if "lint" not in recipes:
        return None
    seen = {"lint"}
    queue = ["lint"]
    chunks: list[str] = []
    while queue:
        name = queue.pop(0)
        chunks.extend(recipes.get(name, []))
        for prerequisite in prerequisites.get(name, []):
            if prerequisite not in seen:
                seen.add(prerequisite)
                queue.append(prerequisite)
    return "\n".join(chunk for chunk in chunks if chunk)


def _linter_findings(
    repository: Repository, relative: str, languages: set[str], recipe: str
) -> list[Finding]:
    findings = []
    lowered = recipe.lower()
    if "--fix" in lowered:
        findings.append(
            violation(
                repository,
                "make-lint-target",
                relative,
                "lint target must be a non-mutating check; remove --fix",
            )
        )
    if "rust" in languages and "clippy" not in lowered:
        findings.append(
            violation(
                repository,
                "make-lint-target",
                relative,
                "lint target must run cargo clippy for Rust sources",
            )
        )
    if "python" in languages:
        if "ruff" not in lowered:
            findings.append(
                violation(
                    repository,
                    "make-lint-target",
                    relative,
                    "lint target must run ruff for Python sources",
                )
            )
        if "flake8" in lowered:
            findings.append(
                violation(
                    repository,
                    "make-lint-target",
                    relative,
                    "lint target must never run flake8; use ruff",
                )
            )
    if {"typescript", "javascript"} & languages and (
        "oxlint" not in lowered and _PACKAGE_LINT.search(recipe) is None
    ):
        findings.append(
            violation(
                repository,
                "make-lint-target",
                relative,
                "lint target must run oxlint for TypeScript sources",
            )
        )
    return findings


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
        if "lint" not in targets:
            return [
                violation(
                    repository,
                    "make-lint-target",
                    relative,
                    "root Makefile does not define a lint target; "
                    "add one so the lint workflow can run make lint",
                )
            ]
    elif re.search(r"(?m)^lint\s*:", source) is None:
        return [
            violation(
                repository,
                "make-lint-target",
                relative,
                "root Makefile does not define a lint target; "
                "add one so the lint workflow can run make lint",
            )
        ]
    recipe = _lint_recipe_text(source) or ""
    return _linter_findings(repository, relative, _language_sources(root), recipe)


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
