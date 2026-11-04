import tomllib
from dataclasses import dataclass

from . import wirejson
from .errors import ConflictError, InputError, IntegrityError
from .fsutil import write_file_durable
from .model import IndexEntry
from .pins import Pins, read_pins
from .store import Store, native_record_from_obj, native_record_obj


@dataclass
class MigrationSummary:
    native: int = 0
    pins: int = 0


def migrate_state(state_dir):
    store = Store(state_dir)
    summary = MigrationSummary()
    with store.lock_exclusive():
        for path in sorted(store.native_dir.glob("*/*.toml")):
            _migrate_native(path)
            summary.native += 1
        for path in sorted(store.pins_dir.glob("*.toml")):
            _migrate_pins(path)
            summary.pins += 1
    return summary


def _load_toml(path):
    try:
        return tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise IntegrityError(f"cannot read legacy record {path}: {error}") from error


def _migrate_native(path):
    obj = _load_toml(path)
    try:
        entry = IndexEntry.from_obj(obj["entry"])
    except (KeyError, InputError) as error:
        raise IntegrityError(f"invalid legacy native record {path}: {error}") from error
    migrated = native_record_obj(entry)
    for field in ("name", "vers", "checksum", "yanked"):
        if obj.get(field) != migrated[field]:
            raise IntegrityError(f"legacy native record {path} disagrees with its index entry on {field}")
    native_record_from_obj(migrated, path)
    _replace(path, wirejson.pretty(migrated))


def _migrate_pins(path):
    obj = _load_toml(path)
    pins = Pins.from_obj(obj)
    _replace(path, wirejson.pretty(pins.to_obj()))
    read_pins(path.with_suffix(".json"))


def _replace(legacy_path, text):
    target = legacy_path.with_suffix(".json") if legacy_path.suffix == ".toml" else legacy_path
    data = text.encode("utf-8")
    if target.exists():
        if target.read_bytes() != data:
            raise ConflictError(f"{target} already exists and differs from the migrated {legacy_path}")
    else:
        write_file_durable(target, data)
    legacy_path.unlink()

