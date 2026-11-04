import os
import shutil
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path

from . import wirejson
from .fsutil import list_dir
from .errors import InputError, IntegrityError, NetworkError, OperationError
from .lockfile import is_crates_io_source, parse_lockfile
from .pins import read_pins
from .store import Store, UpstreamRecord
from .model import IndexEntry

DENIED_PROXY = "http://127.0.0.1:1"
SKIPPED_PROJECT_ENTRIES = ("target", ".git", ".work")


@dataclass(frozen=True)
class ValidationResult:
    project: str
    registry_count: int
    unsupported_count: int
    is_partial: bool


def validate_archive(state_dir, pins_path):
    store = Store(state_dir)
    store.assert_current_format()
    pins = read_pins(pins_path)
    project = pins.project or Path(pins_path).stem
    store.verify()

    for pin in pins.registry:
        record_path = store.upstream_dir / pin.name.lower() / f"{pin.version}.json"
        if not record_path.exists() or not store.blob_path(pin.checksum).exists():
            raise IntegrityError(f"incomplete archive: missing package {pin.name} {pin.version}")
        try:
            record = UpstreamRecord.from_obj(wirejson.loads(record_path.read_text(encoding="utf-8")))
        except InputError as error:
            raise IntegrityError(f"invalid upstream record {record_path}: {error.message}") from error
        if record.checksum != pin.checksum:
            raise IntegrityError(
                f"archive checksum changed for {pin.name} {pin.version}: "
                f"pin {pin.checksum} != record {record.checksum}"
            )
        if record.vers != pin.version or record.name.lower() != pin.name.lower():
            raise IntegrityError(f"archive record identity changed for {pin.name} {pin.version}")
        try:
            entry = IndexEntry.from_obj(wirejson.loads(record.raw_index_line))
        except InputError as error:
            raise IntegrityError(f"invalid cached index line for {pin.name} {pin.version}: {error.message}") from error
        if (
            entry.cksum != pin.checksum
            or entry.name.lower() != pin.name.lower()
            or entry.vers != pin.version
        ):
            raise IntegrityError(f"cached index line mismatch for {pin.name} {pin.version}")
        if entry.yanked:
            raise IntegrityError(f"archived package {pin.name} {pin.version} is yanked upstream")

    if pins.lock_content is not None:
        parsed = parse_lockfile(pins.lock_content)
        locked = {(p.name.lower(), p.version): p for p in parsed.packages if p.checksum is not None}
        for pin in pins.registry:
            package = locked.get((pin.name.lower(), pin.version))
            if package is None:
                raise IntegrityError(f"pinned package {pin.name} {pin.version} missing from lockfile")
            if package.checksum != pin.checksum:
                raise IntegrityError(f"pin checksum changed for {pin.name} {pin.version}")
        pinned = {(p.name.lower(), p.version, p.checksum) for p in pins.registry}
        for package in parsed.packages:
            if (
                package.checksum is not None
                and (package.name.lower(), package.version, package.checksum) not in pinned
                and is_crates_io_source(package.source)
            ):
                raise IntegrityError(f"lockfile package {package.name} {package.version} not in pinned archive")

    return ValidationResult(project, len(pins.registry), len(pins.unsupported), bool(pins.unsupported))


def validate_project(state_dir, pins_path, mirror_index_url, project_dir):
    archived = validate_archive(state_dir, pins_path)
    if archived.registry_count == 0:
        return archived
    pins = read_pins(pins_path)
    if pins.lock_content is None:
        raise InputError("validate: pins file has no captured Cargo.lock (re-import the lockfile)")
    parse_lockfile(pins.lock_content)
    if project_dir is None:
        raise InputError(
            "validate: pinned archive is intact but project manifests are required for offline verification; "
            "re-run with --project-dir <workspace> (or --manifest-path <Cargo.toml>)"
        )
    project_dir = Path(project_dir)
    if not project_dir.exists():
        raise InputError(f"validate: project directory not found: {project_dir}")
    manifest_path = project_dir if project_dir.is_file() else project_dir / "Cargo.toml"
    source_dir = manifest_path.parent if project_dir.is_file() else project_dir
    if not manifest_path.exists():
        raise InputError(
            f"validate: manifest not found: {manifest_path} (supply --project-dir pointing at the workspace root)"
        )
    lock_path = source_dir / "Cargo.lock"
    if not lock_path.exists():
        raise InputError(
            f"validate: project has no Cargo.lock at {lock_path} (generate it with cargo before validating)"
        )
    if lock_path.read_text(encoding="utf-8") != pins.lock_content:
        raise IntegrityError(
            "validate: project Cargo.lock differs from imported pins lock_content (re-run import-lock)"
        )

    scratch = (Path(state_dir) / f".validate_tmp_{os.getpid()}_{time.time_ns()}").resolve()
    scratch.mkdir(parents=True)
    try:
        test_project = scratch / "proj"
        _copy_project(source_dir, test_project)
        cargo_home = scratch / "cargo_home"
        cargo_home.mkdir()
        (cargo_home / "config.toml").write_text(
            '[source.crates-io]\nreplace-with = "mycrates-mirror"\n\n'
            f'[source.mycrates-mirror]\nregistry = "sparse+{mirror_index_url}"\n',
            encoding="utf-8",
        )
        environment = dict(os.environ)
        environment.update(
            CARGO_HOME=str(cargo_home),
            CARGO_TERM_COLOR="never",
            CARGO_HTTP_PROXY=DENIED_PROXY,
            HTTPS_PROXY=DENIED_PROXY,
            NO_PROXY="127.0.0.1,localhost",
        )
        for label, args in (
            ("cargo metadata --locked", ["metadata", "--locked", "--format-version", "1"]),
            ("cargo fetch --locked", ["fetch", "--locked"]),
        ):
            try:
                result = subprocess.run(
                    ["cargo", *args], cwd=test_project, env=environment, capture_output=True, text=True
                )
            except OSError as error:
                raise OperationError(f"failed to run {label}: {error}") from error
            if result.returncode != 0:
                raise NetworkError(f"{label} failed: {result.stderr}")
        if (test_project / "Cargo.lock").read_text(encoding="utf-8") != pins.lock_content:
            raise IntegrityError("Cargo.lock modified during verification")
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    return archived


def _copy_project(source, destination):
    destination.mkdir(parents=True)
    for entry in list_dir(source):
        if entry.name in SKIPPED_PROJECT_ENTRIES:
            continue
        target = destination / entry.name
        if entry.is_symlink():
            raise InputError(f"validate: refusing to copy symlink in project: {entry.path}")
        if entry.is_dir():
            _copy_project(Path(entry.path), target)
        elif entry.is_file():
            shutil.copyfile(entry.path, target)
