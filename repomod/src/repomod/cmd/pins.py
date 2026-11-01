from __future__ import annotations

import argparse
import os
import shutil
import sys
import tempfile
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

from repomod.catalog import atomic_write
from repomod.rules import actions, musl_toolchain, python_standard, typescript_standard

_DOMAINS = ("actions", "python", "typescript", "musl")


@dataclass(frozen=True, slots=True)
class _Plan:
    target: Path
    content: str | None
    observations: tuple[str, ...] = ()


def _targets() -> dict[str, Path]:
    return {
        "actions": actions.actions_file(),
        "python": python_standard.python_tools_file(),
        "typescript": typescript_standard.typescript_tools_file(),
        "musl": musl_toolchain.musl_toolchain_file(),
    }


def _refreshers() -> dict[str, Callable[[Path], bool]]:
    return {
        "actions": actions.refresh_pins,
        "python": python_standard.refresh_python_tool_versions,
        "typescript": typescript_standard.refresh_typescript_tool_pins,
    }


def _validators() -> dict[str, Callable[[Path], object]]:
    return {
        "actions": actions.load_pins,
        "python": python_standard.load_approved_python_tool_versions,
        "typescript": typescript_standard.load_typescript_tool_pins,
    }


def _scratch_directory() -> Path:
    configured = os.environ.get("TMPDIR")
    root = (
        Path(configured)
        if configured is not None
        else Path(__file__).resolve().parents[3] / ".work" / "tmp"
    )
    root.mkdir(parents=True, exist_ok=True)
    if root.is_symlink() or not root.is_dir():
        raise ValueError(f"pins scratch path is not a regular directory: {root}")
    return root


def _file_plan(name: str, target: Path, scratch: Path) -> _Plan:
    if target.is_symlink():
        raise ValueError(f"{target} is a symlink and is not refreshed")
    copy = scratch / name / target.name
    copy.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(target, copy)
    _refreshers()[name](copy)
    _validators()[name](copy)
    content = copy.read_text(encoding="utf-8")
    if content == target.read_text(encoding="utf-8"):
        content = None
    return _Plan(target, content)


def _musl_plan(target: Path) -> _Plan:
    target, source, observations = musl_toolchain.musl_toolchain_refresh_plan(target)
    return _Plan(target, source, observations)


def _plan(name: str, target: Path, scratch: Path) -> _Plan:
    if name == "musl":
        return _musl_plan(target)
    return _file_plan(name, target, scratch)


def _pins(args: argparse.Namespace) -> int:
    if getattr(args, "operation", None) != "refresh":
        raise ValueError("pins requires a refresh operation")
    names = (args.only,) if args.only else _DOMAINS
    targets = _targets()
    with tempfile.TemporaryDirectory(prefix="pins-", dir=_scratch_directory()) as directory:
        plans: list[tuple[str, _Plan]] = []
        for name in names:
            try:
                plans.append((name, _plan(name, targets[name], Path(directory))))
            except (OSError, UnicodeError, ValueError, RuntimeError) as error:
                print(f"repomod: cannot refresh {name} pins: {error}", file=sys.stderr)
                return 2
    for _, plan in plans:
        for observation in plan.observations:
            print(observation, file=sys.stderr)
    if args.check:
        changed = [(name, plan) for name, plan in plans if plan.content is not None]
        for _, plan in changed:
            print(f"{plan.target}: would change")
        return 1 if changed else 0
    for _, plan in plans:
        if plan.content is None:
            continue
        atomic_write(plan.target, plan.content)
        print(f"{plan.target}: updated")
    return 0
