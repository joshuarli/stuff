from __future__ import annotations

import io
import json
import re
import subprocess
import tarfile
import tomllib
import urllib.request
from dataclasses import dataclass, replace
from datetime import datetime
from pathlib import Path
from typing import Any, cast

from repomod.catalog import atomic_write

_SHA256 = re.compile(r"sha256:[0-9a-f]{64}\Z")
_ARCHIVE_SHA256 = re.compile(r"[0-9a-f]{64}\Z")
_UPSTREAM_VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+\Z")
_STABLE_BRANCH = re.compile(r"[0-9]+\.[0-9]+\Z")
_UPSTREAM_RELEASE = re.compile(
    r'href="releases/musl-(?P<version>[0-9]+\.[0-9]+\.[0-9]+)\.tar\.gz"[^>]*>[^<]*</a>'
    r'.{0,400}?-\s*(?P<released>[A-Z][a-z]+ [0-9]{1,2}, [0-9]{4})',
    re.DOTALL,
)
_ARCHES = ("x86_64", "aarch64")
_MANIFEST_ARCHES = ("amd64", "arm64")
_CHANNEL_NAMES = ("stable", "edge")
_TOOLS = ("llvm", "mold", "rustup")
_ADAPTER_FIELDS = ("kind", "channel", "lock", "workflow", "dockerfile", "zig_version", "zig_archive_sha256")
_ADAPTER_KINDS = {
    "python-build": ("lock",),
    "chromium-build": ("lock",),
    "node-build": ("workflow",),
    "git-build": ("dockerfile",),
    "zzz": ("zig_version", "zig_archive_sha256"),
}
_MUSL_RELEASES_URL = "https://musl.libc.org/releases.html"
_ALPINE_CDN = "https://dl-cdn.alpinelinux.org/alpine"


@dataclass(frozen=True, slots=True)
class Archive:
    url: str
    sha256: str


@dataclass(frozen=True, slots=True)
class Channel:
    branch: str
    image: str
    index_digest: str
    release: str
    manifests: dict[str, str]
    packages: dict[str, dict[str, str]]


@dataclass(frozen=True, slots=True)
class Toolchain:
    llvm_version: str
    llvm_release: str
    mold_version: str
    mold_release: str
    rustup_version: str
    archives: dict[str, dict[str, Archive]]


@dataclass(frozen=True, slots=True)
class Adapter:
    kind: str
    channel: str | None = None
    lock: str | None = None
    workflow: str | None = None
    dockerfile: str | None = None
    zig_version: str | None = None
    zig_archive_sha256: str | None = None


@dataclass(frozen=True, slots=True)
class Image:
    reference: str
    digest: str


@dataclass(frozen=True, slots=True)
class MuslToolchain:
    upstream_version: str
    upstream_released: str
    channels: dict[str, Channel]
    toolchain: Toolchain
    adapters: dict[str, Adapter]
    image: Image | None

    def channel(self, name: str) -> Channel:
        try:
            return self.channels[name]
        except KeyError as error:
            raise ValueError(f"unknown musl channel: {name}") from error


@dataclass(frozen=True, slots=True)
class ImageDigests:
    index: str
    manifests: dict[str, str]


def musl_toolchain_file() -> Path:
    return Path(__file__).resolve().parents[3] / "musl-toolchain.toml"


def _object(value: Any, key: str) -> dict[str, Any]:
    if not isinstance(value, dict) or not all(isinstance(name, str) for name in value):
        raise ValueError(f"{key} must be a table")
    return value


def _exact_keys(value: dict[str, Any], keys: set[str], key: str) -> None:
    missing = sorted(keys - set(value))
    if missing:
        raise ValueError(f"{key} is missing required keys: {', '.join(missing)}")
    unknown = sorted(set(value) - keys)
    if unknown:
        raise ValueError(f"{key} has unknown keys: {', '.join(unknown)}")


def _string(value: Any, key: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{key} must be a non-empty string")
    return value


def _sha256_pin(value: Any, key: str) -> str:
    if not isinstance(value, str) or _SHA256.fullmatch(value) is None:
        raise ValueError(f"{key} must be a sha256: digest with 64 lowercase hex characters")
    return value


def _archive_sha256(value: Any, key: str) -> str:
    if not isinstance(value, str) or _ARCHIVE_SHA256.fullmatch(value.casefold()) is None:
        raise ValueError(f"{key} must be a 64-hex sha256 checksum")
    return cast(str, value.casefold())


def _relative_path(value: Any, key: str) -> str:
    path = _string(value, key)
    candidate = Path(path)
    if candidate.is_absolute() or ".." in candidate.parts or "\\" in path:
        raise ValueError(f"{key} must be a repository-relative path")
    return path


def _date(value: Any, key: str) -> str:
    text = _string(value, key)
    try:
        return datetime.strptime(text, "%Y-%m-%d").date().isoformat()
    except ValueError as error:
        raise ValueError(f"{key} must be an ISO date") from error


def _channel(
    value: dict[str, Any], name: str, upstream: str
) -> Channel:
    _exact_keys(value, {"branch", "image", "index_digest", "release", "manifests", "packages"}, f"channels.{name}")
    branch = _string(value["branch"], f"channels.{name}.branch")
    if name == "stable":
        if _STABLE_BRANCH.fullmatch(branch) is None:
            raise ValueError(f"channels.stable.branch must be a major.minor Alpine branch, got {branch!r}")
    elif branch != "edge":
        raise ValueError(f"channels.edge.branch must be edge, got {branch!r}")
    manifests = _object(value["manifests"], f"channels.{name}.manifests")
    _exact_keys(manifests, set(_MANIFEST_ARCHES), f"channels.{name}.manifests")
    parsed_manifests = {
        arch: _sha256_pin(manifests[arch], f"channels.{name}.manifests.{arch}") for arch in _MANIFEST_ARCHES
    }
    packages = _object(value["packages"], f"channels.{name}.packages")
    _exact_keys(packages, set(_ARCHES), f"channels.{name}.packages")
    parsed_packages: dict[str, dict[str, str]] = {}
    for arch in _ARCHES:
        table = _object(packages[arch], f"channels.{name}.packages.{arch}")
        if not table:
            raise ValueError(f"channels.{name}.packages.{arch} must approve at least one package")
        parsed_packages[arch] = {
            package: _string(version, f"channels.{name}.packages.{arch}.{package}")
            for package, version in table.items()
        }
        musl = parsed_packages[arch].get("musl")
        musl_dev = parsed_packages[arch].get("musl-dev")
        if musl is None or musl_dev is None:
            raise ValueError(f"channels.{name}.packages.{arch} must pin musl and musl-dev")
        if musl != musl_dev:
            raise ValueError(
                f"channels.{name}.packages.{arch} pins musl {musl} but musl-dev {musl_dev}"
            )
        if not (musl == upstream or musl.startswith(f"{upstream}-")):
            raise ValueError(
                f"channels.{name}.packages.{arch} musl {musl} does not match upstream musl {upstream}"
            )
    return Channel(
        branch=branch,
        image=_string(value["image"], f"channels.{name}.image"),
        index_digest=_sha256_pin(value["index_digest"], f"channels.{name}.index_digest"),
        release=_string(value["release"], f"channels.{name}.release"),
        manifests=parsed_manifests,
        packages=parsed_packages,
    )


def _toolchain(value: dict[str, Any]) -> Toolchain:
    _exact_keys(
        value,
        {"llvm_version", "llvm_release", "mold_version", "mold_release", "rustup_version", "archives"},
        "toolchain",
    )
    archives = _object(value["archives"], "toolchain.archives")
    _exact_keys(archives, set(_TOOLS), "toolchain.archives")
    parsed_archives: dict[str, dict[str, Archive]] = {}
    for tool in _TOOLS:
        table = _object(archives[tool], f"toolchain.archives.{tool}")
        _exact_keys(table, set(_ARCHES), f"toolchain.archives.{tool}")
        parsed_archives[tool] = {}
        for arch in _ARCHES:
            entry = _object(table[arch], f"toolchain.archives.{tool}.{arch}")
            _exact_keys(entry, {"url", "sha256"}, f"toolchain.archives.{tool}.{arch}")
            url = _string(entry["url"], f"toolchain.archives.{tool}.{arch}.url")
            if not url.startswith("https://") or any(character.isspace() for character in url):
                raise ValueError(f"toolchain.archives.{tool}.{arch}.url must be an https URL")
            parsed_archives[tool][arch] = Archive(
                url=url,
                sha256=_archive_sha256(entry["sha256"], f"toolchain.archives.{tool}.{arch}.sha256"),
            )
    return Toolchain(
        llvm_version=_string(value["llvm_version"], "toolchain.llvm_version"),
        llvm_release=_string(value["llvm_release"], "toolchain.llvm_release"),
        mold_version=_string(value["mold_version"], "toolchain.mold_version"),
        mold_release=_string(value["mold_release"], "toolchain.mold_release"),
        rustup_version=_string(value["rustup_version"], "toolchain.rustup_version"),
        archives=parsed_archives,
    )


def _adapters(value: dict[str, Any], channels: dict[str, Channel]) -> dict[str, Adapter]:
    parsed: dict[str, Adapter] = {}
    for name, raw in value.items():
        if not isinstance(name, str) or name.count("/") != 1 or any(not part for part in name.split("/")):
            raise ValueError(f"adapter {name!r} must be a repository id")
        entry = _object(raw, f"adapters.{name}")
        unknown = sorted(set(entry) - set(_ADAPTER_FIELDS))
        if unknown:
            raise ValueError(f"adapters.{name} has unknown keys: {', '.join(unknown)}")
        kind = _string(entry.get("kind"), f"adapters.{name}.kind")
        required = _ADAPTER_KINDS.get(kind)
        if required is None:
            raise ValueError(f"adapters.{name}.kind must be one of {', '.join(sorted(_ADAPTER_KINDS))}")
        fields: dict[str, str | None] = {field: None for field in _ADAPTER_FIELDS if field != "kind"}
        for field in required:
            if field not in entry:
                raise ValueError(f"adapters.{name} is missing {field}")
        if kind != "zzz":
            channel = _string(entry.get("channel"), f"adapters.{name}.channel")
            if channel not in channels:
                raise ValueError(f"adapters.{name}.channel is not a configured channel: {channel}")
            fields["channel"] = channel
        elif "channel" in entry:
            raise ValueError(f"adapters.{name} must not declare a channel")
        if "lock" in entry:
            fields["lock"] = _relative_path(entry["lock"], f"adapters.{name}.lock")
        if "workflow" in entry:
            fields["workflow"] = _relative_path(entry["workflow"], f"adapters.{name}.workflow")
        if "dockerfile" in entry:
            fields["dockerfile"] = _relative_path(entry["dockerfile"], f"adapters.{name}.dockerfile")
        if "zig_version" in entry:
            fields["zig_version"] = _string(entry["zig_version"], f"adapters.{name}.zig_version")
        if "zig_archive_sha256" in entry:
            fields["zig_archive_sha256"] = _archive_sha256(
                entry["zig_archive_sha256"], f"adapters.{name}.zig_archive_sha256"
            )
        for field, field_value in fields.items():
            allowed = field in required or (field == "channel" and kind != "zzz")
            if not allowed and field_value is not None:
                raise ValueError(f"adapters.{name} must not declare {field} for kind {kind}")
        parsed[name] = Adapter(kind=kind, **fields)
    return parsed


def _image(value: dict[str, Any]) -> Image:
    _exact_keys(value, {"reference", "digest"}, "image")
    return Image(
        reference=_string(value["reference"], "image.reference"),
        digest=_sha256_pin(value["digest"], "image.digest"),
    )


def _parse_musl_toolchain(source: str) -> MuslToolchain:
    try:
        document = tomllib.loads(source)
    except tomllib.TOMLDecodeError as error:
        raise ValueError(f"musl-toolchain.toml is invalid TOML: {error}") from error
    _exact_keys(document, {"version", "upstream", "channels", "toolchain", "adapters"}, "musl-toolchain.toml")
    if type(document["version"]) is not int or document["version"] != 1:
        raise ValueError("musl-toolchain.toml version must be 1")
    upstream = _object(document["upstream"], "upstream")
    _exact_keys(upstream, {"version", "released"}, "upstream")
    upstream_version = _string(upstream["version"], "upstream.version")
    if _UPSTREAM_VERSION.fullmatch(upstream_version) is None:
        raise ValueError(f"upstream.version must be a release version, got {upstream_version!r}")
    upstream_released = _date(upstream["released"], "upstream.released")
    raw_channels = _object(document["channels"], "channels")
    _exact_keys(raw_channels, set(_CHANNEL_NAMES), "channels")
    channels = {
        name: _channel(_object(raw_channels[name], f"channels.{name}"), name, upstream_version)
        for name in _CHANNEL_NAMES
    }
    toolchain = _toolchain(_object(document["toolchain"], "toolchain"))
    adapters = _adapters(_object(document["adapters"], "adapters"), channels)
    image_value = document.get("image")
    image = None if image_value is None else _image(_object(image_value, "image"))
    return MuslToolchain(
        upstream_version=upstream_version,
        upstream_released=upstream_released,
        channels=channels,
        toolchain=toolchain,
        adapters=adapters,
        image=image,
    )


def load_musl_toolchain(path: Path | None = None) -> MuslToolchain:
    target = musl_toolchain_file() if path is None else path
    if target.is_symlink():
        raise ValueError(f"musl-toolchain.toml is a symlink: {target}")
    try:
        source = target.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        raise ValueError(f"cannot read musl-toolchain.toml at {target}: {error}") from error
    return _parse_musl_toolchain(source)


def _quoted(value: str) -> str:
    return json.dumps(value, ensure_ascii=False)


def _table(lines: list[str], header: str, fields: tuple[tuple[str, str], ...]) -> None:
    lines.append(f"[{header}]")
    lines.extend(f"{name} = {_quoted(value)}" for name, value in fields)
    lines.append("")


def _render_channel(lines: list[str], name: str, channel: Channel) -> None:
    _table(
        lines,
        f"channels.{name}",
        (
            ("branch", channel.branch),
            ("image", channel.image),
            ("index_digest", channel.index_digest),
            ("release", channel.release),
        ),
    )
    _table(
        lines,
        f"channels.{name}.manifests",
        tuple((arch, channel.manifests[arch]) for arch in _MANIFEST_ARCHES),
    )
    for arch in _ARCHES:
        _table(
            lines,
            f"channels.{name}.packages.{arch}",
            tuple((package, version) for package, version in channel.packages[arch].items()),
        )


def render_musl_toolchain(lock: MuslToolchain) -> str:
    lines = ["version = 1", ""]
    _table(
        lines,
        "upstream",
        (("version", lock.upstream_version), ("released", lock.upstream_released)),
    )
    for name in _CHANNEL_NAMES:
        _render_channel(lines, name, lock.channels[name])
    toolchain = lock.toolchain
    _table(
        lines,
        "toolchain",
        (
            ("llvm_version", toolchain.llvm_version),
            ("llvm_release", toolchain.llvm_release),
            ("mold_version", toolchain.mold_version),
            ("mold_release", toolchain.mold_release),
            ("rustup_version", toolchain.rustup_version),
        ),
    )
    for tool in _TOOLS:
        for arch in _ARCHES:
            archive = toolchain.archives[tool][arch]
            _table(
                lines,
                f"toolchain.archives.{tool}.{arch}",
                (("url", archive.url), ("sha256", archive.sha256)),
            )
    for name, adapter in lock.adapters.items():
        fields = tuple(
            (field, value)
            for field in _ADAPTER_FIELDS
            if (value := getattr(adapter, field)) is not None
        )
        _table(lines, f"adapters.{_quoted(name)}", fields)
    if not lock.adapters:
        lines.extend(["[adapters]", ""])
    if lock.image is not None:
        _table(lines, "image", (("reference", lock.image.reference), ("digest", lock.image.digest)))
    return "\n".join(lines).rstrip("\n") + "\n"


def _alpine_version_key(version: str) -> tuple[tuple[int, ...], int, str]:
    base, separator, revision = version.partition("-r")
    return (
        tuple(int(part) for part in re.findall(r"[0-9]+", base)),
        int(revision) if separator and revision.isdigit() else 0,
        base,
    )


def _parse_apkindex(source: str) -> dict[str, str]:
    packages: dict[str, str] = {}
    record: dict[str, str] = {}
    records = 0

    def commit() -> None:
        nonlocal records
        if not record:
            return
        names = {key for key in ("P", "V") if key in record}
        if names != {"P", "V"} or not record["P"] or not record["V"]:
            raise ValueError("APKINDEX contains a record without package name and version")
        package = record["P"]
        version = record["V"]
        previous = packages.get(package)
        if previous is None or _alpine_version_key(version) > _alpine_version_key(previous):
            packages[package] = version
        records += 1
        record.clear()

    for line in source.splitlines():
        if not line:
            commit()
            continue
        key, separator, value = line.partition(":")
        if key in {"P", "V", "S"} and separator:
            record[key] = value
    commit()
    if records == 0:
        raise ValueError("APKINDEX contains no package records")
    return packages


def _download(url: str) -> bytes:
    try:
        with urllib.request.urlopen(url, timeout=30) as response:
            return cast(bytes, response.read())
    except OSError as error:
        raise ValueError(f"cannot fetch {url}: {error}") from error


def _apkindex(package_repository: str, arch: str) -> dict[str, str]:
    url = f"{_ALPINE_CDN}/{package_repository}/{arch}/APKINDEX.tar.gz"
    data = _download(url)
    try:
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
            member = archive.extractfile("APKINDEX")
            if member is None:
                raise ValueError(f"{url} contains no APKINDEX")
            source = member.read().decode("utf-8")
    except (tarfile.TarError, OSError, UnicodeError) as error:
        raise ValueError(f"cannot read APKINDEX from {url}: {error}") from error
    return _parse_apkindex(source)


def _package_repository(branch: str) -> str:
    return "edge" if branch == "edge" else f"v{branch}"


def _channel_packages(branch: str, arch: str, approved: dict[str, str]) -> dict[str, str]:
    repository = _package_repository(branch)
    packages = _apkindex_merged(repository, arch, set(approved))
    refreshed: dict[str, str] = {}
    for package in approved:
        version = packages.get(package)
        if version is None:
            raise ValueError(f"Alpine {repository} {arch} has no package named {package}")
        refreshed[package] = version
    return refreshed


def _apkindex_merged(repository: str, arch: str, wanted: set[str]) -> dict[str, str]:
    merged: dict[str, str] = {}
    for component in ("main", "community"):
        source = _apkindex(f"{repository}/{component}", arch)
        for package in wanted:
            version = source.get(package)
            if version is not None and package not in merged:
                merged[package] = version
    return merged


def _docker(*arguments: str) -> str:
    try:
        result = subprocess.run(
            ["docker", *arguments],
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
    except FileNotFoundError as error:
        raise ValueError("docker is required to refresh the musl lock") from error
    except subprocess.TimeoutExpired as error:
        raise ValueError("docker timed out while refreshing the musl lock") from error
    except OSError as error:
        raise ValueError(f"cannot run docker: {error}") from error
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"docker exited {result.returncode}"
        raise ValueError(f"docker failed while refreshing the musl lock: {detail}")
    return result.stdout


def _inspect_image(image: str) -> ImageDigests:
    raw = _docker("buildx", "imagetools", "inspect", image, "--format", "{{json .}}")
    try:
        document = json.loads(raw)
    except json.JSONDecodeError as error:
        raise ValueError(f"docker returned invalid image metadata for {image}: {error}") from error
    manifest = document.get("manifest") if isinstance(document, dict) else None
    index = manifest.get("digest") if isinstance(manifest, dict) else None
    entries = manifest.get("manifests") if isinstance(manifest, dict) else None
    if not isinstance(index, str) or not isinstance(entries, list):
        raise ValueError(f"docker returned incomplete image metadata for {image}")
    manifests: dict[str, str] = {}
    for entry in entries:
        if not isinstance(entry, dict):
            continue
        platform = entry.get("platform")
        if not isinstance(platform, dict) or platform.get("os") != "linux":
            continue
        architecture = platform.get("architecture")
        digest = entry.get("digest")
        if architecture in _MANIFEST_ARCHES and isinstance(digest, str):
            manifests[architecture] = digest
    if set(manifests) != set(_MANIFEST_ARCHES):
        raise ValueError(f"docker did not report {', '.join(_MANIFEST_ARCHES)} manifests for {image}")
    return ImageDigests(
        index=_sha256_pin(index, f"docker index digest for {image}"),
        manifests={
            arch: _sha256_pin(manifests[arch], f"docker {arch} manifest for {image}")
            for arch in _MANIFEST_ARCHES
        },
    )


def _observe_release(image: str) -> str:
    output = _docker(
        "run",
        "--rm",
        "--platform",
        "linux/amd64",
        image,
        "cat",
        "/etc/alpine-release",
    ).strip()
    if not output or "\n" in output:
        raise ValueError(f"docker did not report one Alpine release for {image}")
    return output


def _latest_upstream_musl() -> tuple[str, str]:
    source = _download(_MUSL_RELEASES_URL).decode("utf-8", errors="replace")
    match = _UPSTREAM_RELEASE.search(source)
    if match is None:
        raise ValueError("musl release page has no parseable release")
    version = cast(str, match.group("version"))
    released = datetime.strptime(match.group("released"), "%B %d, %Y").date()
    return version, released.isoformat()


def _branch_observation(branch: str) -> str:
    image = f"alpine:{branch}"
    return f"{image} currently releases Alpine {_observe_release(image)}"


def _refreshed_channel(lock: MuslToolchain, name: str) -> Channel:
    channel = lock.channel(name)
    digests = _inspect_image(channel.image)
    release = _observe_release(channel.image)
    packages = {
        arch: _channel_packages(channel.branch, arch, channel.packages[arch]) for arch in _ARCHES
    }
    return replace(
        channel,
        index_digest=digests.index,
        release=release,
        manifests=digests.manifests,
        packages=packages,
    )


def musl_toolchain_refresh_plan(path: Path | None = None) -> tuple[Path, str | None, tuple[str, ...]]:
    target = musl_toolchain_file() if path is None else path
    lock = load_musl_toolchain(target)
    upstream_version, upstream_released = _latest_upstream_musl()
    channels = {name: _refreshed_channel(lock, name) for name in _CHANNEL_NAMES}
    refreshed = replace(
        lock,
        upstream_version=upstream_version,
        upstream_released=upstream_released,
        channels=channels,
    )
    source = render_musl_toolchain(refreshed)
    if _parse_musl_toolchain(source) != refreshed:
        raise ValueError("rendered musl lock does not round-trip")
    observations = (_branch_observation(channels["stable"].branch),)
    if source == target.read_text(encoding="utf-8"):
        return target, None, observations
    return target, source, observations


def refresh_musl_toolchain(path: Path | None = None) -> bool:
    target, source, _observations = musl_toolchain_refresh_plan(path)
    if source is None:
        return False
    atomic_write(target, source)
    return True
