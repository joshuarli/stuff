import gzip
import hashlib
import tarfile
import tomllib
import zlib
from dataclasses import dataclass, field

from .errors import InputError, IntegrityError, LimitError
from .requirements import canonical_requirement

READ_CHUNK = 65536
MAX_MANIFEST_BYTES = 1024 * 1024
ALLOWED_ENTRY_TYPES = (tarfile.REGTYPE, tarfile.AREGTYPE, tarfile.DIRTYPE)


@dataclass(frozen=True)
class ScanLimits:
    max_compressed: int = 1024 * 1024 * 1024
    max_expanded: int = 4 * 1024 * 1024 * 1024
    max_entries: int = 200_000
    max_ratio: int = 1000
    max_manifest_bytes: int = MAX_MANIFEST_BYTES

    @classmethod
    def from_config(cls, limits):
        return cls(
            max_compressed=limits.max_compressed,
            max_expanded=limits.max_expanded,
            max_entries=limits.max_entries,
            max_ratio=limits.max_ratio,
        )


@dataclass(frozen=True)
class ScanResult:
    checksum: str
    cargo_toml: str


def validate_archive_path(raw):
    traversal = IntegrityError("path traversal")
    if not raw or raw.startswith("/"):
        raise traversal
    parts = raw.split("/")
    if parts[-1] == "":
        parts.pop()
    if not parts or any(part in ("", ".", "..") for part in parts):
        raise traversal


def scan_crate_staged(reader, expected_len, max_compressed, expected_sha256, name, version, limits):
    if expected_len > max_compressed or expected_len > limits.max_compressed:
        raise LimitError("compressed size exceeded")
    digest = hashlib.sha256()
    counted = 0
    reader.seek(0)
    while True:
        chunk = reader.read(READ_CHUNK)
        if not chunk:
            break
        counted += len(chunk)
        if counted > max_compressed or counted > limits.max_compressed:
            raise LimitError("compressed size exceeded")
        digest.update(chunk)
    if counted != expected_len:
        raise IntegrityError("staged upload length mismatch")
    checksum = digest.hexdigest()
    if expected_sha256 is not None and expected_sha256 != checksum:
        raise IntegrityError("staged upload checksum mismatch")
    reader.seek(0)
    return _scan_stream(reader, name, version, limits, checksum, counted)


def _scan_stream(reader, name, version, limits, checksum, compressed_len):
    prefix = f"{name}-{version}/"
    manifest_path = f"{prefix}Cargo.toml"
    seen = set()
    entries_seen = 0
    expanded = 0
    cargo_toml = None
    try:
        with tarfile.open(fileobj=reader, mode="r|gz") as archive:
            for member in archive:
                entries_seen += 1
                if entries_seen > limits.max_entries:
                    raise LimitError("too many entries")
                if member.type not in ALLOWED_ENTRY_TYPES:
                    raise IntegrityError(f"unsupported entry type: {member.type!r}")
                path = member.name.replace("\\", "/")
                if member.isdir():
                    path += "/"
                validate_archive_path(path)
                if path in seen:
                    raise IntegrityError("duplicate archive path")
                seen.add(path)
                if not path.startswith(prefix):
                    raise IntegrityError("wrong prefix")
                expanded += member.size
                if expanded > limits.max_expanded:
                    raise LimitError("expanded size exceeded")
                if compressed_len > 0 and expanded // max(compressed_len, 1) > limits.max_ratio:
                    raise LimitError("expansion ratio exceeded")
                if path == manifest_path:
                    if cargo_toml is not None:
                        raise IntegrityError("duplicate Cargo.toml")
                    data = archive.extractfile(member).read(limits.max_manifest_bytes + 1)
                    if len(data) > limits.max_manifest_bytes:
                        raise LimitError("manifest too large")
                    try:
                        cargo_toml = data.decode("utf-8")
                    except UnicodeDecodeError as error:
                        raise IntegrityError(str(error)) from error
    except (tarfile.TarError, EOFError, zlib.error, gzip.BadGzipFile, OSError) as error:
        raise IntegrityError(str(error)) from error
    if cargo_toml is None:
        raise IntegrityError("missing Cargo.toml")
    parse_manifest_identity(cargo_toml, name, version)
    return ScanResult(checksum=checksum, cargo_toml=cargo_toml)


def _parse_toml(text):
    try:
        return tomllib.loads(text)
    except tomllib.TOMLDecodeError as error:
        raise IntegrityError(str(error)) from error


def parse_manifest_identity(manifest, name, version):
    parsed = _parse_toml(manifest)
    package = parsed.get("package")
    if not isinstance(package, dict):
        raise IntegrityError("missing package in Cargo.toml")
    if package.get("name") != name or package.get("version") != version:
        raise IntegrityError("name/version mismatch")
    return parsed


@dataclass
class PackagedDep:
    alias: str
    package: str | None
    version_req: str
    features: list
    optional: bool
    default_features: bool
    target: str | None
    kind: str | None
    registry: str | None


@dataclass
class PackagedManifest:
    name: str
    version: str
    links: str | None
    rust_version: str | None
    features: dict
    dependencies: list = field(default_factory=list)


DEPENDENCY_SECTIONS = (("dependencies", None), ("dev-dependencies", "dev"), ("build-dependencies", "build"))


def parse_packaged_manifest(manifest):
    parsed = _parse_toml(manifest)
    package = parsed.get("package")
    if not isinstance(package, dict):
        raise IntegrityError("missing package in Cargo.toml")
    name = package.get("name")
    if not isinstance(name, str):
        raise IntegrityError("missing package name")
    version = package.get("version")
    if not isinstance(version, str):
        raise IntegrityError("missing package version")
    links = package.get("links") if isinstance(package.get("links"), str) else None
    rust_version = package.get("rust-version") if isinstance(package.get("rust-version"), str) else None

    features = {}
    raw_features = parsed.get("features")
    if isinstance(raw_features, dict):
        for key, items in raw_features.items():
            if not isinstance(items, list):
                raise IntegrityError(f"feature '{key}' is not an array")
            if not all(isinstance(item, str) for item in items):
                raise IntegrityError(f"feature '{key}' has non-string entry")
            features[key] = list(items)

    dependencies = []
    for section, kind in DEPENDENCY_SECTIONS:
        _collect(parsed, section, kind, None, dependencies)
    targets = parsed.get("target")
    if isinstance(targets, dict):
        for target_spec, table in targets.items():
            if not isinstance(table, dict):
                raise IntegrityError("target entry is not a table")
            for section, kind in DEPENDENCY_SECTIONS:
                _collect(table, section, kind, target_spec, dependencies, scoped=True)
            for section in table:
                if section not in ("dependencies", "dev-dependencies", "build-dependencies"):
                    raise IntegrityError(f"unknown target section '{section}'")
    return PackagedManifest(name, version, links, rust_version, features, dependencies)


def _collect(container, section, forced_kind, target, out, scoped=False):
    table = container.get(section)
    if table is None:
        return
    if not isinstance(table, dict):
        label = f"target.{section}" if scoped else section
        raise IntegrityError(f"'{label}' is not a table")
    for alias, spec in table.items():
        out.append(_parse_dep_spec(alias, spec, forced_kind, target))


def _kind(kind):
    if kind in (None, "normal"):
        return None
    if kind in ("dev", "build"):
        return kind
    raise IntegrityError(f"unknown dependency kind '{kind}'")


def _parse_dep_spec(alias, spec, forced_kind, target):
    table = None
    if isinstance(spec, str):
        version_req = spec
    elif isinstance(spec, dict):
        table = spec
        version_req = spec.get("version") if isinstance(spec.get("version"), str) else "*"
    else:
        raise IntegrityError(f"dependency '{alias}' has invalid spec")
    package = None
    features = []
    optional = False
    default_features = True
    kind = _kind(forced_kind)
    registry = None
    if table is not None:
        if isinstance(table.get("package"), str):
            package = table["package"]
        if isinstance(table.get("features"), list):
            for item in table["features"]:
                if not isinstance(item, str):
                    raise IntegrityError(f"dependency '{alias}' has non-string feature")
                features.append(item)
        if isinstance(table.get("optional"), bool):
            optional = table["optional"]
        if isinstance(table.get("default-features"), bool):
            default_features = table["default-features"]
        if isinstance(table.get("kind"), str):
            if forced_kind is not None:
                raise IntegrityError(f"dependency '{alias}' sets kind inside target section")
            kind = _kind(table["kind"])
        if isinstance(table.get("registry"), str):
            registry = table["registry"]
        if isinstance(table.get("registry-index"), str):
            if registry is not None:
                raise IntegrityError(f"dependency '{alias}' sets both registry and registry-index")
            registry = table["registry-index"]
        if "git" in table or "path" in table:
            raise IntegrityError(f"dependency '{alias}' uses git/path source")
    if version_req != "*":
        try:
            canonical_requirement(version_req)
        except InputError as error:
            raise IntegrityError(f"dependency '{alias}' has invalid version_req '{version_req}'") from error
    return PackagedDep(alias, package, version_req, features, optional, default_features, target, kind, registry)

