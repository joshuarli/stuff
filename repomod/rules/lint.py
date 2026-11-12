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
    if {"typescript", "javascript"} & languages and (root / "package.json").is_file():
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
        "    runs-on: ubuntu-26.04-arm",
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


_CLEAN_FIND_PRUNE = "find . -path './.git' -prune -o -path './.work' -prune -o"


def _clean_requirements(languages: set[str]) -> list[tuple[str, tuple[str, ...], tuple[str, ...]]]:
    requirements: list[tuple[str, tuple[str, ...], tuple[str, ...]]] = []
    if {"javascript", "typescript"} & languages:
        requirements.append(
            (
                "all node_modules directories",
                ("node_modules",),
                ("rm", "find"),
            )
        )
    if "python" in languages:
        requirements.append(("the root .venv directory", (".venv",), ("rm", "find")))
        requirements.append(("all __pycache__ directories", ("__pycache__",), ("rm", "find")))
    if "rust" in languages:
        requirements.append(("Rust build artifacts", ("target", "cargo clean"), ("rm", "find", "cargo")))
    if "swift" in languages:
        requirements.append(("Swift build artifacts", (".build",), ("rm", "find")))
    if "zig" in languages:
        requirements.append(("Zig cache directories", (".zig-cache",), ("rm", "find")))
        requirements.append(("Zig output directories", ("zig-out",), ("rm", "find")))
    return requirements


def _clean_recipe_lines(languages: set[str]) -> list[str]:
    lines: list[str] = []
    if "python" in languages:
        lines.append("\trm -rf .venv")
        lines.append(f"\t{_CLEAN_FIND_PRUNE} -name __pycache__ -type d -prune -exec rm -rf {{}} +")
    if {"javascript", "typescript"} & languages:
        lines.append(f"\t{_CLEAN_FIND_PRUNE} -name node_modules -type d -prune -exec rm -rf {{}} +")
    if "rust" in languages:
        lines.append(f"\t{_CLEAN_FIND_PRUNE} -name target -type d -prune -exec rm -rf {{}} +")
    if "swift" in languages:
        lines.append(f"\t{_CLEAN_FIND_PRUNE} -name .build -type d -prune -exec rm -rf {{}} +")
    if "zig" in languages:
        lines.append(f"\t{_CLEAN_FIND_PRUNE} -name .zig-cache -type d -prune -exec rm -rf {{}} +")
        lines.append(f"\t{_CLEAN_FIND_PRUNE} -name zig-out -type d -prune -exec rm -rf {{}} +")
    if not lines:
        lines.append("\t@:")
    return lines


def _clean_target_block(languages: set[str]) -> str:
    return "\n".join([".PHONY: clean", "clean:", *_clean_recipe_lines(languages)]) + "\n"


def _clean_recipe_text(source: str) -> str | None:
    recipes, _ = _makefile_parts(source)
    if "clean" not in recipes:
        return None
    return "\n".join(chunk for chunk in recipes["clean"] if chunk)


def _missing_clean_requirements(languages: set[str], recipe: str) -> list[str]:
    missing: list[str] = []
    lowered = recipe.lower()
    for label, tokens, actions in _clean_requirements(languages):
        if not any(token.lower() in lowered for token in tokens):
            missing.append(label)
            continue
        if not any(action in lowered for action in actions):
            missing.append(label)
    return missing


def _make_clean_target_findings(root: Path, repository: Repository) -> list[Finding]:
    languages = _language_sources(root)
    makefile = _root_makefile(root)
    if makefile is None:
        return [
            violation(
                repository,
                "make-clean-target",
                "Makefile",
                "add a root Makefile with a clean target that removes language-specific build artifacts",
                fixable=True,
            )
        ]
    relative = makefile.relative_to(root).as_posix()
    if makefile.is_symlink():
        return [
            operational(
                repository,
                "make-clean-target",
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
                "make-clean-target",
                relative,
                f"cannot read root Makefile: {error}",
            )
        ]
    targets = _declared_make_targets(makefile)
    has_clean = "clean" in targets if targets is not None else re.search(r"(?m)^clean\s*:", source) is not None
    if not has_clean:
        return [
            violation(
                repository,
                "make-clean-target",
                relative,
                "root Makefile does not define a clean target",
                fixable=True,
            )
        ]
    recipe = _clean_recipe_text(source)
    if recipe is None:
        return [
            violation(
                repository,
                "make-clean-target",
                relative,
                "clean target recipe could not be determined",
            )
        ]
    missing = _missing_clean_requirements(languages, recipe)
    if not missing:
        return []
    return [
        violation(
            repository,
            "make-clean-target",
            relative,
            "clean target must remove " + ", ".join(missing),
            fixable=True,
        )
    ]


def _make_clean_target_fix(root: Path, repository: Repository) -> dict[str, str]:
    languages = _language_sources(root)
    makefile = _root_makefile(root)
    block = _clean_target_block(languages)
    if makefile is None:
        return {"Makefile": block}
    relative = makefile.relative_to(root).as_posix()
    try:
        source = makefile.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        raise GitError(f"cannot read root Makefile: {error}") from error
    targets = _declared_make_targets(makefile)
    has_clean = "clean" in targets if targets is not None else re.search(r"(?m)^clean\s*:", source) is not None
    if not has_clean:
        new_source = source
        if new_source and not new_source.endswith("\n"):
            new_source += "\n"
        if new_source:
            new_source += "\n"
        new_source += block
        return {relative: new_source}
    # replace existing clean rule block
    lines = source.splitlines(keepends=True)
    start = None
    end = len(lines)
    for index, line in enumerate(lines):
        stripped = line.lstrip()
        if stripped.startswith("#"):
            continue
        match = _TARGET.match(line.rstrip("\r\n"))
        if match is None:
            continue
        names = [part for part in match.group(1).split() if "%" not in part and "$" not in part]
        if "clean" in names:
            start = index
            j = index + 1
            while j < len(lines):
                nxt = lines[j]
                if nxt.startswith(("\t", " ")) or nxt.strip() == "":
                    # recipe lines or blank within recipe - keep consuming while indented
                    if nxt.strip() == "" and j + 1 < len(lines) and not lines[j + 1].startswith(
                        ("\t", " ")
                    ):
                        end = j
                        break
                    j += 1
                    continue
                end = j
                break
            else:
                end = j
            break
    if start is None:
        raise GitError("clean target could not be located for replacement")
    replacement = [line + "\n" for line in _clean_recipe_lines(languages)]
    new_lines = lines[: start + 1] + replacement + lines[end:]
    # ensure .PHONY includes clean
    joined = "".join(new_lines)
    if re.search(r"(?m)^\.PHONY\s*:.*\bclean\b", joined) is None:
        # try to extend existing .PHONY or add
        if re.search(r"(?m)^\.PHONY\s*:", joined):
            joined = re.sub(r"(?m)^\.PHONY\s*:", ".PHONY: clean", joined, count=1)
            # but that would drop others - better to append clean
            joined = "".join(new_lines)
            joined = re.sub(r"(?m)^(\.PHONY\s*:.*)$", lambda m: m.group(1).rstrip() + " clean", joined, count=1)
        else:
            joined = ".PHONY: clean\n" + joined
    return {relative: joined}


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
        Rule(
            "make-clean-target",
            "Require a root Makefile clean target that removes language-specific build artifacts",
            TagPredicate(),
            _make_clean_target_findings,
            _make_clean_target_fix,
            "chore(repomod): add make clean target",
            ("Makefile", "makefile", "GNUmakefile"),
        ),
    )
