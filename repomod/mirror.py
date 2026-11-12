"""Publish to stuff the registry packages the fleet locks or is about to upgrade to.

The stuff registries mirror only what lockfiles ask for, while audit-upgrades only proposes
versions stuff already serves. Mirroring relocks each repository against the upstream registries
in a disposable copy, and publishes whatever the current and upgraded lockfiles need that stuff
does not serve yet.
"""

from __future__ import annotations

import os
import subprocess
import tempfile
import time
import tomllib
from collections.abc import Callable, Iterable, Mapping, Sequence
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field
from pathlib import Path

from packaging.utils import canonicalize_name
from packaging.version import InvalidVersion, Version
from ruamel.yaml import YAML
from ruamel.yaml.error import YAMLError

from repomod import audit
from repomod import catalog as catalog_module
from repomod.dependencies.model import Ecosystem
from repomod.rules import audit_upgrades
from repomod.rules.python_standard import load_approved_python_tool_versions
from repomod.rules.registry_sources import RegistryPolicy, load_registry_policy
from repomod.types import Catalog, Repository

_CRATES_IO_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"
_LOCKFILE_ECOSYSTEM = {"Cargo.lock": Ecosystem.CARGO, "pnpm-lock.yaml": Ecosystem.NPM, "uv.lock": Ecosystem.PYPI}
_NPM_PLATFORMS = {"os": frozenset({"darwin", "linux"}), "cpu": frozenset({"x64", "arm64"})}
_UPSTREAM_REGISTRIES: dict[Ecosystem, frozenset[str | None]] = {
    Ecosystem.CARGO: frozenset({None}),
    Ecosystem.NPM: frozenset({None, audit_upgrades.UPSTREAM_NPM_REGISTRY, "https://registry.npmjs.org"}),
    Ecosystem.PYPI: frozenset({None, audit_upgrades.UPSTREAM_PYTHON_INDEX, "https://pypi.org/simple/"}),
}
_PRUNED = frozenset({".git", ".work", ".venv", "node_modules", "target", "__pycache__"})
_SERVED_TIMEOUT_SECONDS = 300.0
_SERVED_POLL_SECONDS = 15.0

type Runner = Callable[[Sequence[str]], subprocess.CompletedProcess[str]]
type Served = Mapping[Ecosystem, Mapping[str, frozenset[str]]]


@dataclass(frozen=True, order=True, slots=True)
class Package:
    ecosystem: Ecosystem
    name: str
    version: str

    @property
    def label(self) -> str:
        separator = "==" if self.ecosystem is Ecosystem.PYPI else "@"
        return f"{self.ecosystem.value}:{self.name}{separator}{self.version}"


@dataclass(frozen=True, slots=True)
class Lockfile:
    ecosystem: Ecosystem
    project: str
    path: Path
    packages: frozenset[Package]


@dataclass(slots=True)
class Plan:
    lockfiles: list[Lockfile] = field(default_factory=list)
    missing: set[Package] = field(default_factory=set)
    failures: list[str] = field(default_factory=list)

    def lockfiles_for(self, ecosystem: Ecosystem) -> list[Lockfile]:
        return [
            lockfile
            for lockfile in self.lockfiles
            if lockfile.ecosystem is ecosystem and not lockfile.packages.isdisjoint(self.missing)
        ]

    def missing_in(self, ecosystem: Ecosystem) -> list[Package]:
        return sorted(package for package in self.missing if package.ecosystem is ecosystem)


@dataclass(slots=True)
class Report:
    published: set[Package] = field(default_factory=set)
    unserved: set[Package] = field(default_factory=set)
    failures: list[str] = field(default_factory=list)


def parse_cargo_lock(text: str) -> frozenset[Package]:
    document = tomllib.loads(text)
    found = set()
    for raw in document.get("package", []):
        if not isinstance(raw, dict):
            continue
        name, version, source = raw.get("name"), raw.get("version"), raw.get("source")
        if isinstance(name, str) and isinstance(version, str) and source == _CRATES_IO_SOURCE:
            found.add(Package(Ecosystem.CARGO, name, version))
    return frozenset(found)


def parse_uv_lock(text: str) -> frozenset[Package]:
    document = tomllib.loads(text)
    found = set()
    for raw in document.get("package", []):
        if not isinstance(raw, dict):
            continue
        name, version, source = raw.get("name"), raw.get("version"), raw.get("source")
        if isinstance(name, str) and isinstance(version, str) and isinstance(source, dict) and "registry" in source:
            found.add(Package(Ecosystem.PYPI, canonicalize_name(name), version))
    return frozenset(found)


def _npm_targeted(os: object, cpu: object) -> bool:
    """Whether stuff's npm mirror, which serves only macOS and Linux on x64 and arm64, hosts the package."""
    for values, served in ((os, _NPM_PLATFORMS["os"]), (cpu, _NPM_PLATFORMS["cpu"])):
        if not isinstance(values, list) or not values:
            continue
        allowed = {value for value in values if isinstance(value, str) and not value.startswith("!")}
        denied = {value[1:] for value in values if isinstance(value, str) and value.startswith("!")}
        if (allowed and allowed.isdisjoint(served)) or (not allowed and served <= denied):
            return False
    return True


def parse_pnpm_lock(text: str, registries: Iterable[str]) -> frozenset[Package]:
    """Return registry packages stuff mirrors; tarballs count only when they come from one of registries."""
    try:
        documents = list(YAML(typ="safe", pure=True).load_all(text))
    except YAMLError as error:
        raise ValueError(f"pnpm-lock.yaml is not valid YAML: {error}") from error
    packages: dict[object, object] = {}
    for document in documents:
        section = document.get("packages") if isinstance(document, dict) else None
        if isinstance(section, dict):
            packages.update(section)
    prefixes = tuple(registries)
    found = set()
    for key, raw in packages.items():
        if not isinstance(key, str) or not isinstance(raw, dict):
            continue
        name, separator, version = key.rpartition("@")
        resolution = raw.get("resolution")
        if not separator or not name or not isinstance(resolution, dict) or "integrity" not in resolution:
            continue
        tarball = resolution.get("tarball")
        if tarball is not None and not (isinstance(tarball, str) and tarball.startswith(prefixes)):
            continue
        if not _npm_targeted(raw.get("os"), raw.get("cpu")):
            continue
        found.add(Package(Ecosystem.NPM, name, version))
    return frozenset(found)


def _parse_lockfile(path: Path, policy: RegistryPolicy) -> frozenset[Package]:
    text = path.read_text(encoding="utf-8")
    ecosystem = _LOCKFILE_ECOSYSTEM[path.name]
    if ecosystem is Ecosystem.CARGO:
        return parse_cargo_lock(text)
    if ecosystem is Ecosystem.PYPI:
        return parse_uv_lock(text)
    return parse_pnpm_lock(text, (policy.npm_registry, audit_upgrades.UPSTREAM_NPM_REGISTRY))


def _key(package: Package) -> tuple[str, str]:
    if package.ecosystem is Ecosystem.PYPI:
        try:
            version = str(Version(package.version))
        except InvalidVersion:
            version = package.version
        return canonicalize_name(package.name), version
    if package.ecosystem is Ecosystem.CARGO:
        return package.name.replace("_", "-").casefold(), package.version
    return package.name.casefold(), package.version


def is_served(package: Package, served: Served) -> bool:
    name, version = _key(package)
    return version in served.get(package.ecosystem, {}).get(name, frozenset())


def read_served(policy: RegistryPolicy) -> dict[Ecosystem, dict[str, frozenset[str]]]:
    registries = {
        Ecosystem.CARGO: policy.cargo_source,
        Ecosystem.NPM: policy.npm_registry,
        Ecosystem.PYPI: policy.python_index,
    }
    served: dict[Ecosystem, dict[str, frozenset[str]]] = {}
    for ecosystem, registry in registries.items():
        catalog = audit.read_mirror_catalog(registry)
        versions: dict[str, set[str]] = {}
        for name, listed in catalog.items():
            for version in listed:
                key_name, key_version = _key(Package(ecosystem, name, version))
                versions.setdefault(key_name, set()).add(key_version)
        served[ecosystem] = {name: frozenset(values) for name, values in versions.items()}
    return served


def _mirrored_ecosystems(root: Path, repository: Repository, policy: RegistryPolicy) -> frozenset[Ecosystem]:
    """Ecosystems the repository resolves, or the registry-sources fixer will make it resolve, from stuff.

    A repository on another registry, such as an allowed work proxy, keeps it; one exempt from
    registry-sources stays on the public registries.
    """
    configured = audit_upgrades._configured_sources(root)
    moving = "registry-sources" not in repository.ignored_rules
    found = set()
    for ecosystem, current, stuff in (
        (Ecosystem.CARGO, configured.cargo if configured else None, policy.cargo_source),
        (Ecosystem.NPM, configured.npm if configured else None, policy.npm_registry),
        (Ecosystem.PYPI, configured.python if configured else None, policy.python_index),
    ):
        if current == stuff or (moving and current in _UPSTREAM_REGISTRIES[ecosystem]):
            found.add(ecosystem)
    return frozenset(found)


def _lockfiles_in(tree: Path, ecosystems: frozenset[Ecosystem]) -> list[Path]:
    """Every lockfile in the tree, since CI may build nested fixtures against the same registry."""
    names = {name for name, ecosystem in _LOCKFILE_ECOSYSTEM.items() if ecosystem in ecosystems}
    found = []
    for directory, subdirectories, files in os.walk(tree):
        subdirectories[:] = sorted(name for name in subdirectories if name not in _PRUNED)
        base = Path(directory)
        found.extend(
            base / name for name in sorted(files) if name in names and not (base / name).is_symlink()
        )
    return found


def _project_id(repository: Repository, tree: Path, lockfile: Path) -> str:
    relative = lockfile.parent.relative_to(tree).as_posix()
    base = f"{repository.id.owner}-{repository.id.name}"
    return base if relative == "." else f"{base}-{relative.replace('/', '-')}"


def eligible(repositories: Iterable[Repository]) -> list[Repository]:
    """Repositories audit-upgrades would upgrade."""
    return [
        repository
        for repository in repositories
        if not repository.private
        and "fork" not in repository.tags
        and "audit-upgrades" not in repository.ignored_rules
    ]


def _repository_lockfiles(
    catalog: Catalog, repository: Repository, policy: RegistryPolicy, scratch: Path
) -> tuple[list[Lockfile], str | None]:
    """Return the lockfiles to mirror and why the upstream relock failed, if it did.

    A failed relock still mirrors the current lockfiles.
    """
    root = catalog_module.repository_path(catalog, repository)
    if not root.is_dir():
        raise RuntimeError("checkout is missing; run repomod sync")
    ecosystems = _mirrored_ecosystems(root, repository, policy)
    if not ecosystems:
        return [], None
    trees = [root]
    work = scratch / f"{repository.id.owner}-{repository.id.name}" / "sandbox" / root.name
    relock_error = None
    try:
        if audit_upgrades.upgrade_in_sandbox(root, repository, work, upstream=True):
            trees.append(work)
    except Exception as error:
        relock_error = f"upstream relock failed: {error}"
    return [
        Lockfile(
            _LOCKFILE_ECOSYSTEM[path.name],
            _project_id(repository, tree, path),
            path,
            _parse_lockfile(path, policy),
        )
        for tree in trees
        for path in _lockfiles_in(tree, ecosystems)
    ], relock_error


def plan(
    catalog: Catalog,
    repositories: Sequence[Repository],
    policy: RegistryPolicy,
    served: Served,
    scratch: Path,
    *,
    workers: int = 8,
) -> Plan:
    result = Plan()
    with ThreadPoolExecutor(max_workers=max(1, min(workers, len(repositories) or 1))) as pool:
        futures = {
            pool.submit(_repository_lockfiles, catalog, repository, policy, scratch): repository
            for repository in repositories
        }
        for future in as_completed(futures):
            repository = futures[future]
            try:
                lockfiles, relock_error = future.result()
            except Exception as error:
                result.failures.append(f"{repository.id}: {error}")
                continue
            result.lockfiles.extend(lockfiles)
            if relock_error is not None:
                result.failures.append(f"{repository.id}: {relock_error}")
    result.lockfiles.sort(key=lambda lockfile: (lockfile.ecosystem.value, lockfile.project, str(lockfile.path)))
    result.failures.sort()
    wanted = {package for lockfile in result.lockfiles for package in lockfile.packages}
    if any(lockfile.ecosystem is Ecosystem.PYPI for lockfile in result.lockfiles):
        wanted.update(
            Package(Ecosystem.PYPI, name, version) for name, version in load_approved_python_tool_versions().items()
        )
    result.missing = {package for package in wanted if not is_served(package, served)}
    return result


def _stuff(runner: Runner, arguments: Sequence[str]) -> str | None:
    completed = runner(["stuff", "publish", *arguments])
    if completed.returncode == 0:
        return None
    detail = (completed.stderr or completed.stdout or "").strip()
    return detail.splitlines()[-1] if detail else f"exited with status {completed.returncode}"


def _publish_batches(
    runner: Runner, batches: Sequence[tuple[str, Sequence[str]]], combined: Sequence[str] | None
) -> list[str]:
    """Run the combined command when given, falling back to each batch to isolate failures."""
    if combined is not None and len(batches) > 1 and _stuff(runner, combined) is None:
        return []
    failures = []
    for label, arguments in batches:
        error = _stuff(runner, arguments)
        if error is not None:
            failures.append(f"{label}: {error}")
    return failures


def publish(selected: Plan, runner: Runner) -> list[str]:
    failures = []
    cargo = selected.lockfiles_for(Ecosystem.CARGO)
    if cargo:
        failures.extend(
            _publish_batches(
                runner,
                [(f"cargo {lockfile.project}", ["cargo", "--lockfile", str(lockfile.path)]) for lockfile in cargo],
                ["cargo", *(argument for lockfile in cargo for argument in ("--lockfile", str(lockfile.path)))],
            )
        )
    failures.extend(
        _publish_batches(
            runner,
            [
                (f"npm {lockfile.project}", ["npm", "--lockfile", str(lockfile.path), "--project", lockfile.project])
                for lockfile in selected.lockfiles_for(Ecosystem.NPM)
            ],
            None,
        )
    )
    python = [f"{package.name}=={package.version}" for package in selected.missing_in(Ecosystem.PYPI)]
    if python:
        failures.extend(
            _publish_batches(
                runner, [(f"pypi {spec}", ["pypi", spec]) for spec in python], ["pypi", *python]
            )
        )
    return failures


def wait_until_served(
    missing: Iterable[Package],
    read: Callable[[], Served],
    *,
    timeout: float = _SERVED_TIMEOUT_SECONDS,
    interval: float = _SERVED_POLL_SECONDS,
    clock: Callable[[], float] = time.monotonic,
    sleep: Callable[[float], None] = time.sleep,
) -> set[Package]:
    """Poll the stuff catalogs until every package is served; return those still missing."""
    pending: set[Package] = set(missing)
    deadline = clock() + timeout
    while pending:
        served = read()
        pending = {package for package in pending if not is_served(package, served)}
        if not pending or clock() >= deadline:
            return pending
        sleep(interval)
    return pending


def _run(arguments: Sequence[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        list(arguments), capture_output=True, text=True, encoding="utf-8", errors="replace", check=False
    )


def mirror(
    catalog: Catalog,
    repositories: Sequence[Repository],
    *,
    dry_run: bool,
    workers: int = 8,
    runner: Runner = _run,
    policy: RegistryPolicy | None = None,
    read: Callable[[RegistryPolicy], Served] = read_served,
    wait: Callable[[Iterable[Package], Callable[[], Served]], set[Package]] = wait_until_served,
    on_plan: Callable[[Plan], None] | None = None,
) -> Report:
    policy = policy or load_registry_policy()
    report = Report()
    with tempfile.TemporaryDirectory(prefix="repomod-mirror-") as temporary:
        selected = plan(catalog, eligible(repositories), policy, read(policy), Path(temporary), workers=workers)
        if on_plan is not None:
            on_plan(selected)
        report.failures.extend(selected.failures)
        if dry_run or not selected.missing:
            return report
        report.failures.extend(publish(selected, runner))
    report.unserved = wait(selected.missing, lambda: read(policy))
    report.published = selected.missing - report.unserved
    if report.published:
        audit.forget_latest((policy.cargo_source, policy.npm_registry, policy.python_index))
    return report


def summary(selected: Plan) -> list[str]:
    lines = []
    for ecosystem in (Ecosystem.CARGO, Ecosystem.NPM, Ecosystem.PYPI):
        missing = selected.missing_in(ecosystem)
        if missing:
            lines.append(f"{ecosystem.value}: {len(missing)} missing on stuff")
            lines.extend(f"  {package.name} {package.version}" for package in missing)
    return lines or ["stuff already serves every locked and upgradable version"]
