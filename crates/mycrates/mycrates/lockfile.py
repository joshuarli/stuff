import tomllib
from dataclasses import dataclass

from .errors import InputError

SUPPORTED_LOCK_VERSIONS = (3, 4)
CRATES_IO_INDEX_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"


@dataclass(frozen=True)
class LockPackage:
    name: str
    version: str
    source: str | None
    checksum: str | None


@dataclass(frozen=True)
class Lockfile:
    version: int | None
    packages: list


def parse_lockfile(text):
    try:
        parsed = tomllib.loads(text)
    except tomllib.TOMLDecodeError as error:
        raise InputError(str(error)) from error
    version = parsed.get("version")
    if version is not None and (isinstance(version, bool) or not isinstance(version, int)):
        raise InputError("lock version must be an integer")
    if version is not None and version not in SUPPORTED_LOCK_VERSIONS:
        raise InputError(f"unknown lock version {version}")
    packages = []
    for raw in parsed.get("package", []):
        if not isinstance(raw, dict) or not isinstance(raw.get("name"), str) or not isinstance(raw.get("version"), str):
            raise InputError("lock package needs a string name and version")
        source, checksum = raw.get("source"), raw.get("checksum")
        if any(v is not None and not isinstance(v, str) for v in (source, checksum)):
            raise InputError(f"lock package {raw['name']} has a non-string source or checksum")
        packages.append(LockPackage(raw["name"], raw["version"], source, checksum))
    return Lockfile(version, packages)


def is_crates_io_source(source):
    return source is not None and source.startswith("registry+") and (
        "index.crates.io" in source or source == CRATES_IO_INDEX_SOURCE
    )


def classify(package):
    source = package.source
    if source is None:
        return "workspace"
    if source.startswith("git+"):
        return "git"
    if source.startswith("registry+"):
        return "crates-io" if is_crates_io_source(source) else "alternate-registry"
    if source.startswith("path+") or "(path+" in source:
        return "path"
    return "unsupported"
