from __future__ import annotations

import io
import json
import os
import re
import subprocess
import tarfile
import tomllib
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, replace
from datetime import datetime
from pathlib import Path
from typing import Any, cast

from repomod.catalog import atomic_write
from repomod.io import read_json_object
from repomod.rules.binary_release import _release_kind, has_binary_release_contract
from repomod.rules.workflows import read_workflow_sources
from repomod.types import Finding, Repository, Rule, TagPredicate, operational, violation

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
_GCC_PACKAGES = frozenset({"gcc", "g++", "build-base"})
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
    musl_sha256: str
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


def _schema_keys(value: dict[str, Any], required: set[str], optional: set[str], key: str) -> None:
    missing = sorted(required - set(value))
    if missing:
        raise ValueError(f"{key} is missing required keys: {', '.join(missing)}")
    unknown = sorted(set(value) - required - optional)
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
        {"llvm_version", "llvm_release", "mold_version", "mold_release", "rustup_version", "musl_sha256", "archives"},
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
        musl_sha256=_archive_sha256(value["musl_sha256"], "toolchain.musl_sha256"),
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
    _schema_keys(
        document,
        {"version", "upstream", "channels", "toolchain", "adapters"},
        {"image"},
        "musl-toolchain.toml",
    )
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
            ("musl_sha256", toolchain.musl_sha256),
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
    components = ("main", "community")
    with ThreadPoolExecutor(
        max_workers=len(components), thread_name_prefix="repomod-apkindex"
    ) as pool:
        futures = {
            component: pool.submit(_apkindex, f"{repository}/{component}", arch)
            for component in components
        }
        sources = {component: futures[component].result() for component in components}
    merged: dict[str, str] = {}
    for component in components:
        for package in wanted:
            version = sources[component].get(package)
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
    with ThreadPoolExecutor(
        max_workers=4, thread_name_prefix="repomod-musl-channel"
    ) as pool:
        digests_future = pool.submit(_inspect_image, channel.image)
        release_future = pool.submit(_observe_release, channel.image)
        package_futures = {
            arch: pool.submit(
                _channel_packages, channel.branch, arch, channel.packages[arch]
            )
            for arch in _ARCHES
        }
        digests = digests_future.result()
        release = release_future.result()
        packages = {arch: package_futures[arch].result() for arch in _ARCHES}
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
    with ThreadPoolExecutor(
        max_workers=len(_CHANNEL_NAMES) + 2, thread_name_prefix="repomod-musl"
    ) as pool:
        upstream_future = pool.submit(_latest_upstream_musl)
        channel_futures = {
            name: pool.submit(_refreshed_channel, lock, name) for name in _CHANNEL_NAMES
        }
        observation_future = pool.submit(
            _branch_observation, lock.channel("stable").branch
        )
        upstream_version, upstream_released = upstream_future.result()
        channels = {name: channel_futures[name].result() for name in _CHANNEL_NAMES}
        observations = (observation_future.result(),)
    refreshed = replace(
        lock,
        upstream_version=upstream_version,
        upstream_released=upstream_released,
        channels=channels,
    )
    source = render_musl_toolchain(refreshed)
    if _parse_musl_toolchain(source) != refreshed:
        raise ValueError("rendered musl lock does not round-trip")
    if source == target.read_text(encoding="utf-8"):
        return target, None, observations
    return target, source, observations


def refresh_musl_toolchain(path: Path | None = None) -> bool:
    target, source, _observations = musl_toolchain_refresh_plan(path)
    if source is None:
        return False
    atomic_write(target, source)
    return True


_RULE_ID = "linux-musl-toolchain"
_IGNORED_DIRECTORIES = frozenset(
    {
        ".git",
        ".work",
        ".venv",
        "build",
        "dist",
        "node_modules",
        "target",
        "testdata",
        "vendor",
    }
)
_DOCKERFILE_NAME = re.compile(r"(?:Dockerfile(?:\..+)?|.+\.Dockerfile)\Z")
_MUSL_TRIPLE = re.compile(r"\b(?:x86_64|aarch64)(?:-unknown)?-linux-musl\b")
_VERSION_TAG = re.compile(r"\d+\.\d+(?:\.\d+)?\Z")
_FROM = re.compile(
    r"^\s*FROM\s+(?P<options>(?:--\S+\s+)*)(?P<image>\S+)(?:\s+AS\s+(?P<alias>\S+))?\s*$",
    re.IGNORECASE,
)
_ARG = re.compile(r"^\s*ARG\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)(?:=(?P<value>.*))?\s*$", re.IGNORECASE)
_RUN = re.compile(r"^\s*RUN\b", re.IGNORECASE)
_VARIABLE = re.compile(r"\$\{(?P<braced>[A-Za-z_][A-Za-z0-9_]*)\}|\$(?P<bare>[A-Za-z_][A-Za-z0-9_]*)")
_DOCKER_REFERENCE = re.compile(
    r"(?:--file|(?<!\S)-f)[= ](?P<flag>[^\s'\"\\]+)"
    r"|(?m:^\s*file:\s*(?P<file>[^\s'\"#]+))"
)
_APK_URL = re.compile(r"https?://\S+\.apk\b")
_ZIG_URL = re.compile(r"https://ziglang\.org/download/(?P<version>[0-9][^/\s\"']*)/zig-[^\s\"']+\.tar\.xz")
_CHECKSUM_VERIFIED = ("sha256sum -c", "add --checksum=", "add --checksum ")


@dataclass(frozen=True, slots=True)
class _LogicalLine:
    text: str
    offsets: tuple[int, ...]
    line: int

    def span(self, start: int, end: int) -> tuple[int, int] | None:
        if start >= end:
            return None
        offsets = self.offsets[start:end]
        if not offsets or offsets[-1] - offsets[0] != end - start - 1:
            return None
        return offsets[0], offsets[-1] + 1


@dataclass(frozen=True, slots=True)
class _Stage:
    image: str
    tag: str | None
    digest: str | None
    platform: str | None
    channel: str | None
    exempt: bool


@dataclass(frozen=True, slots=True)
class _Token:
    text: str
    start: int
    end: int
    separator: bool


@dataclass(frozen=True, slots=True)
class _Package:
    name: str
    version: str | None
    start: int
    end: int
    raw: str


@dataclass(frozen=True, slots=True)
class _ApkCommand:
    packages: tuple[_Package, ...]
    repositories: tuple[str, ...]


def _logical_lines(source: str) -> list[_LogicalLine]:
    lines = source.splitlines(keepends=True)
    result: list[_LogicalLine] = []
    characters: list[str] = []
    offsets: list[int] = []
    start_line = 0
    offset = 0

    def flush() -> None:
        nonlocal start_line
        text = "".join(characters)
        if text.strip() and not text.lstrip().startswith("#"):
            result.append(_LogicalLine(text, tuple(offsets), start_line))
        characters.clear()
        offsets.clear()
        start_line = 0

    for index, raw in enumerate(lines):
        content = raw.rstrip("\r\n")
        line_offset = offset
        offset += len(raw)
        if not characters:
            if not content.strip() or content.lstrip().startswith("#"):
                continue
            start_line = index + 1
        trimmed = content.rstrip()
        continuation = trimmed.endswith("\\")
        if continuation:
            content = trimmed[:-1]
        characters.extend(content)
        offsets.extend(range(line_offset, line_offset + len(content)))
        if continuation:
            characters.append(" ")
            offsets.append(line_offset + max(0, len(content) - 1))
            continue
        flush()
    flush()
    return result


def _expand(value: str, arguments: dict[str, str]) -> str:
    def replacement(match: re.Match[str]) -> str:
        name = match.group("braced") or match.group("bare")
        return arguments.get(name, match.group(0))

    return _VARIABLE.sub(replacement, value)


def _arg_values(lines: list[_LogicalLine]) -> dict[str, str]:
    values: dict[str, str] = {}
    for line in lines:
        match = _ARG.fullmatch(line.text)
        if match is None:
            continue
        raw = (match.group("value") or "").strip()
        if raw[:1] in {'"', "'"} and raw[-1:] == raw[:1]:
            raw = raw[1:-1]
        if raw:
            values[match.group("name")] = _expand(raw, values)
    return values


def _shell_tokens(text: str) -> list[_Token]:
    tokens: list[_Token] = []
    index = 0
    while index < len(text):
        character = text[index]
        if character.isspace():
            index += 1
            continue
        if text.startswith(("&&", "||"), index):
            tokens.append(_Token(text[index : index + 2], index, index + 2, True))
            index += 2
            continue
        if character in ";&|()":
            tokens.append(_Token(character, index, index + 1, True))
            index += 1
            continue
        start = index
        value: list[str] = []
        while index < len(text) and not text[index].isspace() and text[index] not in ";&|()":
            character = text[index]
            if character in {'"', "'"}:
                quote = character
                index += 1
                while index < len(text) and text[index] != quote:
                    if quote == '"' and text[index] == "\\" and index + 1 < len(text):
                        value.append(text[index + 1])
                        index += 2
                        continue
                    value.append(text[index])
                    index += 1
                index += 1
                continue
            if character == "\\" and index + 1 < len(text):
                value.append(text[index + 1])
                index += 2
                continue
            value.append(character)
            index += 1
        tokens.append(_Token("".join(value), start, index, False))
    return tokens


def _apk_commands(text: str) -> list[_ApkCommand]:
    tokens = _shell_tokens(text)
    commands: list[_ApkCommand] = []
    index = 0
    while index < len(tokens):
        token = tokens[index]
        if token.separator or token.text != "apk":
            index += 1
            continue
        if index > 0 and not tokens[index - 1].separator and index != 0:
            index += 1
            continue
        if index + 1 >= len(tokens) or tokens[index + 1].text != "add":
            index += 1
            continue
        cursor = index + 2
        packages: list[_Package] = []
        repositories: list[str] = []
        while cursor < len(tokens) and not tokens[cursor].separator:
            current = tokens[cursor]
            if current.text in {"--virtual", "-t"}:
                cursor += 2
                continue
            if current.text == "--repository":
                if cursor + 1 < len(tokens) and not tokens[cursor + 1].separator:
                    repositories.append(tokens[cursor + 1].text)
                cursor += 2
                continue
            if current.text.startswith("-"):
                cursor += 1
                continue
            name, separator, version = current.text.partition("=")
            packages.append(
                _Package(
                    name=name,
                    version=version if separator else None,
                    start=current.start,
                    end=current.end,
                    raw=current.text,
                )
            )
            cursor += 1
        commands.append(_ApkCommand(tuple(packages), tuple(repositories)))
        index = cursor
    return commands


def _split_image(image: str) -> tuple[str, str | None, str | None]:
    name, separator, digest = image.partition("@")
    tag = None
    last = name.rsplit("/", 1)[-1]
    if ":" in last:
        name, _, tag = name.rpartition(":")
    return name, tag, digest if separator else None


def _repository_name(repo: str) -> str:
    return repo.rsplit("/", 1)[-1].casefold()


def _platform_arch(platform: str | None) -> str | None:
    if platform is None or platform.startswith("$"):
        return None
    for part in reversed([value for value in platform.split("/") if value]):
        if part in {"amd64", "arm64"}:
            return part
        if part == "x86_64":
            return "amd64"
        if part == "aarch64":
            return "arm64"
    return None


def _version_matches(tag: str, version: str, release: str) -> bool:
    return tag in {version, f"{version}-{release}"}


def _channel_from_tag(lock: MuslToolchain, tag: str | None) -> str | None:
    if tag == "edge":
        return "edge"
    if (
        tag is not None
        and _VERSION_TAG.fullmatch(tag) is not None
        and tag.split(".", 2)[0:2] == lock.channels["stable"].branch.split(".", 1)
    ):
        return "stable"
    return None


def _channel_from_digest(lock: MuslToolchain, digest: str, arch: str | None) -> str | None:
    for name, channel in lock.channels.items():
        expected = channel.manifests[arch] if arch is not None else channel.index_digest
        if digest == expected:
            return name
    return None


def _approved_digest(lock: MuslToolchain, channel: str, arch: str | None) -> str:
    value = lock.channel(channel)
    return value.manifests[arch] if arch is not None else value.index_digest


@dataclass(slots=True)
class _Analysis:
    findings: list[Finding]
    edits: list[tuple[int, int, str]]


def _failure(
    repository: Repository,
    path: str,
    message: str,
    line: int | None = None,
    fixable: bool = False,
) -> Finding:
    return violation(repository, _RULE_ID, path, message, fixable=fixable, line=line)


def _stage_check(
    repository: Repository,
    lock: MuslToolchain,
    path: str,
    line: _LogicalLine,
    match: re.Match[str],
    arguments: dict[str, str],
    adapter: Adapter | None,
) -> tuple[_Stage, list[Finding], list[tuple[int, int, str]]]:
    findings: list[Finding] = []
    edits: list[tuple[int, int, str]] = []
    options = match.group("options") or ""
    platform_match = re.search(r"--platform=(\S+)", options)
    platform = platform_match.group(1) if platform_match is not None else None
    image = _expand(match.group("image"), arguments)
    image_span = line.span(match.start("image"), match.end("image"))
    if "$" in image:
        findings.append(_failure(repository, path, "base image uses an unresolved build argument", line.line))
        return _Stage(image, None, None, platform, None, False), findings, edits
    repo, tag, digest = _split_image(image)
    if lock.image is not None and repo.casefold() == lock.image.reference.casefold():
        if digest != lock.image.digest:
            findings.append(
                _failure(
                    repository,
                    path,
                    f"canonical builder image must use the approved digest {lock.image.digest}",
                    line.line,
                    fixable=image_span is not None,
                )
            )
            if image_span is not None:
                edits.append((image_span[0], image_span[1], f"{lock.image.reference}@{lock.image.digest}"))
        return _Stage(image, tag, digest, platform, None, True), findings, edits
    name = _repository_name(repo)
    if name != "alpine":
        if name in {"debian", "ubuntu"} and (digest is None or _SHA256.fullmatch(digest) is None):
            findings.append(
                _failure(repository, path, "Debian or Ubuntu host image must be digest-pinned", line.line)
            )
        elif name != "scratch":
            findings.append(
                _failure(
                    repository,
                    path,
                    f"base image {repo} is not an approved Alpine base or the published canonical image",
                    line.line,
                )
            )
        return _Stage(image, tag, digest, platform, None, name == "scratch"), findings, edits
    arch = _platform_arch(platform)
    adapter_channel = adapter.channel if adapter is not None else None
    channel = adapter_channel or _channel_from_tag(lock, tag)
    if digest is not None and _SHA256.fullmatch(digest) is None:
        findings.append(_failure(repository, path, f"base digest is not a sha256 digest: {digest}", line.line))
        return _Stage(image, tag, digest, platform, channel, False), findings, edits
    if channel is None and digest is not None:
        channel = _channel_from_digest(lock, digest, arch)
    if channel is None:
        findings.append(
            _failure(
                repository,
                path,
                "Alpine base digest does not match an approved stable or edge digest",
                line.line,
            )
        )
        return _Stage(image, tag, digest, platform, None, False), findings, edits
    expected = _approved_digest(lock, channel, arch)
    if digest != expected:
        findings.append(
            _failure(
                repository,
                path,
                f"Alpine base must use the approved {channel} digest {expected}",
                line.line,
                fixable=image_span is not None,
            )
        )
        if image_span is not None:
            reference = lock.channel(channel).image.partition("@")[0]
            edits.append((image_span[0], image_span[1], f"{reference}@{expected}"))
    return _Stage(image, tag, digest, platform, channel, False), findings, edits


def _package_finding(
    repository: Repository,
    lock: MuslToolchain,
    path: str,
    line: int,
    package: _Package,
    channel: str | None,
    require_table: bool,
    live: bool,
) -> tuple[Finding | None, str | None]:
    if channel is None:
        if package.version is None:
            return (
                _failure(repository, path, f"package {package.name} must be pinned as name=version", line),
                None,
            )
        return None, None
    versions = {
        arch: lock.channel(channel).packages[arch].get(package.name) for arch in _ARCHES
    }
    if all(version is None for version in versions.values()):
        if not require_table:
            if package.version is None:
                return (
                    _failure(repository, path, f"package {package.name} must be pinned as name=version", line),
                    None,
                )
            return None, None
        return (
            _failure(
                repository,
                path,
                f"package {package.name} is not in musl-toolchain.toml; add its approved version",
                line,
            ),
            None,
        )
    if versions["x86_64"] != versions["aarch64"]:
        if "$" in package.raw and package.version in versions.values():
            return None, None
        return (
            _failure(
                repository,
                path,
                f"package {package.name} has different approved versions per architecture; "
                "use an architecture-conditional ARG",
                line,
            ),
            None,
        )
    expected = versions["x86_64"]
    assert expected is not None
    if package.version is None:
        return (
            _failure(
                repository,
                path,
                f"package {package.name} must be pinned as {package.name}={expected}",
                line,
                fixable=live,
            ),
            expected,
        )
    if package.version != expected:
        return (
            _failure(
                repository,
                path,
                f"package {package.name}={package.version} is stale; use {package.name}={expected}",
                line,
                fixable=live,
            ),
            expected,
        )
    return None, expected


def _analyze_dockerfile(
    repository: Repository,
    lock: MuslToolchain,
    root: Path,
    path: Path,
    adapter: Adapter | None,
    require_table: bool,
) -> _Analysis:
    relative = path.relative_to(root).as_posix()
    try:
        source = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        return _Analysis([operational(repository, _RULE_ID, relative, f"cannot read Dockerfile: {error}")], [])
    lines = _logical_lines(source)
    arguments = _arg_values(lines)
    findings: list[Finding] = []
    edits: list[tuple[int, int, str]] = []
    package_edits: list[tuple[int, int, str]] = []
    stage: _Stage | None = None
    packages: list[tuple[_Stage | None, _Package, _LogicalLine, int]] = []
    for line in lines:
        from_match = _FROM.fullmatch(line.text)
        if from_match is not None:
            stage, stage_findings, stage_edits = _stage_check(
                repository, lock, relative, line, from_match, arguments, adapter
            )
            findings.extend(stage_findings)
            edits.extend(stage_edits)
            continue
        run_match = _RUN.match(line.text)
        if run_match is None:
            continue
        run_text = line.text[run_match.end() :]
        for command in _apk_commands(run_text):
            findings.extend(
                _failure(
                    repository,
                    relative,
                    f"apk add uses a non-approved repository: {repository_url}",
                    line.line,
                )
                for repository_url in command.repositories
            )
            packages.extend((stage, package, line, run_match.end()) for package in command.packages)
        if _APK_URL.search(run_text) is not None:
            findings.append(
                _failure(repository, relative, "musl builder must not install a direct .apk URL", line.line)
            )
    resolved_packages: list[tuple[_Stage, _Package, _LogicalLine]] = []
    file_fixable = True
    for package_stage, package, line, offset in packages:
        if package_stage is None or package_stage.exempt:
            continue
        expanded = _expand(package.raw, arguments)
        if "$" in expanded:
            findings.append(
                _failure(
                    repository,
                    relative,
                    f"apk package token {package.raw} uses an unresolved build argument",
                    line.line,
                )
            )
            file_fixable = False
            continue
        name, separator, version = expanded.partition("=")
        resolved = _Package(
            name=name,
            version=version if separator else None,
            start=offset + package.start,
            end=offset + package.end,
            raw=expanded,
        )
        channel = package_stage.channel
        if channel is None:
            if resolved.version is None:
                file_fixable = False
        else:
            versions = {
                arch: lock.channel(channel).packages[arch].get(name) for arch in _ARCHES
            }
            if all(value is None for value in versions.values()):
                if require_table or resolved.version is None:
                    file_fixable = False
            elif versions["x86_64"] != versions["aarch64"] and not (
                "$" in package.raw and resolved.version in versions.values()
            ):
                file_fixable = False
        resolved_packages.append((package_stage, resolved, line))
    for package_stage, resolved, line in resolved_packages:
        finding, expected = _package_finding(
            repository,
            lock,
            relative,
            line.line,
            resolved,
            package_stage.channel,
            require_table,
            live=file_fixable,
        )
        if finding is not None:
            findings.append(finding)
        if expected is not None and package_stage.channel is not None:
            span = line.span(resolved.start, resolved.end)
            if span is not None:
                package_edits.append((span[0], span[1], f"{resolved.name}={expected}"))
    if file_fixable:
        edits.extend(package_edits)
    lowered = source.casefold()
    if "apt-get" in lowered:
        findings.append(_failure(repository, relative, "musl builder must not use apt-get"))
    gnu_source = source
    if adapter is not None and adapter.kind == "chromium-build":
        gnu_source = gnu_source.replace("x86_64-unknown-linux-gnu", "")
    if "unknown-linux-gnu" in gnu_source:
        findings.append(_failure(repository, relative, "musl builder must not build unknown-linux-gnu targets"))
    checksum_verified = any(marker in lowered for marker in _CHECKSUM_VERIFIED)
    if "sh.rustup.rs" in source or "rustup/dist/" in source:
        findings.append(
            _failure(
                repository,
                relative,
                "musl builder must install rustup from the versioned archive URL, not an unverified installer",
            )
        )
    elif not checksum_verified and any(
        marker in source for marker in ("rustup/archive/", "llvm-prebuilt-musl", "mold-musl")
    ):
        findings.append(
            _failure(repository, relative, "musl builder must verify every remote toolchain archive with sha256")
        )
    return _Analysis(findings, edits)


def _without_comments(source: str) -> str:
    return "\n".join("" if line.lstrip().startswith("#") else line for line in source.splitlines())


def _relative_path_from_reference(root: Path, value: str) -> Path | None:
    candidate = Path(value.strip().strip("'\""))
    if candidate.is_absolute():
        path = candidate
    else:
        path = root / candidate
    try:
        resolved = path.resolve(strict=False)
        if not resolved.is_relative_to(root.resolve()):
            return None
    except (OSError, ValueError):
        return None
    if path.is_symlink() or not path.is_file():
        return None
    return path


def _yaml_step(source: str, position: int) -> str:
    lines = source.splitlines()
    starts = [0]
    for line in lines:
        starts.append(starts[-1] + len(line) + 1)
    index = max(0, next((i for i, start in enumerate(starts) if start > position), len(lines)) - 1)
    indent = len(lines[index]) - len(lines[index].lstrip())
    start = index
    while start > 0:
        previous = lines[start - 1]
        previous_indent = len(previous) - len(previous.lstrip())
        if previous.lstrip().startswith("- ") and previous_indent <= indent:
            start -= 1
            break
        start -= 1
    end = len(lines)
    for cursor in range(start + 1, len(lines)):
        line = lines[cursor]
        line_indent = len(line) - len(line.lstrip())
        if line.lstrip().startswith("- ") and line_indent < indent:
            end = cursor
            break
    return "\n".join(lines[start:end])


def _workflow_references(root: Path, workflows: list[tuple[str, str]]) -> set[Path]:
    references: set[Path] = set()
    for _, source in workflows:
        if _MUSL_TRIPLE.search(source) is None:
            continue
        for match in _DOCKER_REFERENCE.finditer(source):
            value = match.group("flag") or match.group("file")
            if value is None:
                continue
            path = _relative_path_from_reference(root, value)
            if path is None:
                continue
            try:
                mentions_musl = "musl" in path.read_text(encoding="utf-8").casefold()
            except (OSError, UnicodeError):
                mentions_musl = True
            if mentions_musl or "musl" in _yaml_step(source, match.start()).casefold():
                references.add(path)
    return references


def _candidate_dockerfiles(root: Path, references: set[Path]) -> list[Path]:
    candidates: set[Path] = set()
    for directory, names, filenames in os.walk(root, followlinks=False):
        current = Path(directory)
        relative = current.relative_to(root)
        names[:] = sorted(
            name
            for name in names
            if name not in _IGNORED_DIRECTORIES
            and not (current / name).is_symlink()
            and not (relative == Path("tests") and name == "fixtures")
        )
        for filename in filenames:
            path = current / filename
            if path.is_symlink() or not path.is_file():
                continue
            if _DOCKERFILE_NAME.fullmatch(filename) is not None:
                candidates.add(path)
    candidates.update(references)
    return sorted(candidates, key=lambda path: path.relative_to(root).as_posix().casefold())


def _release_recipe(root: Path) -> str | None:
    for name in ("GNUmakefile", "makefile", "Makefile"):
        path = root / name
        if path.is_symlink() or not path.is_file():
            continue
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except (OSError, UnicodeError):
            return None
        recipe: list[str] = []
        for index, line in enumerate(lines):
            if re.match(r"^(?:release|dist)(?:[-A-Za-z0-9_]*)?\s*:", line) is None:
                continue
            for following in lines[index + 1 :]:
                if following and not following[0].isspace():
                    break
                recipe.append(following)
        return "\n".join(recipe) if recipe else None
    return None


def _rust_contract_findings(
    root: Path,
    repository: Repository,
    lock: MuslToolchain,
    container: Path | None,
) -> list[Finding]:
    if container is None:
        return []
    relative = container.relative_to(root).as_posix()
    try:
        source = container.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        return [operational(repository, _RULE_ID, relative, f"cannot read Dockerfile: {error}")]
    arguments = _arg_values(_logical_lines(source))
    expanded = _expand(source, arguments)
    findings: list[Finding] = []
    checksum_verified = any(marker in source.casefold() for marker in _CHECKSUM_VERIFIED)
    versions = set(re.findall(r"rustup/archive/([^/\s\"'\\]+)/", expanded))
    findings.extend(
        _failure(
            repository,
            relative,
            f"rustup archive {version} is not approved {lock.toolchain.rustup_version}",
        )
        for version in sorted(versions)
        if version != lock.toolchain.rustup_version
    )
    if "llvm-prebuilt-musl" not in source:
        findings.append(
            _failure(repository, relative, "Rust builder must install the musl-linked LLVM prebuilt")
        )
    llvm_version = arguments.get("LLVM_VERSION")
    if llvm_version is not None and llvm_version != lock.toolchain.llvm_version:
        findings.append(
            _failure(
                repository,
                relative,
                f"LLVM version {llvm_version} is not approved {lock.toolchain.llvm_version}",
            )
        )
    llvm_release = arguments.get("LLVM_RELEASE_SHA")
    if llvm_release is not None and llvm_release != lock.toolchain.llvm_release:
        findings.append(
            _failure(
                repository,
                relative,
                f"LLVM release {llvm_release} is not approved {lock.toolchain.llvm_release}",
            )
        )
    findings.extend(
        _failure(repository, relative, "LLVM archive URL is not the approved release")
        for match in re.finditer(r"llvm-musl-([0-9A-Za-z._-]+)", expanded)
        if not _version_matches(match.group(1), lock.toolchain.llvm_version, lock.toolchain.llvm_release)
    )
    mold_version = arguments.get("MOLD_VERSION")
    if mold_version is not None and mold_version != lock.toolchain.mold_version:
        findings.append(
            _failure(repository, relative, f"mold version {mold_version} is not approved {lock.toolchain.mold_version}")
        )
    mold_release = arguments.get("MOLD_RELEASE_SHA")
    if mold_release is not None and mold_release != lock.toolchain.mold_release:
        findings.append(
            _failure(repository, relative, f"mold release {mold_release} is not approved {lock.toolchain.mold_release}")
        )
    findings.extend(
        _failure(repository, relative, "mold archive URL is not the approved release")
        for match in re.finditer(r"mold-musl-([0-9A-Za-z._-]+)", expanded)
        if not _version_matches(match.group(1), lock.toolchain.mold_version, lock.toolchain.mold_release)
    )
    findings.extend(
        _failure(repository, relative, f"Rust builder must install the {target} target")
        for target in ("x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl")
        if target not in source
    )
    findings.extend(
        _failure(repository, relative, message)
        for marker, message in (
            ("rust-src", "Rust builder must include the rust-src component"),
            ("musl-cargo", "Rust builder must provide the musl-cargo linker wrapper"),
            ("crtbegin", "Rust builder must provide musl CRT startup stubs"),
            ("libutil.a", "Rust builder must stub libutil.a from musl libc"),
        )
        if marker not in source
    )
    if not checksum_verified:
        findings.append(
            _failure(repository, relative, "Rust builder must verify remote toolchain archives with sha256")
        )
    recipe = _release_recipe(root)
    if recipe is None:
        findings.append(
            _failure(repository, relative, "root Makefile must define a release or dist recipe for musl builds")
        )
    elif "-Z build-std=std" not in recipe or "-Z build-std-features=" not in recipe:
        findings.append(
            _failure(
                repository,
                relative,
                "root Makefile release recipe must build std with -Z build-std=std and -Z build-std-features=",
            )
        )
    return findings


def _read_adapter_file(root: Path, relative: str, repository: Repository) -> tuple[str | None, Finding | None]:
    path = root / relative
    if path.is_symlink() or not path.is_file():
        return None, operational(repository, _RULE_ID, relative, "adapter file is missing or not a regular file")
    try:
        return path.read_text(encoding="utf-8"), None
    except (OSError, UnicodeError) as error:
        return None, operational(repository, _RULE_ID, relative, f"cannot read adapter file: {error}")


def _adapter_with_lock_findings(
    root: Path, repository: Repository, lock: MuslToolchain, adapter: Adapter
) -> list[Finding]:
    if adapter.lock is None:
        return [operational(repository, _RULE_ID, None, "adapter has no lock path")]
    document, error = read_json_object(root / adapter.lock)
    if document is None:
        return [
            operational(
                repository,
                _RULE_ID,
                adapter.lock,
                f"cannot read adapter lock: {error}",
            )
        ]
    channel = lock.channel(adapter.channel or "")
    findings: list[Finding] = []
    if adapter.kind == "python-build":
        linux = document.get("linux_toolchain")
        if not isinstance(linux, dict):
            return [_failure(repository, adapter.lock, "bootstrap lock has no linux_toolchain table")]
        if linux.get("release") != channel.release:
            findings.append(
                _failure(
                    repository,
                    adapter.lock,
                    f"linux toolchain release {linux.get('release')} is not approved {channel.release}; "
                    "run the repository's bootstrap refresh",
                )
            )
        if linux.get("image_index_digest") != channel.index_digest:
            findings.append(
                _failure(
                    repository,
                    adapter.lock,
                    "linux toolchain index digest is not approved; run the repository's bootstrap refresh",
                )
            )
        images = linux.get("images")
        images = images if isinstance(images, dict) else {}
        for manifest_arch in _MANIFEST_ARCHES:
            entry = images.get(manifest_arch)
            digest = entry.get("digest") if isinstance(entry, dict) else None
            if digest != channel.manifests[manifest_arch]:
                findings.append(
                    _failure(
                        repository,
                        adapter.lock,
                        f"linux {manifest_arch} image digest is not approved; run the repository's bootstrap refresh",
                    )
                )
        closure = linux.get("apk_closure")
        closure = closure if isinstance(closure, dict) else {}
        for arch in _ARCHES:
            entries = closure.get(arch)
            entries = entries if isinstance(entries, list) else []
            versions = {
                entry.get("name"): entry.get("version")
                for entry in entries
                if isinstance(entry, dict)
            }
            expected = channel.packages[arch]["musl"]
            findings.extend(
                _failure(
                    repository,
                    adapter.lock,
                    f"{arch} closure {name} must be {expected}; run the repository's bootstrap refresh",
                )
                for name in ("musl", "musl-dev")
                if versions.get(name) != expected
            )
        return findings
    if adapter.kind == "chromium-build":
        if document.get("base_release") != channel.release:
            findings.append(
                _failure(
                    repository,
                    adapter.lock,
                    f"package lock base_release {document.get('base_release')} is not approved "
                    f"{channel.release}; run update-lock",
                )
            )
        world = document.get("world")
        expected = channel.packages["x86_64"]["musl-dev"]
        entries = world if isinstance(world, list) else []
        versions = {
            entry.partition("=")[0]: entry.partition("=")[2]
            for entry in entries
            if isinstance(entry, str) and "=" in entry
        }
        if versions.get("musl-dev") != expected:
            findings.append(
                _failure(
                    repository,
                    adapter.lock,
                    f"package lock musl-dev must be {expected}; run update-lock",
                )
            )
        source, error = _read_adapter_file(root, "Dockerfile", repository)
        if source is None:
            return findings + ([error] if error is not None else [])
        arguments = _arg_values(_logical_lines(source))
        base = _expand(arguments.get("BASE_IMAGE", ""), arguments)
        _, _, digest = _split_image(base)
        approved = {channel.index_digest, *channel.manifests.values()}
        if digest not in approved:
            findings.append(
                _failure(repository, "Dockerfile", "BASE_IMAGE digest is not approved; run update-lock")
            )
        for line in _logical_lines(source):
            match = _FROM.fullmatch(line.text)
            if match is None:
                continue
            image = _expand(match.group("image"), arguments)
            repo, _, digest = _split_image(image)
            if _repository_name(repo) in {"debian", "ubuntu"} and digest is None:
                findings.append(
                    _failure(repository, "Dockerfile", "compiler host image must be digest-pinned", line.line)
                )
        direct: dict[str, str | None] = {}
        for line in _logical_lines(source):
            run_match = _RUN.match(line.text)
            if run_match is None:
                continue
            for command in _apk_commands(line.text[run_match.end() :]):
                for package in command.packages:
                    direct[package.name] = package.version
        if direct.get("musl-dev") != expected:
            findings.append(
                _failure(
                    repository,
                    "Dockerfile",
                    f"Dockerfile musl-dev must be {expected}; run update-lock",
                )
            )
        if "apt-get" in source.casefold():
            findings.append(_failure(repository, "Dockerfile", "musl builder must not use apt-get"))
        if "unknown-linux-gnu" in source.replace("x86_64-unknown-linux-gnu", ""):
            findings.append(
                _failure(repository, "Dockerfile", "musl builder must not build unknown-linux-gnu targets")
            )
        return findings
    return []


def _workflow_apk_packages(line: str) -> list[_Package]:
    match = re.search(r"\bapk\s+add\b", line)
    if match is None:
        return []
    commands = _apk_commands(line[match.start() :])
    return [package for command in commands for package in command.packages]


def _adapter_workflow_findings(
    root: Path, repository: Repository, lock: MuslToolchain, adapter: Adapter
) -> list[Finding]:
    if adapter.kind == "node-build":
        if adapter.workflow is None:
            return []
        source, error = _read_adapter_file(root, adapter.workflow, repository)
        if source is None:
            return [error] if error is not None else []
        channel = lock.channel(adapter.channel or "")
        findings: list[Finding] = []
        image = re.search(r"(?m)^\s*ALPINE_AMD64:\s*(?P<image>\S+)\s*$", source)
        _, _, digest = _split_image(image.group("image")) if image is not None else ("", None, None)
        approved = {channel.index_digest, *channel.manifests.values()}
        if digest not in approved:
            findings.append(
                _failure(repository, adapter.workflow, "ALPINE_AMD64 digest is not the approved edge digest")
            )
        apk_lines = []
        for index, line in enumerate(source.splitlines(), start=1):
            packages = _workflow_apk_packages(line)
            if packages:
                apk_lines.append((index, packages))
        all_packages = [package for _, packages in apk_lines for package in packages]
        file_fixable = bool(all_packages) and all(
            channel.packages["x86_64"].get(package.name) is not None for package in all_packages
        )
        for index, packages in apk_lines:
            for package in packages:
                finding, _ = _package_finding(
                    repository,
                    lock,
                    adapter.workflow,
                    index,
                    package,
                    adapter.channel,
                    require_table=True,
                    live=file_fixable,
                )
                if finding is not None:
                    findings.append(finding)
        return findings
    if adapter.kind == "zzz":
        findings = []
        checksum = adapter.zig_archive_sha256
        for relative, source in read_workflow_sources(root, repository)[0]:
            urls = list(_ZIG_URL.finditer(source))
            if not urls:
                continue
            findings.extend(
                _failure(
                    repository,
                    relative,
                    f"Zig archive {match.group('version')} is not approved {adapter.zig_version}",
                )
                for match in urls
                if match.group("version") != adapter.zig_version
            )
            if checksum is not None and checksum not in source:
                findings.append(
                    _failure(
                        repository,
                        relative,
                        "Zig archive download must verify the approved sha256 checksum",
                        fixable=True,
                    )
                )
        return findings
    return []


def _host_musl_build_findings(
    root: Path,
    repository: Repository,
    lock: MuslToolchain,
    workflows: list[tuple[str, str]],
) -> list[Finding]:
    findings: list[Finding] = []
    adapter = lock.adapters.get(str(repository.id))
    zzz_pinned = adapter is not None and adapter.kind == "zzz" and adapter.zig_archive_sha256 is not None
    for relative, source in workflows:
        clean = _without_comments(source)
        if _release_kind(root, clean) != "binary":
            continue
        if _MUSL_TRIPLE.search(clean) is None:
            continue
        if re.search(r"(?m)^\s*runs-on:\s*ubuntu-", clean) is None:
            continue
        if any(
            marker in clean
            for marker in (
                "container:",
                "docker build",
                "docker run",
                "docker/build-push-action",
                "docker/setup-buildx-action",
                "DOCKER_PLATFORM",
            )
        ):
            continue
        if zzz_pinned:
            continue
        findings.append(
            _failure(
                repository,
                relative,
                "binary prerelease must build Linux musl targets in a container or through Docker",
            )
        )
    return findings


def _dockerfile_in_scope(path: Path, references: set[Path]) -> bool:
    if path in references:
        return True
    try:
        return "musl" in path.read_text(encoding="utf-8").casefold()
    except (OSError, UnicodeError):
        return True


def _linux_musl_toolchain_findings(root: Path, repository: Repository) -> list[Finding]:
    if repository.private or set(repository.tags) & {"fork", "macos"}:
        return []
    try:
        lock = load_musl_toolchain()
    except (OSError, ValueError) as error:
        return [operational(repository, _RULE_ID, "musl-toolchain.toml", str(error))]
    workflows, problems = read_workflow_sources(root, repository)
    findings = [operational(repository, _RULE_ID, path, message) for path, message in problems]
    adapter = lock.adapters.get(str(repository.id))
    if adapter is not None and adapter.kind in {"python-build", "chromium-build"}:
        findings.extend(_adapter_with_lock_findings(root, repository, lock, adapter))
        return findings
    references = _workflow_references(root, workflows)
    dockerfiles = [
        path
        for path in _candidate_dockerfiles(root, references)
        if _dockerfile_in_scope(path, references)
    ]
    require_table = adapter is None or adapter.kind != "git-build"
    for path in dockerfiles:
        analysis = _analyze_dockerfile(repository, lock, root, path, adapter, require_table)
        findings.extend(analysis.findings)
    if has_binary_release_contract(root, repository) and (root / "Cargo.toml").is_file():
        root_dockerfile = next(
            (path for path in dockerfiles if path == root / "Dockerfile"),
            dockerfiles[0] if dockerfiles else None,
        )
        findings.extend(_rust_contract_findings(root, repository, lock, root_dockerfile))
    findings.extend(_adapter_workflow_findings(root, repository, lock, adapter) if adapter is not None else [])
    findings.extend(_host_musl_build_findings(root, repository, lock, workflows))
    unique: list[Finding] = []
    seen: set[tuple[str | None, str, str]] = set()
    for finding in findings:
        key = (finding.file, finding.message, finding.kind)
        if key in seen:
            continue
        seen.add(key)
        unique.append(finding)
    return unique


def _fixable_dockerfile(path: Path, root: Path) -> bool:
    name = path.name
    if name == "Dockerfile" or name.startswith("Dockerfile."):
        return path.parent == root
    return name.endswith(".Dockerfile")


def _apply_edits(source: str, edits: list[tuple[int, int, str]]) -> str:
    for start, end, replacement in sorted(edits, key=lambda edit: edit[0], reverse=True):
        source = source[:start] + replacement + source[end:]
    return source


def _fixed_node_workflow(source: str, lock: MuslToolchain, channel: str) -> str:
    lines = source.splitlines(keepends=True)
    apk_lines: list[tuple[int, int, list[_Package]]] = []
    for index, line in enumerate(lines):
        match = re.search(r"\bapk\s+add\b", line)
        if match is None:
            continue
        packages = _workflow_apk_packages(line)
        if packages:
            apk_lines.append((index, match.start(), packages))
    if not apk_lines:
        return source
    versions = lock.channel(channel).packages["x86_64"]
    if any(versions.get(package.name) is None for _, _, packages in apk_lines for package in packages):
        return source
    for index, offset, packages in apk_lines:
        line = lines[index]
        for package in sorted(packages, key=lambda item: item.start, reverse=True):
            expected = versions[package.name]
            line = f"{line[: offset + package.start]}{package.name}={expected}{line[offset + package.end :]}"
        lines[index] = line
    return "".join(lines)


def _fixed_zig_workflow(source: str, checksum: str) -> str:
    pattern = re.compile(
        r"(?m)^(?P<indent>\s*(?:run:\s*)?)curl\s+-fsSL\s+(?P<url>https://ziglang\.org/download/\S+\.tar\.xz)"
        r"\s*\|\s*tar\s+xJ\s*$"
    )
    return pattern.sub(
        lambda match: (
            f"{match.group('indent')}curl -fsSL -o zig.tar.xz {match.group('url')} "
            f'&& echo "{checksum}  zig.tar.xz" | sha256sum -c - && tar xJf zig.tar.xz'
        ),
        source,
    )


def _linux_musl_toolchain_fix(root: Path, repository: Repository) -> dict[str, str]:
    if repository.private or set(repository.tags) & {"fork", "macos"}:
        return {}
    try:
        lock = load_musl_toolchain()
    except (OSError, ValueError):
        return {}
    adapter = lock.adapters.get(str(repository.id))
    if adapter is not None and adapter.kind in {"python-build", "chromium-build"}:
        return {}
    workflows, _ = read_workflow_sources(root, repository)
    references = _workflow_references(root, workflows)
    dockerfiles = _candidate_dockerfiles(root, references)
    require_table = adapter is None or adapter.kind != "git-build"
    plan: dict[str, str] = {}
    for path in dockerfiles:
        if not _fixable_dockerfile(path, root):
            continue
        if not _dockerfile_in_scope(path, references):
            continue
        analysis = _analyze_dockerfile(repository, lock, root, path, adapter, require_table)
        if not analysis.edits:
            continue
        try:
            source = path.read_text(encoding="utf-8")
        except (OSError, UnicodeError):
            continue
        fixed = _apply_edits(source, analysis.edits)
        if fixed != source:
            plan[path.relative_to(root).as_posix()] = fixed
    if adapter is not None and adapter.kind == "node-build" and adapter.workflow is not None:
        source, error = _read_adapter_file(root, adapter.workflow, repository)
        if source is not None and error is None:
            fixed = _fixed_node_workflow(source, lock, adapter.channel or "")
            if fixed != source:
                plan[adapter.workflow] = fixed
    if adapter is not None and adapter.kind == "zzz" and adapter.zig_archive_sha256 is not None:
        for relative, source in workflows:
            if _ZIG_URL.search(source) is None or adapter.zig_archive_sha256 in source:
                continue
            fixed = _fixed_zig_workflow(source, adapter.zig_archive_sha256)
            if fixed != source:
                plan[relative] = fixed
    return plan


def image_build_arguments(lock: MuslToolchain) -> dict[str, str]:
    stable = lock.channels["stable"]
    archives = lock.toolchain.archives
    return {
        "BASE_IMAGE": f"{stable.image}@{stable.index_digest}",
        "MUSL_VERSION": lock.upstream_version,
        "MUSL_SHA256": lock.toolchain.musl_sha256,
        "LLVM_VERSION": lock.toolchain.llvm_version,
        "LLVM_RELEASE_SHA": lock.toolchain.llvm_release,
        "LLVM_X86_64_SHA256": archives["llvm"]["x86_64"].sha256,
        "LLVM_AARCH64_SHA256": archives["llvm"]["aarch64"].sha256,
        "MOLD_VERSION": lock.toolchain.mold_version,
        "MOLD_RELEASE_SHA": lock.toolchain.mold_release,
        "MOLD_X86_64_SHA256": archives["mold"]["x86_64"].sha256,
        "MOLD_AARCH64_SHA256": archives["mold"]["aarch64"].sha256,
        "RUSTUP_VERSION": lock.toolchain.rustup_version,
        "RUSTUP_X86_64_SHA256": archives["rustup"]["x86_64"].sha256,
        "RUSTUP_AARCH64_SHA256": archives["rustup"]["aarch64"].sha256,
    }


def image_package_versions(lock: MuslToolchain) -> dict[str, str]:
    stable = lock.channels["stable"]
    x86_64 = stable.packages["x86_64"]
    aarch64 = stable.packages["aarch64"]
    differing = sorted(
        name
        for name in set(x86_64) | set(aarch64)
        if x86_64.get(name) != aarch64.get(name)
    )
    if differing:
        raise ValueError(
            "canonical image requires one approved stable version per architecture: "
            + ", ".join(differing)
        )
    return dict(x86_64)


def validate_image_definition(
    lock: MuslToolchain, dockerfile: str, toolchain_source: str
) -> None:
    arguments = _arg_values(_logical_lines(dockerfile))
    for name, expected in image_build_arguments(lock).items():
        if arguments.get(name) != expected:
            raise ValueError(f"canonical image ARG {name} must default to {expected}")
    packages = image_package_versions(lock)
    for line in _logical_lines(dockerfile):
        run_match = _RUN.match(line.text)
        if run_match is None:
            continue
        for command in _apk_commands(line.text[run_match.end() :]):
            for package in command.packages:
                if package.name in _GCC_PACKAGES:
                    raise ValueError(
                        f"canonical image package {package.name} puts GCC objects in the build image"
                    )
                expected = packages.get(package.name)
                if expected is None:
                    raise ValueError(
                        f"canonical image package {package.name} is not in the approved stable package table"
                    )
                if package.version != expected:
                    raise ValueError(
                        f"canonical image package {package.name} must be {package.name}={expected}"
                    )
    for marker, message in (
        ("FROM ${BASE_IMAGE}", "canonical image must use the locked BASE_IMAGE argument"),
        ("sha256sum -c", "canonical image must verify every toolchain archive with sha256"),
        ("rustup toolchain install", "canonical image must install a Rust toolchain"),
        ("x86_64-unknown-linux-musl", "canonical image must install the x86_64 musl target"),
        ("aarch64-unknown-linux-musl", "canonical image must install the aarch64 musl target"),
        ("rust-src", "canonical image must install rust-src"),
        ("musl-cargo", "canonical image must provide the musl-cargo wrapper"),
        ("crtbegin", "canonical image must provide musl CRT stubs"),
        ("libutil.a", "canonical image must stub libutil.a"),
        ("libunwind.a", "canonical image must provide the LLVM libunwind"),
        ("musl-llvm", "canonical image must build musl libc with clang"),
    ):
        if marker not in dockerfile:
            raise ValueError(message)
    try:
        document = tomllib.loads(toolchain_source)
    except tomllib.TOMLDecodeError as error:
        raise ValueError(f"canonical image rust-toolchain.toml is invalid TOML: {error}") from error
    toolchain = document.get("toolchain")
    if not isinstance(toolchain, dict):
        raise ValueError("canonical image rust-toolchain.toml must contain [toolchain]")
    channel = toolchain.get("channel")
    if not isinstance(channel, str) or not channel.startswith("nightly-"):
        raise ValueError("canonical image rust-toolchain.toml must pin a nightly channel")
    components = toolchain.get("components")
    if not isinstance(components, list) or "rust-src" not in components:
        raise ValueError("canonical image rust-toolchain.toml must include rust-src")


def musl_toolchain_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            _RULE_ID,
            "Check every musl build input against the approved toolchain lock",
            TagPredicate(),
            _linux_musl_toolchain_findings,
            _linux_musl_toolchain_fix,
            "fix(musl): align builders with the approved toolchain lock",
            (
                "Dockerfile",
                "Dockerfile.*",
                "*.Dockerfile",
                "docker/*.Dockerfile",
                ".github/workflows/*.yml",
                ".github/workflows/*.yaml",
            ),
        ),
    )
