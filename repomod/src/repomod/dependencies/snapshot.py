from __future__ import annotations

import hashlib
import json
from collections.abc import Mapping, Sequence
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from functools import partial
from pathlib import Path
from typing import cast

from repomod.catalog import Catalog, atomic_write, repository_path
from repomod.dependencies.adapters import ADAPTERS
from repomod.dependencies.discover import discover
from repomod.dependencies.git_tree import default_commit, read_blobs, tree_entries
from repomod.dependencies.model import (
    SCHEMA_VERSION,
    Adapter,
    Component,
    CoverageKind,
    Edge,
    IndexEntry,
    InputContent,
    InputFile,
    Instance,
    Issue,
    Project,
    RepositoryData,
    redact_url,
    redact_urls,
)
from repomod.git import GitError, expected_origin, is_git_checkout, origin_url
from repomod.types import Repository

PARSER_VERSION = 4


@dataclass(frozen=True, slots=True)
class RepositoryInspection:
    data: RepositoryData
    commit: str | None


@dataclass(frozen=True, slots=True)
class Snapshot:
    inventory: Path
    repositories: tuple[RepositoryData, ...]
    entries: Mapping[str, IndexEntry]
    written: tuple[Path, ...]
    deleted: tuple[Path, ...]


@dataclass(frozen=True, slots=True)
class Check:
    repositories: tuple[RepositoryData, ...]
    entries: Mapping[str, IndexEntry]
    drifted: tuple[Path, ...]


def fingerprint(inputs: Sequence[InputFile]) -> str:
    ordered = sorted(inputs, key=lambda item: (item.path, item.blob))
    payload = json.dumps(
        {"parser_version": PARSER_VERSION, "inputs": [[item.path, item.blob] for item in ordered]},
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
    )
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def inspect_repository(catalog: Catalog, repository: Repository) -> RepositoryInspection:
    try:
        path = repository_path(catalog, repository)
    except (OSError, ValueError) as error:
        return _unavailable(repository, CoverageKind.CHECKOUT_MISSING, f"cannot resolve the checkout path: {error}")
    if path.is_symlink() or not path.is_dir():
        return _unavailable(repository, CoverageKind.CHECKOUT_MISSING, "no checkout at the catalogued path")
    try:
        is_checkout = is_git_checkout(path)
        remote = origin_url(path)
    except GitError as error:
        return _failed(repository, str(error))
    if not is_checkout:
        return _unavailable(repository, CoverageKind.CHECKOUT_MISSING, "the catalogued path is not a Git work tree")
    if remote is None or not expected_origin(repository.id, remote):
        return _unavailable(repository, CoverageKind.ORIGIN_MISMATCH, "origin does not match the catalogued repository")
    try:
        commit = default_commit(path)
    except GitError as error:
        return _unavailable(repository, CoverageKind.DEFAULT_REF_MISSING, str(error))
    try:
        entries = tree_entries(path, commit)
    except GitError as error:
        return _failed(repository, str(error), commit)

    discovery = discover(entries)
    discovered = {item.path: item for item in discovery.inputs}
    by_path = {entry.path: entry for entry in entries}
    issues: list[Issue] = [
        Issue(CoverageKind.SUBMODULE, "gitlink content is not inspected", path=entry.path)
        for entry in entries
        if entry.kind == "gitlink"
    ]
    issues.extend(
        Issue(CoverageKind.VENDORED_UNKNOWN, "vendored tree has no recognized manifest", path=root)
        for root in discovery.vendored_unknown
    )

    candidate_paths = {item.path for item in (*discovery.inputs, *discovery.symlinks)}
    for entry in entries:
        if entry.kind != "gitlink" and _adapter_for(entry.path) is not None:
            candidate_paths.add(entry.path)

    inputs: list[InputFile] = []
    for candidate in sorted(candidate_paths):
        entry = by_path[candidate]
        inputs.append(InputFile(candidate, entry.object_id))
        if entry.kind == "symlink":
            issues.append(
                Issue(CoverageKind.SYMLINK_SKIPPED, "dependency input is a symlink", path=candidate)
            )

    try:
        blobs = read_blobs(
            path,
            [by_path[item.path].object_id for item in inputs if by_path[item.path].kind == "blob"],
        )
    except GitError as error:
        issues.append(Issue(CoverageKind.GIT_FAILURE, str(error)))
        return RepositoryInspection(_repository_data(repository, inputs, issues), commit)
    contents: list[InputContent] = []
    gitlinks = tuple(
        sorted((entry.path, entry.object_id) for entry in entries if entry.kind == "gitlink")
    )
    for item in inputs:
        entry = by_path[item.path]
        if entry.kind != "blob":
            continue
        try:
            text = blobs[entry.object_id].decode("utf-8")
        except UnicodeDecodeError:
            issues.append(Issue(CoverageKind.NON_UTF8, "dependency input is not UTF-8", path=item.path))
            continue
        contents.append(
            InputContent(
                item.path,
                text,
                vendored=_is_vendored(item.path, discovery.vendored_roots),
                gitlinks=gitlinks,
            )
        )

    groups: list[tuple[Adapter, list[InputContent]]] = []
    for content in contents:
        adapter = _adapter_for(content.path)
        if adapter is None:
            found = discovered.get(content.path)
            label = found.label if found is not None else content.path
            issues.append(Issue(CoverageKind.UNSUPPORTED_FORMAT, f"no parser for {label}", path=content.path))
            continue
        for owner, items in groups:
            if owner is adapter:
                items.append(content)
                break
        else:
            groups.append((adapter, [content]))

    projects: list[Project] = []
    components: list[Component] = []
    instances: list[Instance] = []
    edges: list[Edge] = []
    for adapter, items in groups:
        try:
            result = adapter.extract(tuple(items))
        except ValueError as error:
            issues.append(Issue(CoverageKind.PARSE_ERROR, redact_urls(f"{adapter.name}: {error}")))
            continue
        projects.extend(result.projects)
        components.extend(result.components)
        instances.extend(result.instances)
        edges.extend(result.edges)
        issues.extend(result.issues)

    return RepositoryInspection(
        _repository_data(repository, inputs, issues, projects, components, instances, edges),
        commit,
    )


def snapshot(
    catalog: Catalog,
    repositories: Sequence[Repository],
    *,
    inventory: Path,
    filtered: bool,
    workers: int = 4,
) -> Snapshot:
    inspections = _inspect_all(catalog, repositories, workers)
    data = tuple(inspection.data for inspection in inspections)
    entries = {
        inspection.data.repository: IndexEntry(
            commit=inspection.commit,
            inputs_fingerprint=inspection.data.inputs_fingerprint,
            state=inspection.data.state,
        )
        for inspection in inspections
    }
    written: list[Path] = []
    for item in data:
        path = repository_file(inventory, item.repository)
        if _write_if_changed(path, serialize(repository_dict(item))):
            written.append(path)
    index_path = inventory / "index.json"
    if _write_if_changed(index_path, serialize(_index_payload(inventory, entries, filtered))):
        written.append(index_path)
    deleted = () if filtered else _delete_unselected(inventory, set(entries))
    return Snapshot(inventory, data, entries, tuple(written), deleted)


def check(
    catalog: Catalog,
    repositories: Sequence[Repository],
    *,
    inventory: Path,
    filtered: bool,
    workers: int = 4,
) -> Check:
    inspections = _inspect_all(catalog, repositories, workers)
    data = tuple(inspection.data for inspection in inspections)
    entries = {
        inspection.data.repository: IndexEntry(
            commit=inspection.commit,
            inputs_fingerprint=inspection.data.inputs_fingerprint,
            state=inspection.data.state,
        )
        for inspection in inspections
    }
    drifted: list[Path] = []
    for item in data:
        path = repository_file(inventory, item.repository)
        if not _matches(path, serialize(repository_dict(item))):
            drifted.append(path)
    index_path = inventory / "index.json"
    if not _matches(index_path, serialize(_index_payload(inventory, entries, filtered))):
        drifted.append(index_path)
    if not filtered:
        drifted.extend(_unselected_files(inventory, set(entries)))
    return Check(data, entries, tuple(drifted))


def repository_file(inventory: Path, repository: str) -> Path:
    owner, _, name = repository.partition("/")
    return inventory / "repos" / owner / f"{name}.json"


def repository_dict(data: RepositoryData) -> dict[str, object]:
    return {
        "schema": data.schema,
        "repository": data.repository,
        "tags": list(data.tags),
        "inputs": [
            {"path": item.path, "blob": item.blob}
            for item in sorted(data.inputs, key=lambda item: (item.path, item.blob))
        ],
        "inputs_fingerprint": data.inputs_fingerprint,
        "projects": [
            {"id": item.id, "ecosystem": item.ecosystem.value, "directory": item.directory}
            for item in sorted(data.projects, key=lambda item: item.id)
        ],
        "components": [
            {
                "purl": item.purl,
                "ecosystem": item.ecosystem.value,
                "name": item.name,
                "display_name": item.display_name,
            }
            for item in sorted(data.components, key=lambda item: item.purl)
        ],
        "instances": [
            {
                "id": item.id,
                "purl": item.purl,
                "component": item.component,
                "version": item.version,
                "source_kind": item.source_kind.value,
                "context": item.context,
                "registry_url": _private_url(item.registry_url),
                "repository_url": _private_url(item.repository_url),
                "vcs_url": _private_url(item.vcs_url),
                "commit": item.commit,
                "first_party": item.first_party,
            }
            for item in sorted(data.instances, key=lambda item: item.id)
        ],
        "edges": [_edge_dict(item) for item in sorted(data.edges, key=_edge_key)],
        "issues": [
            {
                "kind": item.kind.value,
                "path": item.path,
                "locator": redact_urls(item.locator) if item.locator is not None else None,
                "message": redact_urls(item.message),
            }
            for item in sorted(data.issues, key=_issue_key)
        ],
    }


def index_dict(entries: Mapping[str, IndexEntry], parser_version: int) -> dict[str, object]:
    return {
        "schema": SCHEMA_VERSION,
        "parser_version": parser_version,
        "repositories": {key: _entry_dict(entries[key]) for key in sorted(entries)},
    }


def serialize(value: dict[str, object]) -> str:
    return json.dumps(value, indent=1, sort_keys=True, ensure_ascii=False) + "\n"


def _entry_dict(entry: IndexEntry) -> dict[str, object]:
    return {
        "commit": entry.commit,
        "inputs_fingerprint": entry.inputs_fingerprint,
        "state": entry.state.value,
    }


def _edge_dict(edge: Edge) -> dict[str, object]:
    return {
        "from": edge.from_id,
        "to": edge.to_id,
        "relation": edge.relation.value,
        "scope": edge.scope.value,
        "optional": edge.optional,
        "constraint": redact_urls(edge.constraint) if edge.constraint is not None else None,
        "resolved_version": edge.resolved_version,
        "condition": edge.condition,
        "source_kind": edge.source_kind.value,
        "resolution": edge.resolution.value,
        "evidence": [{"path": item.path, "locator": redact_urls(item.locator)} for item in edge.evidence],
    }


def _private_url(value: str | None) -> str | None:
    return redact_url(value, drop_query_and_fragment=True) if value is not None else None


def _edge_key(edge: Edge) -> tuple[str, ...]:
    return (
        edge.from_id,
        edge.to_id,
        edge.scope.value,
        edge.relation.value,
        edge.constraint or "",
        edge.condition or "",
        edge.resolved_version or "",
        edge.source_kind.value,
        edge.resolution.value,
    )


def _issue_key(issue: Issue) -> tuple[str, str, str, str]:
    return (issue.path or "", issue.kind.value, issue.locator or "", issue.message)


def _repository_data(
    repository: Repository,
    inputs: Sequence[InputFile],
    issues: Sequence[Issue],
    projects: Sequence[Project] = (),
    components: Sequence[Component] = (),
    instances: Sequence[Instance] = (),
    edges: Sequence[Edge] = (),
) -> RepositoryData:
    ordered_inputs = tuple(sorted(inputs, key=lambda item: (item.path, item.blob)))
    by_project: dict[str, Project] = {}
    for project in projects:
        by_project.setdefault(project.id, project)
    by_component: dict[str, Component] = {}
    for component in components:
        by_component.setdefault(component.purl, component)
    by_instance: dict[str, Instance] = {}
    for instance in instances:
        by_instance.setdefault(instance.id, instance)
    unique_edges: dict[Edge, Edge] = {}
    for edge in edges:
        unique_edges.setdefault(edge, edge)
    nodes = by_project.keys() | by_instance.keys()
    for edge in unique_edges:
        if edge.from_id not in nodes or edge.to_id not in by_instance:
            raise ValueError(f"dependency edge has a missing endpoint: {edge.from_id} -> {edge.to_id}")
    return RepositoryData(
        repository=str(repository.id),
        tags=tuple(sorted(repository.tags)),
        inputs=ordered_inputs,
        inputs_fingerprint=fingerprint(ordered_inputs),
        projects=tuple(sorted(by_project.values(), key=lambda item: item.id)),
        components=tuple(sorted(by_component.values(), key=lambda item: item.purl)),
        instances=tuple(sorted(by_instance.values(), key=lambda item: item.id)),
        edges=tuple(sorted(unique_edges, key=_edge_key)),
        issues=tuple(sorted(issues, key=_issue_key)),
    )


def _unavailable(repository: Repository, kind: CoverageKind, message: str) -> RepositoryInspection:
    return RepositoryInspection(_single_issue(repository, Issue(kind, message)), None)


def _failed(
    repository: Repository, message: str, commit: str | None = None
) -> RepositoryInspection:
    return RepositoryInspection(_single_issue(repository, Issue(CoverageKind.GIT_FAILURE, message)), commit)


def _single_issue(repository: Repository, issue: Issue) -> RepositoryData:
    return RepositoryData(
        repository=str(repository.id),
        tags=tuple(sorted(repository.tags)),
        inputs=(),
        inputs_fingerprint=fingerprint(()),
        issues=(issue,),
    )


def _inspect_all(
    catalog: Catalog, repositories: Sequence[Repository], workers: int
) -> tuple[RepositoryInspection, ...]:
    if workers < 1:
        raise ValueError("workers must be at least one")
    ordered = tuple(sorted(repositories, key=lambda repository: str(repository.id)))
    if workers == 1 or len(ordered) <= 1:
        return tuple(inspect_repository(catalog, repository) for repository in ordered)
    with ThreadPoolExecutor(
        max_workers=min(workers, len(ordered)), thread_name_prefix="repomod-deps"
    ) as pool:
        return tuple(pool.map(partial(inspect_repository, catalog), ordered))


def _adapter_for(path: str) -> Adapter | None:
    return next((adapter for adapter in ADAPTERS if adapter.matches(path)), None)


def _is_vendored(path: str, roots: Sequence[str]) -> bool:
    return any(path.startswith(root + "/") for root in roots)


def _index_payload(
    inventory: Path, entries: Mapping[str, IndexEntry], filtered: bool
) -> dict[str, object]:
    if not filtered:
        return index_dict(entries, PARSER_VERSION)
    path = inventory / "index.json"
    try:
        source = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return index_dict(entries, PARSER_VERSION)
    if (
        not isinstance(source, dict)
        or source.get("schema") != SCHEMA_VERSION
        or type(source.get("parser_version")) is not int
        or not isinstance(source.get("repositories"), dict)
    ):
        raise ValueError(f"cannot read the dependency inventory index: {path}")
    if source["parser_version"] != PARSER_VERSION:
        raise ValueError(
            f"dependency inventory at {path.parent} uses parser version "
            f"{source['parser_version']}; run an unfiltered snapshot"
        )
    repositories = cast(dict[str, dict[str, object]], source["repositories"])
    merged = dict(repositories)
    for key, entry in entries.items():
        merged[key] = _entry_dict(entry)
    return {"schema": SCHEMA_VERSION, "parser_version": PARSER_VERSION, "repositories": merged}


def _write_if_changed(path: Path, content: str) -> bool:
    payload = content.encode("utf-8")
    try:
        if path.read_bytes() == payload:
            return False
    except FileNotFoundError:
        pass
    atomic_write(path, content)
    return True


def _matches(path: Path, content: str) -> bool:
    try:
        return path.read_bytes() == content.encode("utf-8")
    except FileNotFoundError:
        return False


def _unselected_files(inventory: Path, selected: set[str]) -> tuple[Path, ...]:
    root = inventory / "repos"
    if not root.is_dir():
        return ()
    extra: list[Path] = []
    for owner_dir in sorted(root.iterdir()):
        if owner_dir.is_symlink() or not owner_dir.is_dir():
            continue
        for path in sorted(owner_dir.glob("*.json")):
            if path.is_symlink() or not path.is_file():
                continue
            if f"{owner_dir.name}/{path.stem}" not in selected:
                extra.append(path)
    return tuple(extra)


def _delete_unselected(inventory: Path, selected: set[str]) -> tuple[Path, ...]:
    deleted: list[Path] = []
    for path in _unselected_files(inventory, selected):
        path.unlink()
        deleted.append(path)
    root = inventory / "repos"
    if root.is_dir():
        for owner_dir in sorted(root.iterdir()):
            if owner_dir.is_symlink() or not owner_dir.is_dir():
                continue
            if not any(owner_dir.iterdir()):
                owner_dir.rmdir()
    return tuple(deleted)
