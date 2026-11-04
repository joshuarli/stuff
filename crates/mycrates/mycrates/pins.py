from dataclasses import dataclass, field
from pathlib import Path

from . import wirejson
from .errors import InputError


@dataclass
class PinEntry:
    name: str
    version: str
    checksum: str
    source: str


@dataclass
class Pins:
    project: str | None = None
    lock_path: str | None = None
    lock_content: str | None = None
    registry: list = field(default_factory=list)
    unsupported: list = field(default_factory=list)

    @classmethod
    def from_obj(cls, obj):
        if not isinstance(obj, dict):
            raise InputError("not an object")
        try:
            registry = [PinEntry(p["name"], p["version"], p["checksum"], p["source"]) for p in obj.get("registry", [])]
        except (KeyError, TypeError) as error:
            raise InputError(f"bad registry pin ({error})") from error
        pins = cls(
            project=obj.get("project"),
            lock_path=obj.get("lock_path"),
            lock_content=obj.get("lock_content"),
            registry=registry,
            unsupported=list(obj.get("unsupported", [])),
        )
        text_fields = [pins.project, pins.lock_path, pins.lock_content]
        if any(f is not None and not isinstance(f, str) for f in text_fields) or not all(
            isinstance(v, str) for pin in registry for v in (pin.name, pin.version, pin.checksum, pin.source)
        ):
            raise InputError("a field has the wrong type")
        return pins

    def to_obj(self):
        out = {}
        for key in ("project", "lock_path", "lock_content"):
            value = getattr(self, key)
            if value is not None:
                out[key] = value
        out["registry"] = [
            {"name": p.name, "version": p.version, "checksum": p.checksum, "source": p.source} for p in self.registry
        ]
        out["unsupported"] = list(self.unsupported)
        return out


def read_pins(path):
    path = Path(path)
    try:
        return Pins.from_obj(wirejson.loads(path.read_text(encoding="utf-8")))
    except (InputError, UnicodeDecodeError) as error:
        message = error.message if isinstance(error, InputError) else str(error)
        raise InputError(f"failed to parse {path}: {message}") from error


def write_pins(path, pins):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(wirejson.pretty(pins.to_obj()), encoding="utf-8")
