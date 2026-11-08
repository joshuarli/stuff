from __future__ import annotations

import argparse
import sys
from collections.abc import Sequence
from pathlib import Path

from repomod.catalog import CatalogError
from repomod.cmd.deps import _deps
from repomod.cmd.discover import _discover
from repomod.cmd.fix import _fix
from repomod.cmd.gc import _gc
from repomod.cmd.lint import _lint
from repomod.cmd.merge import _merge
from repomod.cmd.musl import _musl
from repomod.cmd.pins import _pins
from repomod.cmd.push import _push
from repomod.cmd.status import _status
from repomod.cmd.sync import _sync
from repomod.cmd.topics import _topics
from repomod.git import GitError
from repomod.github import GitHubError
from repomod.policy import PolicyError


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="repomod", description="Repair policy violations and open pull requests")
    commands = parser.add_subparsers(dest="command", required=True)

    status_parser = commands.add_parser(
        "status", help="show CI, release, local, upstream, and pull request status"
    )
    _filters(status_parser)
    _checkout_option(status_parser)
    status_parser.add_argument("--offline", action="store_true")
    status_parser.add_argument("--dirty", action="store_true")
    status_parser.add_argument("--failed", action="store_true", help="only failures")
    status_parser.add_argument("--refresh", action="store_true")
    status_parser.add_argument(
        "--details", action="store_true", help="show failed CI checks"
    )
    status_parser.add_argument("--json", action="store_true")

    lint = commands.add_parser("lint", help="check declarative repository policies")
    _filters(lint)
    _checkout_option(lint)
    lint.add_argument("--rule", action="append", default=[])
    lint.add_argument("--workers", type=int, default=16)
    lint.add_argument("--json", action="store_true")
    lint.add_argument("--strict", action="store_true", help="treat warnings as violations")

    fix = commands.add_parser(
        "fix", help="sync selected checkouts, repair policy violations, and open pull requests"
    )
    _filters(fix)
    fix.add_argument("--rule", action="append", default=[])
    fix.add_argument("--workers", type=int, default=4)

    sync = commands.add_parser("sync", help="clone or safely fast-forward catalogued checkouts")
    sync.add_argument("--dry-run", action="store_true")
    sync.add_argument("--workers", type=int, default=4)

    pins = commands.add_parser("pins", help="refresh approved action, tool, and toolchain pins")
    pin_commands = pins.add_subparsers(dest="operation", required=True)
    refresh = pin_commands.add_parser("refresh", help="refresh pin files from their upstream sources")
    refresh.add_argument("--check", action="store_true", help="report drift without writing")
    refresh.add_argument("--only", choices=("actions", "python", "typescript", "musl"))

    musl = commands.add_parser("musl", help="build the canonical musl toolchain image")
    musl_commands = musl.add_subparsers(dest="operation", required=True)
    build = musl_commands.add_parser("build", help="build a local OCI layout")
    build.add_argument("--platform", required=True, choices=("linux/amd64", "linux/arm64"))
    build.add_argument("--output", type=Path, default=Path(".work/oci/musl"), metavar="PATH")

    push = commands.add_parser(
        "push", help="push checked-out branches that are ahead of their origin upstream"
    )
    _filters(push)
    push.add_argument("--workers", type=int, default=4)

    merge = commands.add_parser(
        "merge", help="merge open pull requests from repomod fix branches"
    )
    _filters(merge)
    merge.add_argument("--dry-run", action="store_true")
    merge.add_argument("--workers", type=int, default=4)

    gc = commands.add_parser(
        "gc", help="remove checkouts the catalog no longer references"
    )
    gc.add_argument("--dry-run", action="store_true")
    gc.add_argument("--force", action="store_true")
    gc.add_argument("--workers", type=int, default=4)

    discover = commands.add_parser(
        "discover", help="compare GitHub owner repositories against catalogued entries"
    )
    discover.add_argument("--owner", action="append", default=[])
    discover.add_argument("--json", action="store_true")

    deps = commands.add_parser(
        "deps", help="inventory the dependency inputs of catalogued repositories"
    )
    deps_commands = deps.add_subparsers(dest="operation", required=True)
    snapshot = deps_commands.add_parser(
        "snapshot",
        help="read catalogued checkouts and write data/dependencies/v1",
        description=(
            "read the origin/HEAD tree of each selected checkout and write "
            "data/dependencies/v1"
        ),
    )
    _filters(snapshot)
    snapshot.add_argument("--workers", type=int, default=4)
    snapshot.add_argument(
        "--strict",
        action="store_true",
        help="exit 1 when a selected repository is failed or unavailable",
    )
    check = deps_commands.add_parser(
        "check",
        help="report inventory drift without writing",
        description=(
            "regenerate the inventory in memory and list files that differ from "
            "data/dependencies/v1"
        ),
    )
    _filters(check)
    check.add_argument(
        "--strict",
        action="store_true",
        help="exit 1 when a selected repository is failed or unavailable",
    )
    why = deps_commands.add_parser(
        "why",
        help="explain why a package is in the inventory",
        description=(
            "read the inventory and print every simple path from each selected "
            "project to the matching package instances"
        ),
    )
    _filters(why)
    why.add_argument("package", help="name, ecosystem:name, or either with @version")
    why.add_argument("--max-paths", type=int, default=20)
    why.add_argument("--json", action="store_true")
    report = deps_commands.add_parser(
        "report",
        help="summarize the dependency inventory",
        description=(
            "read the inventory and print composition, concentration, divergence, "
            "source risk, scope, first-party, and freshness analyses"
        ),
    )
    _filters(report)
    report.add_argument("--json", action="store_true")
    diff = deps_commands.add_parser(
        "diff",
        help="compare the inventory across Git revisions",
        description=(
            "read data/dependencies/v1 at two revisions and list added, removed, "
            "and changed repositories, instances, versions, sources, and issues"
        ),
    )
    diff.add_argument("--from", dest="from_rev", metavar="REV")
    diff.add_argument("--to", dest="to_rev", metavar="REV")
    diff.add_argument("--json", action="store_true")
    enrich = deps_commands.add_parser(
        "enrich",
        help="fetch latest versions and vulnerabilities into the local cache",
        description=(
            "read the inventory and query registries and OSV for resolved Cargo, "
            "Python, npm, and Go components; write ~/.cache/repomod/deps-enrichment.json"
        ),
    )
    enrich.add_argument("--refresh", action="store_true")
    enrich.add_argument("--offline", action="store_true")
    enrich.add_argument("--json", action="store_true")
    render = deps_commands.add_parser(
        "render",
        help="write the static dashboard under .work/dependencies/dashboard",
        description=(
            "read the inventory and enrichment cache and write a static dashboard "
            "under .work/dependencies/dashboard"
        ),
    )
    render.add_argument(
        "--output",
        help="output directory (default .work/dependencies/dashboard)",
    )
    render.add_argument(
        "--status",
        action="store_true",
        help="also read live CI, release, upstream, pull request, and checkout status",
    )

    topics = commands.add_parser("topics", help="compare or replace GitHub repository topics")
    topic_commands = topics.add_subparsers(dest="operation", required=True)
    for operation in ("diff", "apply"):
        topic = topic_commands.add_parser(operation)
        _filters(topic)
        topic.add_argument("--workers", type=int, default=4)
    return parser


def _filters(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--repo", action="append", default=[])
    parser.add_argument("--tag", action="append", default=[])


def _checkout_option(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--checkout",
        type=Path,
        metavar="PATH",
        help="evaluate this Git work tree instead of the canonical checkout (requires exactly one --repo)",
    )


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        dispatch = {
            "status": _status,
            "lint": _lint,
            "fix": _fix,
            "sync": _sync,
            "pins": _pins,
            "musl": _musl,
            "push": _push,
            "merge": _merge,
            "gc": _gc,
            "discover": _discover,
            "deps": _deps,
            "topics": _topics,
        }
        return dispatch[args.command](args)
    except (CatalogError, PolicyError, GitError, GitHubError, OSError, ValueError) as error:
        print(f"repomod: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
