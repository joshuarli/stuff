import json
from dataclasses import asdict, dataclass, field, fields
from pathlib import Path

from .errors import InputError, OperationError


@dataclass
class UpstreamConfig:
    index_url: str = "https://index.crates.io/"


@dataclass
class LimitsConfig:
    max_compressed: int = 1024 * 1024 * 1024
    max_expanded: int = 4 * 1024 * 1024 * 1024
    max_entries: int = 200_000
    max_ratio: int = 1000
    concurrent_requests: int = 4
    fetch_bytes: int = 10 * 1024 * 1024 * 1024
    fetch_timeout_secs: int = 3600
    request_timeout_secs: int = 300
    redirects: int = 5
    serve_connections: int = 64
    serve_writes: int = 4
    upload_timeout_secs: int = 300


@dataclass
class Config:
    version: int = 1
    public_base_url: str = "http://localhost:8080"
    upstream: UpstreamConfig = field(default_factory=UpstreamConfig)
    limits: LimitsConfig = field(default_factory=LimitsConfig)
    auth_required: bool = False

    @classmethod
    def load(cls, path):
        path = Path(path)
        if not path.exists():
            raise InputError(f"{path} does not exist; run `mycrates init` for this state directory first")
        try:
            obj = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError) as error:
            raise InputError(f"{path}: {error}") from error
        config = _build(cls, obj, "config")
        if config.version != 1:
            raise InputError(f"unsupported config version {config.version}")
        return config

    def save(self, path):
        path = Path(path)
        try:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(json.dumps(asdict(self), indent=2), encoding="utf-8")
        except OSError as error:
            raise OperationError(str(error)) from error


def _build(kind, obj, context):
    if not isinstance(obj, dict):
        raise InputError(f"{context} must be an object")
    known = {f.name: f for f in fields(kind)}
    unknown = sorted(set(obj) - set(known))
    if unknown:
        raise InputError(f"{context} has unknown field {unknown[0]}")
    values = {}
    for name, value in obj.items():
        default = getattr(kind(), name)
        if hasattr(default, "__dataclass_fields__"):
            values[name] = _build(type(default), value, f"{context}.{name}")
        else:
            if isinstance(default, bool) != isinstance(value, bool) or not isinstance(value, type(default)):
                raise InputError(f"{context}.{name} has the wrong type")
            values[name] = value
    return kind(**values)
