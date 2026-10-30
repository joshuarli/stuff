from __future__ import annotations

import json
import os
import sys
from collections.abc import Sequence
from pathlib import Path

from repomod.catalog import TAGS, CatalogError, find_repository, load_catalog
from repomod.git import validated_checkout
from repomod.policy import filter_repositories
from repomod.types import Catalog, Finding, Repository


def _catalog_path() -> Path:
    return Path(__file__).resolve().parents[3] / "catalog.toml"


def _catalog() -> Catalog:
    return load_catalog(_catalog_path())


def _tags(values: Sequence[str]) -> tuple[str, ...]:
    unknown = set(values) - TAGS
    if unknown:
        raise CatalogError(f"unknown tags: {', '.join(sorted(unknown))}")
    if len(values) != len(set(values)):
        raise CatalogError("duplicate tags are not allowed")
    return tuple(values)


def _select(
    catalog: Catalog, repo_values: Sequence[str], tags: Sequence[str]
) -> tuple[Repository, ...]:
    selected_tags = _tags(tags)
    if repo_values:
        repositories = tuple(find_repository(catalog, value) for value in repo_values)
        return filter_repositories(repositories, tags=selected_tags)
    return filter_repositories(catalog.repositories, tags=selected_tags)


def _checkouts(
    checkout: Path | None, repo_values: Sequence[str], repositories: Sequence[Repository]
) -> dict[str, Path]:
    if checkout is None:
        return {}
    if len(repo_values) != 1 or len(repositories) != 1:
        raise CatalogError("--checkout requires exactly one --repo that selects one repository")
    repository = repositories[0]
    return {str(repository.id): validated_checkout(checkout, repository.id)}


def _print_json(value: object) -> None:
    print(json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")))


def _paint(value: str, color: str) -> str:
    if (
        not sys.stdout.isatty()
        or os.environ.get("TERM") == "dumb"
        or "NO_COLOR" in os.environ
        or os.environ.get("CLICOLOR") == "0"
    ):
        return value
    return f"\033[{color}m{value}\033[0m"


def _finding_color(finding: Finding) -> str:
    if finding.kind == "operational" or finding.severity == "error":
        return "31"
    if finding.severity == "warning":
        return "33"
    return "36"


def _print_repository_block(
    name: str, stdout_lines: Sequence[str], stderr_lines: Sequence[str]
) -> None:
    if not stdout_lines and not stderr_lines:
        return
    header = _paint(name, "36")
    if stdout_lines:
        print(header)
    else:
        print(header, file=sys.stderr)
    for line in stdout_lines:
        print(f"    {line}")
    for line in stderr_lines:
        print(f"    {line}", file=sys.stderr)
