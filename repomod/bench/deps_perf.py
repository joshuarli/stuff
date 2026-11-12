"""Measure the `make deps` dashboard workflow.

The workflow is `uv run repomod deps render --status`: load the tracked
inventory, collect live repository status, and write the dashboard. `srv`
startup is a separate binary and is not included.

    uv run python bench/deps_perf.py workflow --mode both --runs 3
    uv run python bench/deps_perf.py phases --mode both --runs 3

`warm` uses the real cache directory, as `make deps` does. `cold` points
XDG_CACHE_HOME at a scratch directory that is cleared before each run, so the
first run after the caches expire is measured without touching user data.

The `work_*` columns sum the durations of operations that run concurrently, so
they can exceed the wall-clock `status` time. The `load`, `visibility`, `status`,
and `render` columns partition the wall-clock `total`.
"""

from __future__ import annotations

import argparse
import functools
import json
import os
import shutil
import subprocess
import sys
import time
from collections.abc import Callable, Mapping, Sequence
from pathlib import Path
from typing import Any, ParamSpec, TypeVar

ROOT = Path(__file__).resolve().parents[1]
INVENTORY = ROOT / "data" / "dependencies" / "v1"
SCRATCH = ROOT / ".work" / "tmp" / "deps-perf"

_P = ParamSpec("_P")
_T = TypeVar("_T")

_WALL_PHASES = ("load", "visibility", "status", "render")
_WORK_PHASES = (
    "checkouts",
    "remote_metadata",
    "ci_status",
    "release_status",
    "upstream_status",
    "open_pull_requests",
    "git_extras",
)


def _modes(value: str) -> tuple[str, ...]:
    return ("cold", "warm") if value == "both" else (value,)


def _cache_dir(mode: str, run: int) -> Path | None:
    return None if mode == "warm" else SCRATCH / "cache" / f"{mode}-{run}"


def _output_dir(mode: str, run: int) -> Path:
    return SCRATCH / "dashboard" / f"{mode}-{run}"


def _prepare(mode: str, run: int) -> None:
    shutil.rmtree(_output_dir(mode, run), ignore_errors=True)
    cache = _cache_dir(mode, run)
    if cache is not None:
        shutil.rmtree(cache, ignore_errors=True)
        cache.mkdir(parents=True)


def _env(cache: Path | None) -> dict[str, str]:
    env = os.environ.copy()
    if cache is not None:
        env["XDG_CACHE_HOME"] = str(cache)
    return env


def _command(output: Path) -> list[str]:
    return [
        "uv",
        "run",
        "repomod",
        "deps",
        "render",
        "--status",
        "--output",
        str(output),
    ]


def _workflow(args: argparse.Namespace) -> int:
    records: list[dict[str, Any]] = []
    for mode in _modes(args.mode):
        for run in range(1, args.runs + 1):
            _prepare(mode, run)
            output = _output_dir(mode, run)
            started = time.perf_counter()
            result = subprocess.run(
                _command(output), env=_env(_cache_dir(mode, run)), capture_output=True, text=True
            )
            elapsed = time.perf_counter() - started
            if result.returncode != 0:
                sys.stderr.write(result.stderr)
                return result.returncode
            records.append(
                {
                    "mode": mode,
                    "run": run,
                    "total": round(elapsed, 3),
                    "returncode": result.returncode,
                    "stdout": result.stdout.strip(),
                }
            )
    _print(records, ("mode", "run", "total"), args.json)
    return 0


def _measure(args: argparse.Namespace) -> int:
    cache = _cache_dir(args.mode, args.run)
    if cache is not None:
        os.environ["XDG_CACHE_HOME"] = str(cache)

    from repomod import github
    from repomod.cmd import common
    from repomod.cmd import status as status_module
    from repomod.dependencies import graph
    from repomod.dependencies import render as render_module

    timings: dict[str, float] = {}
    wall: dict[str, float] = {}

    def timed(name: str, function: Callable[_P, _T]) -> Callable[_P, _T]:
        @functools.wraps(function)
        def wrapper(*args: _P.args, **kwargs: _P.kwargs) -> _T:
            started = time.perf_counter()
            try:
                return function(*args, **kwargs)
            finally:
                timings[name] = timings.get(name, 0.0) + time.perf_counter() - started

        return wrapper

    setattr(github, "remote_metadata", timed("remote_metadata", github.remote_metadata))
    setattr(github, "ci_status", timed("ci_status", github.ci_status))
    setattr(github, "release_status", timed("release_status", github.release_status))
    setattr(github, "upstream_status", timed("upstream_status", github.upstream_status))
    setattr(github, "open_pull_requests", timed("open_pull_requests", github.open_pull_requests))
    setattr(status_module, "_status_checkout", timed("checkouts", status_module._status_checkout))
    setattr(
        status_module,
        "oldest_unpushed_commit_time",
        timed("git_extras", status_module.oldest_unpushed_commit_time),
    )
    setattr(
        status_module,
        "branch_checked_out_time",
        timed("git_extras", status_module.branch_checked_out_time),
    )

    started = time.perf_counter()
    catalog = common.default_catalog()
    selected = common.select_repositories(catalog, [], [])
    loaded = graph.load_inventory(INVENTORY)
    graph.build_graph(loaded, catalog)
    wall["load"] = time.perf_counter() - started

    started = time.perf_counter()
    reported = common.reporting_repositories(catalog, [], [], include_private=False)
    wall["visibility"] = time.perf_counter() - started

    started = time.perf_counter()
    report = status_module.collect_status(catalog, tuple(reported), {}, offline=False, refresh=False)
    wall["status"] = time.perf_counter() - started

    started = time.perf_counter()
    status = {
        str(row["repository"]): status_module.status_items(row, report.ci[str(row["repository"])])
        for row in report.rows
    }
    hidden = frozenset(str(repository.id) for repository in selected) - frozenset(
        str(repository.id) for repository in reported
    )
    visible_inventory = graph.excluding(loaded, hidden)
    visible_graph = graph.build_graph(visible_inventory, catalog)
    result = render_module.render(
        visible_inventory,
        visible_graph,
        _output_dir(args.mode, args.run),
        root=ROOT,
        status=status,
        hidden=hidden,
    )
    wall["render"] = time.perf_counter() - started

    record: dict[str, Any] = {"mode": args.mode, "run": args.run, "total": round(sum(wall.values()), 3)}
    record.update({name: round(wall.get(name, 0.0), 3) for name in _WALL_PHASES})
    record.update({f"work_{name}": round(timings.get(name, 0.0), 3) for name in _WORK_PHASES})
    record.update({"pages": len(result.pages), "bytes": result.bytes})
    print(json.dumps(record))
    return 0


def _phases(args: argparse.Namespace) -> int:
    records: list[dict[str, Any]] = []
    for mode in _modes(args.mode):
        for run in range(1, args.runs + 1):
            _prepare(mode, run)
            result = subprocess.run(
                [
                    sys.executable,
                    str(Path(__file__).resolve()),
                    "measure",
                    "--mode",
                    mode,
                    "--run",
                    str(run),
                ],
                env=_env(_cache_dir(mode, run)),
                capture_output=True,
                text=True,
            )
            if result.returncode != 0:
                sys.stderr.write(result.stderr)
                return result.returncode
            records.append(json.loads(result.stdout))
    _print(
        records,
        ("mode", "run", "total", *_WALL_PHASES, *(f"work_{name}" for name in _WORK_PHASES)),
        args.json,
    )
    return 0


def _print(records: Sequence[Mapping[str, Any]], columns: Sequence[str], as_json: bool) -> None:
    if as_json:
        for record in records:
            print(json.dumps(record, sort_keys=True))
        return
    widths = {column: len(column) for column in columns}
    for record in records:
        for column in columns:
            widths[column] = max(widths[column], len(str(record.get(column, ""))))
    print("  ".join(column.ljust(widths[column]) for column in columns))
    for record in records:
        print("  ".join(str(record.get(column, "")).ljust(widths[column]) for column in columns))


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)

    def common_options(command: argparse.ArgumentParser) -> None:
        command.add_argument("--mode", choices=("warm", "cold", "both"), default="both")
        command.add_argument("--runs", type=int, default=3)
        command.add_argument("--json", action="store_true")

    workflow = commands.add_parser("workflow", help="time the real render command end to end")
    common_options(workflow)
    phases = commands.add_parser("phases", help="break the same pipeline into stages")
    common_options(phases)
    measure = commands.add_parser("measure", help="internal: one in-process run as JSON")
    measure.add_argument("--mode", choices=("warm", "cold"), required=True)
    measure.add_argument("--run", type=int, default=1)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    if args.command == "workflow":
        return _workflow(args)
    if args.command == "phases":
        return _phases(args)
    return _measure(args)


if __name__ == "__main__":
    raise SystemExit(main())
