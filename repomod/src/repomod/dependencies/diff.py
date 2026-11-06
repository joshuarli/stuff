from __future__ import annotations

import json
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

from packaging.version import InvalidVersion, Version

from repomod.dependencies.graph import Inventory, load_inventory, parse_index, parse_repository
from repomod.dependencies.model import (
    Ecosystem,
    Instance,
    Issue,
    RepositoryData,
    SourceKind,
)
from repomod.git import run_git

_INVENTORY_PREFIX = "data/dependencies/v1"

type Direction = Literal["upgrade", "downgrade", "changed", "added", "removed"]
type RepositoryStatus = Literal["added", "removed", "changed"]


@dataclass(frozen=True, slots=True)
class VersionChange:
    component: str
    name: str
    ecosystem: Ecosystem
    old: str | None
    new: str | None
    direction: Direction


@dataclass(frozen=True, slots=True)
class SourceChange:
    component: str
    version: str | None
    old: SourceKind
    new: SourceKind
    old_url: str | None
    new_url: str | None


@dataclass(frozen=True, slots=True)
class RepositoryDiff:
    repository: str
    status: RepositoryStatus
    old_state: str | None
    new_state: str | None
    old_commit: str | None
    new_commit: str | None
    added: tuple[Instance, ...]
    removed: tuple[Instance, ...]
    versions: tuple[VersionChange, ...]
    sources: tuple[SourceChange, ...]
    new_issues: tuple[Issue, ...]
    resolved_issues: tuple[Issue, ...]


@dataclass(frozen=True, slots=True)
class Diff:
    baseline: bool
    source: str
    target: str
    repositories: tuple[RepositoryDiff, ...]


def diff_inventory(
    root: Path,
    inventory: Path,
    *,
    from_rev: str = "HEAD",
    to_rev: str | None = None,
) -> Diff:
    _validate_revision(root, from_rev)
    if to_rev is not None:
        _validate_revision(root, to_rev)
    old = _revision_inventory(root, from_rev)
    if old is None:
        return Diff(False, from_rev, to_rev or "working tree", ())
    if to_rev is None:
        new = (
            load_inventory(inventory)
            if (inventory / "index.json").is_file()
            else Inventory(inventory, {}, {})
        )
    else:
        new = _revision_inventory(root, to_rev)
        if new is None:
            new = Inventory(inventory, {}, {})
    names = sorted(
        set(old.entries) | set(new.entries), key=str.casefold
    )
    changes = tuple(
        change
        for name in names
        if (change := _compare(name, old, new)) is not None
    )
    return Diff(True, from_rev, to_rev or "working tree", changes)


def _validate_revision(root: Path, revision: str) -> None:
    result = run_git(
        ["rev-parse", "--verify", f"{revision}^{{commit}}"], cwd=root, check=False
    )
    if result.returncode != 0:
        raise ValueError(f"unknown revision: {revision}")


def _show(root: Path, revision: str, relative: str) -> str | None:
    result = run_git(
        ["show", f"{revision}:{_INVENTORY_PREFIX}/{relative}"], cwd=root, check=False
    )
    if result.returncode != 0:
        return None
    return result.stdout


def _revision_inventory(root: Path, revision: str) -> Inventory | None:
    index_text = _show(root, revision, "index.json")
    if index_text is None:
        return None
    try:
        entries = parse_index(json.loads(index_text))
    except (json.JSONDecodeError, ValueError) as error:
        raise ValueError(f"cannot read {revision}:{_INVENTORY_PREFIX}/index.json: {error}") from error
    repositories: dict[str, RepositoryData] = {}
    for name in entries:
        text = _show(root, revision, f"repos/{name}.json")
        if text is None:
            continue
        try:
            repositories[name] = parse_repository(json.loads(text))
        except (json.JSONDecodeError, ValueError) as error:
            raise ValueError(
                f"cannot read {revision}:{_INVENTORY_PREFIX}/repos/{name}.json: {error}"
            ) from error
    return Inventory(root / _INVENTORY_PREFIX, repositories, entries)


def _compare(name: str, old: Inventory, new: Inventory) -> RepositoryDiff | None:
    old_entry = old.entries.get(name)
    new_entry = new.entries.get(name)
    if old_entry is None and new_entry is None:
        return None
    if old_entry is None:
        data = new.repositories.get(name)
        return RepositoryDiff(
            name,
            "added",
            None,
            new_entry.state.value if new_entry is not None else None,
            None,
            new_entry.commit if new_entry is not None else None,
            tuple(data.instances) if data is not None else (),
            (),
            (),
            (),
            tuple(data.issues) if data is not None else (),
            (),
        )
    if new_entry is None:
        data = old.repositories.get(name)
        return RepositoryDiff(
            name,
            "removed",
            old_entry.state.value,
            None,
            old_entry.commit,
            None,
            (),
            tuple(data.instances) if data is not None else (),
            (),
            (),
            (),
            tuple(data.issues) if data is not None else (),
        )
    old_data = old.repositories.get(name)
    new_data = new.repositories.get(name)
    if old_data is None and new_data is None:
        return None
    old_data = old_data or _empty(name)
    new_data = new_data or _empty(name)
    old_by_id = {item.id: item for item in old_data.instances}
    new_by_id = {item.id: item for item in new_data.instances}
    added = tuple(new_by_id[key] for key in sorted(new_by_id.keys() - old_by_id.keys()))
    removed = tuple(old_by_id[key] for key in sorted(old_by_id.keys() - new_by_id.keys()))
    versions = _version_changes(old_data, new_data)
    sources = _source_changes(old_data, new_data)
    new_issues, resolved_issues = _issue_changes(old_data, new_data)
    state_changed = old_entry.state is not new_entry.state
    commit_changed = old_entry.commit != new_entry.commit
    if not (
        added
        or removed
        or versions
        or sources
        or new_issues
        or resolved_issues
        or state_changed
        or commit_changed
    ):
        return None
    return RepositoryDiff(
        name,
        "changed",
        old_entry.state.value,
        new_entry.state.value,
        old_entry.commit,
        new_entry.commit,
        added,
        removed,
        versions,
        sources,
        new_issues,
        resolved_issues,
    )


def _empty(name: str) -> RepositoryData:
    return RepositoryData(name, (), (), "")


def _version_changes(old: RepositoryData, new: RepositoryData) -> tuple[VersionChange, ...]:
    old_components = {component.purl: component for component in old.components}
    new_components = {component.purl: component for component in new.components}
    old_versions: dict[str, set[str]] = {}
    new_versions: dict[str, set[str]] = {}
    for instance in old.instances:
        if instance.version is not None:
            old_versions.setdefault(instance.component, set()).add(instance.version)
    for instance in new.instances:
        if instance.version is not None:
            new_versions.setdefault(instance.component, set()).add(instance.version)
    changes: list[VersionChange] = []
    for component in sorted(set(old_versions) | set(new_versions)):
        before = old_versions.get(component, set())
        after = new_versions.get(component, set())
        added = sorted(after - before)
        removed = sorted(before - after)
        record = old_components.get(component) or new_components.get(component)
        if record is None:
            continue
        if len(added) == 1 and len(removed) == 1:
            changes.append(
                VersionChange(
                    component,
                    record.name,
                    record.ecosystem,
                    removed[0],
                    added[0],
                    _direction(removed[0], added[0], record.ecosystem),
                )
            )
            continue
        changes.extend(
            VersionChange(component, record.name, record.ecosystem, None, version, "added")
            for version in added
        )
        changes.extend(
            VersionChange(component, record.name, record.ecosystem, version, None, "removed")
            for version in removed
        )
    return tuple(changes)


def _direction(old: str, new: str, ecosystem: Ecosystem) -> Direction:
    if ecosystem is Ecosystem.PYPI:
        try:
            old_version = Version(old)
            new_version = Version(new)
        except InvalidVersion:
            return "changed"
        if old_version == new_version:
            return "changed"
        return "upgrade" if new_version > old_version else "downgrade"
    old_key = _numeric_key(old)
    new_key = _numeric_key(new)
    if old_key is None or new_key is None or old_key == new_key:
        return "changed"
    return "upgrade" if new_key > old_key else "downgrade"


def _numeric_key(value: str) -> tuple[int, ...] | None:
    parts: list[int] = []
    stripped = value.strip().removeprefix("v").removeprefix("V")
    for piece in stripped.split("."):
        digits = ""
        for character in piece:
            if not character.isdigit():
                break
            digits += character
        if not digits:
            break
        parts.append(int(digits))
    return tuple(parts) if parts else None


def _source_changes(old: RepositoryData, new: RepositoryData) -> tuple[SourceChange, ...]:
    old_components = {component.purl: component for component in old.components}
    new_components = {component.purl: component for component in new.components}
    old_instances = _by_version(old.instances)
    new_instances = _by_version(new.instances)
    changes: list[SourceChange] = []
    for key in sorted(
        set(old_instances) & set(new_instances),
        key=lambda item: (item[0], item[1] or ""),
    ):
        component, _version = key
        before = old_instances[key]
        after = new_instances[key]
        if _source_key(before) == _source_key(after):
            continue
        record = old_components.get(component) or new_components.get(component)
        if record is None:
            continue
        changes.append(
            SourceChange(
                component,
                before.version,
                before.source_kind,
                after.source_kind,
                before.registry_url or before.vcs_url or before.repository_url,
                after.registry_url or after.vcs_url or after.repository_url,
            )
        )
    return tuple(changes)


def _by_version(instances: Sequence[Instance]) -> dict[tuple[str, str | None], Instance]:
    result: dict[tuple[str, str | None], Instance] = {}
    for instance in sorted(instances, key=lambda item: item.id):
        result.setdefault((instance.component, instance.version), instance)
    return result


def _source_key(instance: Instance) -> tuple[SourceKind, str | None, str | None, str | None, str | None]:
    return (
        instance.source_kind,
        instance.registry_url,
        instance.repository_url,
        instance.vcs_url,
        instance.commit,
    )


def _issue_changes(
    old: RepositoryData, new: RepositoryData
) -> tuple[tuple[Issue, ...], tuple[Issue, ...]]:
    old_issues = {_issue_key(issue): issue for issue in old.issues}
    new_issues = {_issue_key(issue): issue for issue in new.issues}
    added = tuple(new_issues[key] for key in sorted(new_issues.keys() - old_issues.keys()))
    removed = tuple(old_issues[key] for key in sorted(old_issues.keys() - new_issues.keys()))
    return added, removed


def _issue_key(issue: Issue) -> tuple[str, str, str, str]:
    return (issue.kind.value, issue.path or "", issue.locator or "", issue.message)


def diff_dict(
    value: Diff, inventory: str = "data/dependencies/v1"
) -> dict[str, object]:
    return {
        "baseline": value.baseline,
        "from": value.source,
        "to": value.target,
        "inventory": inventory,
        "summary": _summary(value),
        "repositories": [
            {
                "repository": change.repository,
                "status": change.status,
                "state": {"old": change.old_state, "new": change.new_state},
                "commit": {"old": change.old_commit, "new": change.new_commit},
                "instances": {
                    "added": [_instance_dict(item) for item in change.added],
                    "removed": [_instance_dict(item) for item in change.removed],
                },
                "versions": [
                    {
                        "component": item.component,
                        "name": item.name,
                        "ecosystem": item.ecosystem.value,
                        "old": item.old,
                        "new": item.new,
                        "direction": item.direction,
                    }
                    for item in change.versions
                ],
                "sources": [
                    {
                        "component": item.component,
                        "version": item.version,
                        "old": item.old.value,
                        "new": item.new.value,
                        "old_url": item.old_url,
                        "new_url": item.new_url,
                    }
                    for item in change.sources
                ],
                "issues": {
                    "new": [_issue_dict(item) for item in change.new_issues],
                    "resolved": [_issue_dict(item) for item in change.resolved_issues],
                },
            }
            for change in value.repositories
        ],
    }


def _summary(value: Diff) -> dict[str, object]:
    upgraded = sum(
        1 for change in value.repositories for item in change.versions if item.direction == "upgrade"
    )
    downgraded = sum(
        1
        for change in value.repositories
        for item in change.versions
        if item.direction == "downgrade"
    )
    changed = sum(
        1 for change in value.repositories for item in change.versions if item.direction == "changed"
    )
    added_versions = sum(
        1 for change in value.repositories for item in change.versions if item.direction == "added"
    )
    removed_versions = sum(
        1 for change in value.repositories for item in change.versions if item.direction == "removed"
    )
    return {
        "repositories": _metric(len(value.repositories), "repositories", "selected repositories"),
        "instances_added": _metric(
            sum(len(change.added) for change in value.repositories),
            "instances",
            "all instances",
        ),
        "instances_removed": _metric(
            sum(len(change.removed) for change in value.repositories),
            "instances",
            "all instances",
        ),
        "versions_upgraded": _metric(upgraded, "version changes", "all version changes"),
        "versions_downgraded": _metric(downgraded, "version changes", "all version changes"),
        "versions_changed": _metric(changed, "version changes", "all version changes"),
        "versions_added": _metric(added_versions, "version changes", "all version changes"),
        "versions_removed": _metric(removed_versions, "version changes", "all version changes"),
        "sources_changed": _metric(
            sum(len(change.sources) for change in value.repositories),
            "sources",
            "all instances",
        ),
        "issues_new": _metric(
            sum(len(change.new_issues) for change in value.repositories),
            "issues",
            "all coverage issues",
        ),
        "issues_resolved": _metric(
            sum(len(change.resolved_issues) for change in value.repositories),
            "issues",
            "all coverage issues",
        ),
    }


def _metric(value: int, unit: str, denominator: str) -> dict[str, object]:
    return {"value": value, "unit": unit, "denominator": denominator}


def _instance_dict(instance: Instance) -> dict[str, object]:
    return {
        "id": instance.id,
        "purl": instance.purl,
        "component": instance.component,
        "version": instance.version,
        "source_kind": instance.source_kind.value,
    }


def _issue_dict(issue: Issue) -> dict[str, object]:
    return {
        "kind": issue.kind.value,
        "path": issue.path,
        "locator": issue.locator,
        "message": issue.message,
    }


def diff_text(value: Diff) -> list[str]:
    if not value.baseline:
        return ["no baseline"]
    lines: list[str] = []
    for change in value.repositories:
        header = f"{change.repository} ({change.status})"
        if change.status == "changed":
            header += f": {change.old_commit or '?'} -> {change.new_commit or '?'}"
            if change.old_state != change.new_state:
                header += f", {change.old_state} -> {change.new_state}"
        lines.append(header)
        lines.extend(f"    + {instance.purl}" for instance in change.added)
        lines.extend(f"    - {instance.purl}" for instance in change.removed)
        for item in change.versions:
            if item.direction in {"upgrade", "downgrade", "changed"}:
                lines.append(
                    f"    ~ {item.name}: {item.old} -> {item.new} ({item.direction})"
                )
            elif item.direction == "added":
                lines.append(f"    + {item.name}@{item.new}")
            else:
                lines.append(f"    - {item.name}@{item.old}")
        lines.extend(
            f"    source ~ {item.component} {item.version or ''}: "
            f"{item.old.value} -> {item.new.value}"
            for item in change.sources
        )
        lines.extend(
            f"    + issue {issue.kind.value} {issue.path or ''}".rstrip()
            for issue in change.new_issues
        )
        lines.extend(
            f"    - issue {issue.kind.value} {issue.path or ''}".rstrip()
            for issue in change.resolved_issues
        )
    if not lines:
        lines.append("no changes")
    return lines
