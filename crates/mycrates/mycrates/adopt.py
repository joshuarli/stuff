import os
import re
from dataclasses import dataclass
from pathlib import Path

from .fsutil import list_dir
from .errors import InputError, IntegrityError
from .model import IndexEntry
from .names import shard_for_name, validate_crate_name
from .store import NativeRecord, UpstreamRecord
from .wirejson import loads

SHA256_HEX = re.compile(r"^[0-9a-f]{64}$")
GENERATION_DIRECTORY = re.compile(r"^generations/([0-9A-Za-z]+)$")
VIEWS = (("registry", "native"), ("crates-io", "upstream"))


@dataclass
class AdoptSummary:
    native: int = 0
    upstream: int = 0
    blobs: int = 0


def adopt_site(store, site_dir):
    site_dir = Path(site_dir)
    generation = _current_generation_dir(site_dir)
    summary = AdoptSummary()
    with store.lock_exclusive():
        for view_dir, view in VIEWS:
            root = generation / view_dir
            for path in _index_files(root):
                relative = path.relative_to(root).as_posix()
                for line in path.read_text(encoding="utf-8").split("\n"):
                    line = line.rstrip("\r")
                    if not line.strip():
                        continue
                    entry = IndexEntry.from_obj(loads(line))
                    validate_crate_name(entry.name)
                    expected = shard_for_name(entry.name.lower())
                    if expected is None:
                        raise IntegrityError(f"index line name is not ASCII: {entry.name}")
                    if expected != relative:
                        raise IntegrityError(f"index line for {entry.name} is stored at {relative}")
                    summary.blobs += _adopt_blob(store, site_dir, entry.cksum)
                    if view == "native":
                        store.put_native_record(NativeRecord(entry))
                        summary.native += 1
                    else:
                        store.put_upstream_record(
                            UpstreamRecord(entry.name, entry.vers, entry.cksum, line.strip(), 0)
                        )
                        summary.upstream += 1
        store.verify_locked()
    return summary


def _adopt_blob(store, site_dir, checksum):
    if not SHA256_HEX.fullmatch(checksum):
        raise IntegrityError(f"index checksum is not lowercase SHA-256 hex: {checksum}")
    present = store.blob_path(checksum).exists()
    source = site_dir / "blobs" / f"{checksum}.crate"
    try:
        handle = open(source, "rb")
    except OSError as error:
        raise IntegrityError(f"missing blob {source}: {error}") from error
    with handle:
        actual = store.ingest_blob(handle)
    if actual != checksum:
        raise IntegrityError(f"blob {source} hashes to {actual}, not the index checksum")
    return 0 if present else 1


def _current_generation_dir(site_dir):
    current = site_dir / "current"
    try:
        target = os.readlink(current)
    except OSError as error:
        raise InputError(f"{current} is not a current symlink: {error}") from error
    match = GENERATION_DIRECTORY.fullmatch(target)
    if not match:
        raise IntegrityError(f"unsafe current target: {target}")
    generation = site_dir / "generations" / match.group(1)
    if not generation.is_dir():
        raise IntegrityError(f"current generation is missing: {generation}")
    return generation


def _index_files(root):
    files = []
    pending = [root]
    while pending:
        directory = pending.pop()
        for entry in list_dir(directory):
            path = Path(entry.path)
            if entry.is_symlink():
                raise IntegrityError(f"unexpected index entry: {path}")
            if entry.is_dir():
                pending.append(path)
            elif entry.is_file():
                if not (directory == root and entry.name == "config.json"):
                    files.append(path)
            else:
                raise IntegrityError(f"unexpected index entry: {path}")
    files.sort()
    return files
