from __future__ import annotations

import os
import shutil
from pathlib import Path

import pytest
from packaging.utils import canonicalize_name

from repomod.audit import _collect_locked, _collect_vuln_candidates
from repomod.catalog import Catalog, repository_path
from repomod.cmd.common import default_catalog, select_repositories
from repomod.dependencies import snapshot
from repomod.dependencies.model import CoverageKind, Ecosystem, Relation, Resolution
from repomod.git import GitError, run_git
from repomod.types import Repository

pytestmark = pytest.mark.skipif(
    os.environ.get("REPOMOD_FLEET") != "1",
    reason="set REPOMOD_FLEET=1 to run the live fleet tests",
)

_ECOSYSTEMS = {
    "crates": Ecosystem.CARGO,
    "pypi": Ecosystem.PYPI,
    "npm": Ecosystem.NPM,
    "go": Ecosystem.GOLANG,
}
_LOCKS = {
    Ecosystem.CARGO: ("Cargo.lock",),
    Ecosystem.PYPI: ("uv.lock",),
    Ecosystem.NPM: ("pnpm-lock.yaml",),
}


def _checkout_state(path: Path) -> tuple[str, str] | None:
    if not path.is_dir():
        return None
    try:
        head = run_git(["rev-parse", "HEAD"], cwd=path).stdout.strip()
        status = run_git(["status", "--porcelain"], cwd=path, check=False).stdout
    except GitError:
        return None
    return head, status


def _files(inventory: Path) -> dict[str, bytes]:
    return {
        path.relative_to(inventory).as_posix(): path.read_bytes()
        for path in sorted(inventory.rglob("*.json"))
    }


def _snapshot(
    catalog: Catalog, repositories: tuple[Repository, ...], inventory: Path, workers: int
) -> snapshot.Snapshot:
    shutil.rmtree(inventory, ignore_errors=True)
    return snapshot.snapshot(
        catalog, repositories, inventory=inventory, filtered=False, workers=workers
    )


def _normalize_name(ecosystem: Ecosystem, name: str) -> str:
    if ecosystem is Ecosystem.CARGO:
        return name.replace("_", "-").casefold()
    if ecosystem is Ecosystem.PYPI:
        return canonicalize_name(name)
    if ecosystem is Ecosystem.NPM:
        return name.casefold()
    return name


def _normalize_version(ecosystem: Ecosystem, version: str) -> str:
    return version.split("(", 1)[0] if ecosystem is Ecosystem.NPM else version


def test_fleet_inventory_is_deterministic_and_read_only(tmp_path: Path) -> None:
    catalog = default_catalog()
    repositories = select_repositories(catalog, [], [])
    before = {
        str(repository.id): _checkout_state(repository_path(catalog, repository))
        for repository in repositories
    }

    one = _snapshot(catalog, repositories, tmp_path / "one", workers=1)
    _snapshot(catalog, repositories, tmp_path / "eight", workers=8)
    assert _files(tmp_path / "one") == _files(tmp_path / "eight")

    second = snapshot.snapshot(
        catalog,
        repositories,
        inventory=tmp_path / "one",
        filtered=False,
        workers=8,
    )
    assert second.written == ()
    assert _files(tmp_path / "one") == _files(tmp_path / "eight")

    parse_errors = [
        f"{repository.repository}: {issue.path}: {issue.message}"
        for repository in one.repositories
        for issue in repository.issues
        if issue.kind is CoverageKind.PARSE_ERROR
    ]
    assert parse_errors == []

    after = {
        str(repository.id): _checkout_state(repository_path(catalog, repository))
        for repository in repositories
    }
    assert after == before


def test_fleet_matches_the_audit(tmp_path: Path) -> None:
    """D7 parity, allowing the inventory's documented improvements.

    The inventory reads the committed tree and separates local packages:
    an ignored working-tree lock is invisible to it, and a local workspace
    package is a generic first-party instance rather than a registry
    dependency. A nested lock is more precise than the audit's root lock,
    and a nested package without its own lock is declared-only. Replacements
    change the resolved identity. Anything else is a miss.
    """
    catalog = default_catalog()
    repositories = select_repositories(catalog, [], [])
    result = _snapshot(catalog, repositories, tmp_path / "inventory", workers=8)
    data_by_repository = {repository.repository: repository for repository in result.repositories}
    instances: dict[tuple[str, str, str], set[str]] = {}
    generic: dict[tuple[str, str | None], set[str]] = {}
    direct: set[str] = set()
    direct_by_name: dict[tuple[str, str], set[str]] = {}
    direct_constraints: dict[str, set[tuple[str, str]]] = {}
    direct_declared: dict[tuple[str, str], set[str]] = {}
    version_of: dict[str, str | None] = {}
    for repository in result.repositories:
        components = {component.purl: component for component in repository.components}
        by_id = {instance.id: instance for instance in repository.instances}
        for instance in repository.instances:
            component = components.get(instance.component)
            if component is None:
                continue
            name = _normalize_name(component.ecosystem, component.name)
            version = _normalize_version(component.ecosystem, instance.version or "")
            instances.setdefault((component.ecosystem.value, name, version), set()).add(
                instance.id
            )
            version_of[instance.id] = instance.version
            if component.ecosystem is Ecosystem.GENERIC:
                for normalized in {
                    name.casefold(),
                    _normalize_name(Ecosystem.CARGO, component.name),
                    _normalize_name(Ecosystem.PYPI, component.name),
                }:
                    generic.setdefault((normalized, instance.version), set()).add(instance.id)
                    generic.setdefault((normalized, None), set()).add(instance.id)
        for edge in repository.edges:
            if edge.relation is not Relation.DIRECT or edge.to_id not in by_id:
                continue
            direct.add(edge.to_id)
            target = by_id[edge.to_id]
            component = components.get(target.component)
            if component is None:
                continue
            name = _normalize_name(component.ecosystem, component.name)
            direct_by_name.setdefault((component.ecosystem.value, name), set()).add(edge.to_id)
            if edge.resolution is Resolution.DECLARED_ONLY:
                direct_declared.setdefault((component.ecosystem.value, name), set()).add(
                    edge.to_id
                )
            if edge.constraint is not None:
                direct_constraints.setdefault(edge.constraint, set()).add(
                    (component.ecosystem.value, name)
                )

    unexplained: list[str] = []
    for repository in repositories:
        path = repository_path(catalog, repository)
        state = _checkout_state(path)
        if state is None:
            continue
        head, status = state
        try:
            origin = run_git(
                ["rev-parse", "refs/remotes/origin/HEAD"], cwd=path
            ).stdout.strip()
        except GitError:
            continue
        if status.strip() or head != origin:
            continue
        if str(repository.id) not in data_by_repository:
            continue
        committed = run_git(
            ["ls-tree", "-r", "--name-only", "refs/remotes/origin/HEAD"], cwd=path
        ).stdout.splitlines()
        locked, _findings = _collect_locked(path, repository)
        candidates = _collect_vuln_candidates(path, repository, locked)
        for candidate in candidates:
            ecosystem = _ECOSYSTEMS.get(candidate.ecosystem)
            if ecosystem is None:
                continue
            name = _normalize_name(ecosystem, candidate.name)
            version = _normalize_version(ecosystem, candidate.locked)
            found = instances.get((ecosystem.value, name, version))
            if found is None or len(found) == 0:
                if _local_package(generic, name, version):
                    continue
                if _ignored_lock(ecosystem, committed):
                    continue
                if ecosystem is Ecosystem.GOLANG and _replaced(
                    direct_constraints, candidate.locked, name
                ):
                    continue
                unexplained.append(
                    f"{repository.id}: {candidate.ecosystem} {candidate.name} "
                    f"{candidate.locked} is missing"
                )
                continue
            if candidate.direct and not (found & direct):
                if _local_package(generic, name, version):
                    continue
                if direct_by_name.get((ecosystem.value, name), set()) & generic.get(
                    (name, version), set()
                ):
                    continue
                if _ignored_lock(ecosystem, committed):
                    continue
                if ecosystem is Ecosystem.CARGO and _nested_lock(
                    direct_by_name.get((ecosystem.value, name), set()), version, version_of
                ):
                    continue
                if bool(direct_declared.get((ecosystem.value, name))):
                    continue
                unexplained.append(
                    f"{repository.id}: {candidate.ecosystem} {candidate.name} "
                    f"{candidate.locked} is direct in the audit"
                )
    assert unexplained == []


def _local_package(
    generic: dict[tuple[str, str | None], set[str]], name: str, version: str
) -> bool:
    return bool(generic.get((name, version)) or generic.get((name, None)))


def _ignored_lock(ecosystem: Ecosystem, committed: list[str]) -> bool:
    names = _LOCKS.get(ecosystem)
    if names is None:
        return False
    return not any(
        path == name or path.endswith("/" + name) for path in committed for name in names
    )


def _nested_lock(
    direct_targets: set[str], version: str, version_of: dict[str, str | None]
) -> bool:
    return any(
        value is not None and value != version
        for value in (version_of.get(target) for target in direct_targets)
    )


def _replaced(
    constraints: dict[str, set[tuple[str, str]]], version: str, normalized: str
) -> bool:
    return any(
        ecosystem == Ecosystem.GOLANG.value and name != normalized
        for ecosystem, name in constraints.get(version, set())
    )
