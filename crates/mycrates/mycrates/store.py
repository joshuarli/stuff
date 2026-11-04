import hashlib
import os
import threading
import time
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path

from . import wirejson
from .errors import ConflictError, InputError, IntegrityError
from .fsutil import exclusive_lock, list_dir, sha256_file, sync_dir, unique_suffix, write_file_durable
from .model import IndexEntry
from .names import lower_name, normalized_key, validate_crate_name
from .semver import Version

STALE_UPLOAD_AGE = 3600
TEMP_PREFIX = ".tmp"
READ_CHUNK = 65536


@dataclass
class NativeRecord:
    entry: IndexEntry


@dataclass
class UpstreamRecord:
    name: str
    vers: str
    checksum: str
    raw_index_line: str
    acquired_at: int

    @classmethod
    def from_obj(cls, obj):
        if not isinstance(obj, dict):
            raise InputError("upstream record must be an object")
        try:
            record = cls(obj["name"], obj["vers"], obj["checksum"], obj["raw_index_line"], obj["acquired_at"])
        except KeyError as error:
            raise InputError(f"upstream record is missing {error.args[0]}") from error
        text_fields = (record.name, record.vers, record.checksum, record.raw_index_line)
        if not all(isinstance(f, str) for f in text_fields) or not isinstance(record.acquired_at, int):
            raise InputError("upstream record has a field of the wrong type")
        return record

    def to_obj(self):
        return {
            "name": self.name,
            "vers": self.vers,
            "checksum": self.checksum,
            "raw_index_line": self.raw_index_line,
            "acquired_at": self.acquired_at,
        }


def native_record_obj(entry):
    return {
        "name": entry.name,
        "vers": entry.vers,
        "checksum": entry.cksum,
        "yanked": entry.yanked,
        "entry": entry.to_obj(),
    }


def native_record_from_obj(obj, path):
    try:
        entry = IndexEntry.from_obj(obj["entry"])
        for field, entry_value in (
            ("name", entry.name),
            ("vers", entry.vers),
            ("checksum", entry.cksum),
            ("yanked", entry.yanked),
        ):
            if obj[field] != entry_value:
                raise IntegrityError(f"native record {path} disagrees with its index entry on {field}")
    except KeyError as error:
        raise IntegrityError(f"invalid native record {path}: missing {error.args[0]}") from error
    except InputError as error:
        raise IntegrityError(f"invalid native record {path}: {error.message}") from error
    return NativeRecord(entry)


def _is_temporary(name):
    return name.startswith(TEMP_PREFIX)


class Store:
    def __init__(self, state_dir):
        self.state_dir = Path(state_dir)
        self._collision_index = {}
        self._collision_mutex = threading.Lock()

    @property
    def blobs_dir(self):
        return self.state_dir / "blobs"

    @property
    def native_dir(self):
        return self.state_dir / "native"

    @property
    def upstream_dir(self):
        return self.state_dir / "upstream"

    @property
    def pins_dir(self):
        return self.state_dir / "pins"

    @property
    def uploads_dir(self):
        return self.state_dir / "uploads"

    @property
    def lock_path(self):
        return self.state_dir / "lock"

    def blob_path(self, checksum):
        return self.blobs_dir / f"{checksum}.crate"

    def ensure_dirs(self):
        for directory in (self.blobs_dir, self.native_dir, self.upstream_dir, self.pins_dir, self.uploads_dir):
            directory.mkdir(parents=True, exist_ok=True)

    @contextmanager
    def lock_exclusive(self):
        self.ensure_dirs()
        with exclusive_lock(self.lock_path):
            self.recover_staging()
            self.recover_uploads()
            yield

    def recover_staging(self):
        if self.blobs_dir.exists():
            for entry in list_dir(self.blobs_dir):
                if _is_temporary(entry.name):
                    Path(entry.path).unlink(missing_ok=True)

    def recover_uploads(self):
        if self.uploads_dir.exists():
            now = time.time()
            for entry in list_dir(self.uploads_dir):
                if entry.is_file(follow_symlinks=False):
                    try:
                        age = now - entry.stat(follow_symlinks=False).st_mtime
                    except OSError:
                        continue
                    if age >= STALE_UPLOAD_AGE:
                        Path(entry.path).unlink(missing_ok=True)

    def assert_current_format(self):
        legacy = []
        if self.pins_dir.exists():
            legacy.extend(self.pins_dir.glob("*.toml"))
        if self.native_dir.exists():
            legacy.extend(self.native_dir.glob("*/*.toml"))
        if legacy:
            raise InputError(
                f"{self.state_dir} holds {len(legacy)} TOML records from the Rust mycrates; "
                "run `mycrates migrate-state` once to convert them to JSON"
            )

    def check_name_collision(self, directory, name):
        if not directory.exists():
            return
        wanted = normalized_key(name)
        with self._collision_mutex:
            if directory not in self._collision_index:
                by_norm = {}
                for entry in list_dir(directory):
                    if entry.is_dir(follow_symlinks=False):
                        by_norm.setdefault(normalized_key(entry.name), []).append(Path(entry.path))
                self._collision_index[directory] = by_norm
            matching = list(self._collision_index[directory].get(wanted, ()))
        for crate_dir in matching:
            canonicals = _canonical_names_in(crate_dir)
            if canonicals is None:
                if crate_dir.name != lower_name(name):
                    raise ConflictError(f"crate name collision: '{name}' conflicts with existing '{crate_dir.name}'")
            elif name not in canonicals:
                raise ConflictError(f"crate name collision: '{name}' conflicts with existing '{canonicals[0]}'")

    def _remember_crate_dir(self, directory, name, crate_dir):
        with self._collision_mutex:
            by_norm = self._collision_index.get(directory)
            if by_norm is not None:
                entries = by_norm.setdefault(normalized_key(name), [])
                if crate_dir not in entries:
                    entries.append(crate_dir)

    def ingest_staged_file(self, path, expected_sha256):
        self.ensure_dirs()
        actual = sha256_file(path)
        if actual != expected_sha256:
            raise IntegrityError(
                f"staged file checksum changed before ingest: expected {expected_sha256}, got {actual}"
            )
        with open(path, "rb") as source:
            return self.ingest_blob(source)

    def ingest_blob(self, reader):
        self.ensure_dirs()
        tmp = self.blobs_dir / f".tmp.{unique_suffix()}.crate"
        digest = hashlib.sha256()
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o666)
        try:
            with os.fdopen(fd, "wb") as handle:
                while True:
                    chunk = reader.read(READ_CHUNK)
                    if not chunk:
                        break
                    digest.update(chunk)
                    handle.write(chunk)
                handle.flush()
                os.fsync(handle.fileno())
            checksum = digest.hexdigest()
            final = self.blob_path(checksum)
            if final.exists():
                tmp.unlink(missing_ok=True)
                return checksum
            os.replace(tmp, final)
        except BaseException:
            tmp.unlink(missing_ok=True)
            raise
        sync_dir(self.blobs_dir)
        return checksum

    def _version_files(self, crate_dir, suffix):
        if not crate_dir.exists():
            return []
        return [
            Path(entry.path)
            for entry in list_dir(crate_dir)
            if entry.name.endswith(suffix) and not _is_temporary(entry.name) and entry.is_file(follow_symlinks=False)
        ]

    def put_native_record(self, record):
        self.ensure_dirs()
        self.assert_current_format()
        entry = record.entry
        validate_crate_name(entry.name)
        version = Version.parse(entry.vers)
        self.check_name_collision(self.native_dir, entry.name)
        crate_dir = self.native_dir / lower_name(entry.name)
        crate_dir.mkdir(parents=True, exist_ok=True)
        self._remember_crate_dir(self.native_dir, entry.name, crate_dir)

        for existing_path in self._version_files(crate_dir, ".json"):
            if existing_path.stem == str(version):
                continue
            existing = self._read_native(existing_path)
            try:
                existing_version = Version.parse(existing.entry.vers)
            except InputError:
                continue
            if existing_version.uniqueness_key() == version.uniqueness_key():
                raise ConflictError(
                    f"version {entry.vers} conflicts with existing {existing.entry.vers} "
                    "(build metadata ignored for identity)"
                )

        record_path = crate_dir / f"{version}.json"
        if record_path.exists():
            existing = self._read_native(record_path)
            if existing.entry.cksum == entry.cksum:
                return False
            raise ConflictError(f"version {entry.vers} already exists with different checksum")
        write_file_durable(record_path, wirejson.pretty(native_record_obj(entry)).encode("utf-8"))
        return True

    def _read_native(self, path):
        try:
            obj = wirejson.loads(Path(path).read_text(encoding="utf-8"))
        except (InputError, UnicodeDecodeError) as error:
            raise IntegrityError(f"invalid native record {path}: {error}") from error
        if not isinstance(obj, dict):
            raise IntegrityError(f"invalid native record {path}: not an object")
        return native_record_from_obj(obj, path)

    def set_native_yank(self, name, vers, yanked):
        validate_crate_name(name)
        version = Version.parse(vers)
        record_path = self.native_dir / lower_name(name) / f"{version}.json"
        if not record_path.exists():
            raise InputError(f"crate {name} version {vers} not found")
        record = self._read_native(record_path)
        record.entry.yanked = yanked
        write_file_durable(record_path, wirejson.pretty(native_record_obj(record.entry)).encode("utf-8"))

    def put_upstream_record(self, record):
        self.ensure_dirs()
        validate_crate_name(record.name)
        version = Version.parse(record.vers)
        self.check_name_collision(self.upstream_dir, record.name)
        crate_dir = self.upstream_dir / lower_name(record.name)
        crate_dir.mkdir(parents=True, exist_ok=True)
        self._remember_crate_dir(self.upstream_dir, record.name, crate_dir)

        for existing_path in self._version_files(crate_dir, ".json"):
            if existing_path.stem == str(version):
                continue
            existing = self._read_upstream(existing_path)
            try:
                existing_version = Version.parse(existing.vers)
            except InputError:
                continue
            if existing_version.uniqueness_key() == version.uniqueness_key():
                raise ConflictError(
                    f"upstream version {record.vers} conflicts with existing {existing.vers} "
                    "(build metadata ignored for identity)"
                )

        record_path = crate_dir / f"{version}.json"
        if record_path.exists():
            existing = self._read_upstream(record_path)
            if existing.checksum == record.checksum:
                return False
            raise ConflictError(f"upstream version {record.vers} already exists with different checksum")
        write_file_durable(record_path, wirejson.pretty(record.to_obj()).encode("utf-8"))
        return True

    def _read_upstream(self, path):
        try:
            return UpstreamRecord.from_obj(wirejson.loads(Path(path).read_text(encoding="utf-8")))
        except (InputError, UnicodeDecodeError) as error:
            raise IntegrityError(f"invalid upstream record {path}: {error}") from error

    def load_native_records(self):
        self.assert_current_format()
        records = []
        if not self.native_dir.exists():
            return records
        for crate_dir in list_dir(self.native_dir):
            if not crate_dir.is_dir(follow_symlinks=False):
                continue
            for path in self._version_files(Path(crate_dir.path), ".json"):
                records.append(self._read_native(path))
        records.sort(key=lambda r: (r.entry.name, r.entry.vers))
        return records

    def load_upstream_records(self):
        records = []
        if not self.upstream_dir.exists():
            return records
        for crate_dir in list_dir(self.upstream_dir):
            if not crate_dir.is_dir(follow_symlinks=False):
                continue
            for path in self._version_files(Path(crate_dir.path), ".json"):
                record = self._read_upstream(path)
                if record.name.lower() != crate_dir.name:
                    raise IntegrityError(f"upstream record {path} in wrong directory {crate_dir.name}")
                records.append(record)
        records.sort(key=lambda r: (r.name, r.vers))
        return records

    def verify(self):
        with self.lock_exclusive():
            self.verify_locked()

    def verify_locked(self):
        if self.blobs_dir.exists():
            for entry in list_dir(self.blobs_dir):
                if entry.name.endswith(".crate") and not _is_temporary(entry.name) and entry.is_file(
                    follow_symlinks=False
                ):
                    expected = entry.name[: -len(".crate")]
                    actual = sha256_file(entry.path)
                    if actual != expected:
                        raise IntegrityError(f"corrupted blob: {entry.path} (expected {expected}, got {actual})")
        for record in self.load_native_records():
            path = self.blob_path(record.entry.cksum)
            if not path.exists():
                raise IntegrityError(
                    f"missing blob for native crate {record.entry.name} {record.entry.vers}: {path}"
                )
        for record in self.load_upstream_records():
            path = self.blob_path(record.checksum)
            if not path.exists():
                raise IntegrityError(f"missing blob for upstream crate {record.name} {record.vers}: {path}")


def _canonical_names_in(crate_dir):
    names = []
    for entry in list_dir(crate_dir):
        if not entry.is_file(follow_symlinks=False) or _is_temporary(entry.name) or not entry.name.endswith(".json"):
            continue
        try:
            value = wirejson.loads(Path(entry.path).read_text(encoding="utf-8"))
        except (InputError, UnicodeDecodeError):
            continue
        if isinstance(value, dict) and isinstance(value.get("name"), str):
            names.append(value["name"])
    return names or None
