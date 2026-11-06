from __future__ import annotations

import argparse
import sys
from collections.abc import Sequence
from pathlib import Path
from typing import TYPE_CHECKING

from repomod.cmd import common
from repomod.dependencies.model import RepositoryData, RepositoryState

if TYPE_CHECKING:
    from repomod.dependencies.enrich import EnrichReport
    from repomod.dependencies.graph import Graph, WhyPath, WhyResult


def _root() -> Path:
    return Path(__file__).resolve().parents[3]


def _inventory() -> Path:
    return _root() / "data" / "dependencies" / "v1"


def _display(path: Path) -> str:
    try:
        return path.relative_to(_root()).as_posix()
    except ValueError:
        return str(path)


def _deps(args: argparse.Namespace) -> int:
    from repomod.dependencies import analysis, graph
    from repomod.dependencies import diff as diff_module
    from repomod.dependencies import snapshot as snapshot_module

    inventory = _inventory()
    if args.operation == "diff":
        value = diff_module.diff_inventory(
            _root(),
            inventory,
            from_rev=args.from_rev or "HEAD",
            to_rev=args.to_rev,
        )
        if args.json:
            common.print_json(diff_module.diff_dict(value))
        else:
            for line in diff_module.diff_text(value):
                print(line)
        return 0
    catalog = common.default_catalog()
    repositories = common.select_repositories(
        catalog, getattr(args, "repo", []), getattr(args, "tag", [])
    )
    filtered = bool(getattr(args, "repo", []) or getattr(args, "tag", []))
    if args.operation == "snapshot":
        result = snapshot_module.snapshot(
            catalog,
            repositories,
            inventory=inventory,
            filtered=filtered,
            workers=args.workers,
        )
        _print_states(result.repositories)
        if result.written:
            print(f"wrote {len(result.written)} files")
        else:
            print("inventory is current")
        if result.deleted:
            print(f"deleted {len(result.deleted)} stale files")
        return _strict(result.repositories, args.strict)
    if args.operation == "check":
        result = snapshot_module.check(
            catalog,
            repositories,
            inventory=inventory,
            filtered=filtered,
        )
        for path in result.drifted:
            print(_display(path))
        if result.drifted:
            return 1
        return _strict(result.repositories, args.strict)
    loaded = graph.load_inventory(inventory)
    built = graph.build_graph(loaded, catalog)
    selected = [str(repository.id) for repository in repositories]
    if args.operation == "enrich":
        from repomod.dependencies import enrich as enrich_module

        result = enrich_module.enrich(
            built, selected, offline=args.offline, refresh=args.refresh
        )
        if args.json:
            common.print_json(_enrich_dict(result))
        else:
            print(enrich_module.summary_text(result))
        return 0
    if args.operation == "why":
        query = graph.parse_query(args.package)
        result = graph.why(built, query, repositories=selected, max_paths=args.max_paths)
        if args.json:
            common.print_json(_why_dict(built, result))
        else:
            _print_why(built, result)
        return 0 if result.paths else 1
    value = analysis.report(built, selected)
    if args.json:
        common.print_json(value)
    else:
        for line in analysis.report_text(value):
            print(line)
    return 0


def _enrich_dict(value: EnrichReport) -> dict[str, object]:
    return {
        "fetched": value.fetched,
        "checked": value.checked,
        "stale": value.stale,
        "not_checked": value.not_checked,
        "vulnerable": value.vulnerable,
        "total": value.total,
        "cache": str(value.cache.path),
        "fetched_at": value.cache.fetched_at,
        "results": value.cache.results,
    }


def _why_dict(built: Graph, result: WhyResult) -> dict[str, object]:
    return {
        "package": {
            "text": result.package.text,
            "ecosystem": (
                result.package.ecosystem.value
                if result.package.ecosystem is not None
                else None
            ),
            "name": result.package.name,
            "version": result.package.version,
        },
        "matched": list(result.matched),
        "paths": [
            {
                "project": path.project,
                "repository": path.repository,
                "target": path.target,
                "nodes": list(path.nodes),
                "labels": [built.label(node) for node in path.nodes],
                "edges": [
                    {
                        "from": edge.from_id,
                        "to": edge.to_id,
                        "scope": edge.scope.value,
                        "relation": edge.relation.value,
                        "constraint": edge.constraint,
                        "condition": edge.condition,
                        "resolution": edge.resolution.value,
                        "evidence": [
                            {"path": item.path, "locator": item.locator}
                            for item in edge.evidence
                        ],
                    }
                    for edge in path.edges
                ],
            }
            for path in result.paths
        ],
        "truncated": result.truncated,
    }


def _print_why(built: Graph, result: WhyResult) -> None:
    if not result.matched:
        print(f"no matching package: {result.package.text}")
        return
    print(
        f"{result.package.text}: {len(result.matched)} instance(s), "
        f"{len(result.paths)} path(s)"
    )
    groups: dict[str, dict[str, list[WhyPath]]] = {}
    for path in result.paths:
        groups.setdefault(path.repository, {}).setdefault(path.project, []).append(path)
    for repository in sorted(groups):
        print(repository)
        for project in sorted(groups[repository]):
            print(f"    {built.label(project)}")
            for path in sorted(
                groups[repository][project], key=lambda item: (len(item.nodes), item.nodes)
            ):
                labels = " -> ".join(built.label(node) for node in path.nodes)
                edge = path.edges[-1]
                constraint = (
                    f", constraint={edge.constraint}"
                    if edge.constraint is not None and len(edge.constraint) > 0
                    else ""
                )
                print(
                    f"        {labels} "
                    f"[{edge.scope.value}, {edge.resolution.value}{constraint}]"
                )
    if result.truncated:
        print("    truncated: path limit reached")


def _print_states(repositories: Sequence[RepositoryData]) -> None:
    counts = dict.fromkeys(RepositoryState, 0)
    for item in repositories:
        counts[item.state] += 1
    summary = ", ".join(f"{state.value} {count}" for state, count in counts.items())
    print(f"{len(repositories)} repositories: {summary}")


def _strict(repositories: Sequence[RepositoryData], strict: bool) -> int:
    if not strict:
        return 0
    failures = [
        item.repository
        for item in repositories
        if item.state in {RepositoryState.FAILED, RepositoryState.UNAVAILABLE}
    ]
    if not failures:
        return 0
    print(
        f"strict: {len(failures)} repositories are failed or unavailable",
        file=sys.stderr,
    )
    return 1
