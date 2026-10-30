from __future__ import annotations

import argparse
import sys
from collections.abc import Sequence

from repomod.catalog import CatalogError
from repomod.cmd.discover import _discover
from repomod.cmd.fix import _fix
from repomod.cmd.gc import _gc
from repomod.cmd.lint import _lint
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

    status_parser = commands.add_parser("status", help="show local Git, remote CI, TODO, and lint status")
    _filters(status_parser)
    status_parser.add_argument("--offline", action="store_true")
    status_parser.add_argument("--dirty", action="store_true")
    status_parser.add_argument("--failed", action="store_true")
    status_parser.add_argument("--refresh", action="store_true")
    status_parser.add_argument(
        "--details", action="store_true", help="show failed CI checks"
    )
    status_parser.add_argument("--json", action="store_true")

    lint = commands.add_parser("lint", help="check declarative repository policies")
    _filters(lint)
    lint.add_argument("--rule", action="append", default=[])
    lint.add_argument("--json", action="store_true")
    lint.add_argument("--strict", action="store_true", help="treat warnings as violations")

    fix = commands.add_parser("fix", help="repair policy violations and open pull requests")
    _filters(fix)
    fix.add_argument("--rule", action="append", default=[])
    fix.add_argument("--workers", type=int, default=4)

    sync = commands.add_parser("sync", help="clone or safely fast-forward catalogued checkouts")
    sync.add_argument("--dry-run", action="store_true")
    sync.add_argument("--workers", type=int, default=4)

    push = commands.add_parser(
        "push", help="push checked-out branches that are ahead of their origin upstream"
    )
    _filters(push)
    push.add_argument("--workers", type=int, default=4)

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


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        dispatch = {
            "status": _status,
            "lint": _lint,
            "fix": _fix,
            "sync": _sync,
            "push": _push,
            "gc": _gc,
            "discover": _discover,
            "topics": _topics,
        }
        return dispatch[args.command](args)
    except (CatalogError, PolicyError, GitError, GitHubError, OSError, ValueError) as error:
        print(f"repomod: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
