from __future__ import annotations

import io
import tarfile
from pathlib import Path

import pytest

from repomod.rules import musl_toolchain as musl

STABLE_INDEX = "sha256:" + "1" * 64
STABLE_AMD64 = "sha256:" + "2" * 64
STABLE_ARM64 = "sha256:" + "3" * 64
EDGE_INDEX = "sha256:" + "4" * 64
EDGE_AMD64 = "sha256:" + "5" * 64
EDGE_ARM64 = "sha256:" + "6" * 64
UPSTREAM_HTML = (
    '<p><dt><a href="releases/musl-1.2.6.tar.gz">musl-1.2.6.tar.gz</a>\n'
    '(<a href="releases/musl-1.2.6.tar.gz.asc">sig</a>)\n'
    "- March 20, 2026</p>\n"
)


def lock_source(
    stable_musl: str = "1.2.6-r3",
    edge_musl: str = "1.2.6-r6",
    stable_index: str = STABLE_INDEX,
    stable_amd64: str = STABLE_AMD64,
    stable_arm64: str = STABLE_ARM64,
) -> str:
    return f"""version = 1

[upstream]
version = "1.2.6"
released = "2026-03-20"

[channels.stable]
branch = "3.24"
image = "alpine:3.24.2"
index_digest = "{stable_index}"
release = "3.24.2"

[channels.stable.manifests]
amd64 = "{stable_amd64}"
arm64 = "{stable_arm64}"

[channels.stable.packages.x86_64]
bash = "5.3.9-r1"
musl = "{stable_musl}"
musl-dev = "{stable_musl}"

[channels.stable.packages.aarch64]
bash = "5.3.9-r1"
musl = "{stable_musl}"
musl-dev = "{stable_musl}"

[channels.edge]
branch = "edge"
image = "alpine:edge"
index_digest = "{EDGE_INDEX}"
release = "3.25.0_alpha20260805"

[channels.edge.manifests]
amd64 = "{EDGE_AMD64}"
arm64 = "{EDGE_ARM64}"

[channels.edge.packages.x86_64]
bash = "5.3.9-r2"
musl = "{edge_musl}"
musl-dev = "{edge_musl}"

[channels.edge.packages.aarch64]
bash = "5.3.9-r2"
musl = "{edge_musl}"
musl-dev = "{edge_musl}"

[toolchain]
llvm_version = "23.1.0-rc2"
llvm_release = "6eb5fb9"
mold_version = "3.0.0"
mold_release = "cfb4aab"
rustup_version = "1.29.1"

[toolchain.archives.llvm.x86_64]
url = "https://example.invalid/llvm-x86_64.tar.xz"
sha256 = "{'a' * 64}"

[toolchain.archives.llvm.aarch64]
url = "https://example.invalid/llvm-aarch64.tar.xz"
sha256 = "{'b' * 64}"

[toolchain.archives.mold.x86_64]
url = "https://example.invalid/mold-x86_64.tar.gz"
sha256 = "{'c' * 64}"

[toolchain.archives.mold.aarch64]
url = "https://example.invalid/mold-aarch64.tar.gz"
sha256 = "{'d' * 64}"

[toolchain.archives.rustup.x86_64]
url = "https://example.invalid/rustup-init-x86_64"
sha256 = "{'e' * 64}"

[toolchain.archives.rustup.aarch64]
url = "https://example.invalid/rustup-init-aarch64"
sha256 = "{'f' * 64}"

[adapters."laputa-systems/zzz"]
kind = "zzz"
zig_version = "0.16.0"
zig_archive_sha256 = "{'0' * 64}"
"""


def apkindex_tar(source: str) -> bytes:
    payload = source.encode()
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
        info = tarfile.TarInfo("APKINDEX")
        info.size = len(payload)
        archive.addfile(info, io.BytesIO(payload))
    return buffer.getvalue()


def index_source(musl_version: str, bash_version: str) -> str:
    return (
        f"P:bash\nV:{bash_version}\nS:1\n\n"
        f"P:musl\nV:{musl_version}\nS:1\n\n"
        f"P:musl-dev\nV:{musl_version}\nS:1\n\n"
        f"P:dotnet\nV:8.0.1-r0\nS:1\n\n"
        f"P:dotnet\nV:10.0.1-r0\nS:1\n\n"
    )


def write_lock(tmp_path: Path, source: str | None = None) -> Path:
    path = tmp_path / "musl-toolchain.toml"
    path.write_text(lock_source() if source is None else source, encoding="utf-8")
    return path


class Sources:
    def __init__(
        self,
        *,
        stable_musl: str = "1.2.6-r3",
        edge_musl: str = "1.2.6-r6",
        stable_index: str = STABLE_INDEX,
        stable_amd64: str = STABLE_AMD64,
        stable_arm64: str = STABLE_ARM64,
        releases: bytes | None = None,
        broken_index: str | None = None,
    ) -> None:
        self.stable_musl = stable_musl
        self.edge_musl = edge_musl
        self.stable_index = stable_index
        self.stable_amd64 = stable_amd64
        self.stable_arm64 = stable_arm64
        self.releases = releases
        self.broken_index = broken_index

    def apkindex(self, repository: str, component: str, arch: str) -> bytes:
        name = f"{musl._ALPINE_CDN}/{repository}/{component}/{arch}/APKINDEX.tar.gz"
        if name == self.broken_index:
            return b"this is not a tarball"
        if component == "community":
            return apkindex_tar(
                "P:mold\nV:2.39.1-r0\nS:1\n\nP:dotnet\nV:8.0.1-r0\nS:1\n\n"
                "P:dotnet\nV:10.0.1-r0\nS:1\n\n"
            )
        musl_version = self.stable_musl if repository == "v3.24" else self.edge_musl
        bash = "5.3.9-r1" if repository == "v3.24" else "5.3.9-r2"
        return apkindex_tar(index_source(musl_version, bash))

    def download(self, url: str) -> bytes:
        if url == musl._MUSL_RELEASES_URL:
            return UPSTREAM_HTML.encode() if self.releases is None else self.releases
        for repository in ("v3.24", "edge"):
            for component in ("main", "community"):
                for arch in ("x86_64", "aarch64"):
                    expected = f"{musl._ALPINE_CDN}/{repository}/{component}/{arch}/APKINDEX.tar.gz"
                    if url == expected:
                        return self.apkindex(repository, component, arch)
        raise AssertionError(f"unexpected URL {url}")

    def inspect(self, image: str) -> musl.ImageDigests:
        if image == "alpine:3.24.2":
            return musl.ImageDigests(
                self.stable_index,
                {"amd64": self.stable_amd64, "arm64": self.stable_arm64},
            )
        if image == "alpine:edge":
            return musl.ImageDigests(EDGE_INDEX, {"amd64": EDGE_AMD64, "arm64": EDGE_ARM64})
        raise AssertionError(f"unexpected image {image}")

    def observe(self, image: str) -> str:
        releases = {
            "alpine:3.24.2": "3.24.2",
            "alpine:edge": "3.25.0_alpha20260805",
            "alpine:3.24": "3.24.3",
        }
        return releases[image]


def install_sources(monkeypatch: pytest.MonkeyPatch, sources: Sources) -> None:
    monkeypatch.setattr(musl, "_download", sources.download)
    monkeypatch.setattr(musl, "_inspect_image", sources.inspect)
    monkeypatch.setattr(musl, "_observe_release", sources.observe)


def test_seed_lock_loads_and_round_trips() -> None:
    lock = musl.load_musl_toolchain()

    assert lock.upstream_version == "1.2.6"
    assert set(lock.channels) == {"stable", "edge"}
    assert lock.channels["stable"].packages["x86_64"]["musl"] == "1.2.6-r3"
    assert lock.channels["edge"].packages["aarch64"]["musl-dev"] == "1.2.6-r6"
    assert lock.adapters["laputa-systems/zzz"].zig_version == "0.16.0"
    assert lock.image is None
    assert musl._parse_musl_toolchain(musl.render_musl_toolchain(lock)) == lock


def test_fixture_lock_renders_identically(tmp_path: Path) -> None:
    path = write_lock(tmp_path)

    lock = musl.load_musl_toolchain(path)

    assert musl.render_musl_toolchain(lock) == path.read_text(encoding="utf-8")


def test_refresh_leaves_an_unchanged_lock_alone(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    path = write_lock(tmp_path)
    install_sources(monkeypatch, Sources())
    before = path.stat()

    target, source, observations = musl.musl_toolchain_refresh_plan(path)

    assert target == path
    assert source is None
    assert observations == ("alpine:3.24 currently releases Alpine 3.24.3",)
    assert not musl.refresh_musl_toolchain(path)
    after = path.stat()
    assert (after.st_ino, after.st_mtime_ns) == (before.st_ino, before.st_mtime_ns)


def test_refresh_accepts_unrelated_duplicate_index_records(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    path = write_lock(tmp_path)
    install_sources(monkeypatch, Sources())

    assert musl.musl_toolchain_refresh_plan(path)[1] is None


def test_refresh_rewrites_a_stale_package_version(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    path = write_lock(tmp_path)
    install_sources(monkeypatch, Sources(stable_musl="1.2.6-r4", edge_musl="1.2.6-r7"))

    assert musl.refresh_musl_toolchain(path)

    refreshed = musl.load_musl_toolchain(path)
    assert refreshed.channels["stable"].packages["x86_64"]["musl"] == "1.2.6-r4"
    assert refreshed.channels["stable"].packages["aarch64"]["musl-dev"] == "1.2.6-r4"
    assert refreshed.channels["edge"].packages["x86_64"]["musl"] == "1.2.6-r7"


def test_refresh_rewrites_a_stale_image_digest(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    path = write_lock(tmp_path)
    replacement = "sha256:" + "9" * 64
    install_sources(monkeypatch, Sources(stable_index=replacement, stable_amd64=replacement))

    assert musl.refresh_musl_toolchain(path)

    refreshed = musl.load_musl_toolchain(path)
    assert refreshed.channels["stable"].index_digest == replacement
    assert refreshed.channels["stable"].manifests["amd64"] == replacement


def test_refresh_writes_nothing_when_apkindex_is_malformed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    path = write_lock(tmp_path)
    original = path.read_text(encoding="utf-8")
    broken = f"{musl._ALPINE_CDN}/v3.24/main/aarch64/APKINDEX.tar.gz"
    install_sources(monkeypatch, Sources(broken_index=broken))

    with pytest.raises(ValueError, match="cannot read APKINDEX"):
        musl.refresh_musl_toolchain(path)

    assert path.read_text(encoding="utf-8") == original


def test_refresh_writes_nothing_when_a_fetch_fails(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    path = write_lock(tmp_path)
    original = path.read_text(encoding="utf-8")

    def fail(url: str) -> bytes:
        raise ValueError(f"cannot fetch {url}: offline")

    monkeypatch.setattr(musl, "_download", fail)

    with pytest.raises(ValueError, match="offline"):
        musl.refresh_musl_toolchain(path)

    assert path.read_text(encoding="utf-8") == original


def test_refresh_writes_nothing_when_the_release_page_is_malformed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    path = write_lock(tmp_path)
    original = path.read_text(encoding="utf-8")
    install_sources(monkeypatch, Sources(releases=b"<html>no releases</html>"))

    with pytest.raises(ValueError, match="no parseable release"):
        musl.refresh_musl_toolchain(path)

    assert path.read_text(encoding="utf-8") == original


def test_refresh_writes_nothing_when_docker_fails(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    path = write_lock(tmp_path)
    original = path.read_text(encoding="utf-8")
    install_sources(monkeypatch, Sources())

    def fail(image: str) -> musl.ImageDigests:
        raise ValueError("docker is required to refresh the musl lock")

    monkeypatch.setattr(musl, "_inspect_image", fail)

    with pytest.raises(ValueError, match="docker is required"):
        musl.refresh_musl_toolchain(path)

    assert path.read_text(encoding="utf-8") == original


@pytest.mark.parametrize(
    ("digest", "message"),
    [
        ("sha256:" + "g" * 64, "must be a sha256: digest"),
        ("sha256:" + "1" * 63, "must be a sha256: digest"),
        ("sha256:" + "1" * 65, "must be a sha256: digest"),
    ],
)
def test_lock_rejects_a_malformed_digest(
    tmp_path: Path, digest: str, message: str
) -> None:
    path = write_lock(tmp_path, lock_source(stable_index=digest))

    with pytest.raises(ValueError, match=message):
        musl.load_musl_toolchain(path)


def test_lock_rejects_a_malformed_manifest_digest(tmp_path: Path) -> None:
    path = write_lock(tmp_path, lock_source(stable_arm64="sha256:not-a-digest"))

    with pytest.raises(ValueError, match=r"channels\.stable\.manifests\.arm64"):
        musl.load_musl_toolchain(path)


def test_lock_rejects_mismatched_musl_versions(tmp_path: Path) -> None:
    source = lock_source().replace(
        '[channels.stable.packages.x86_64]\nbash = "5.3.9-r1"\nmusl = "1.2.6-r3"\nmusl-dev = "1.2.6-r3"',
        '[channels.stable.packages.x86_64]\nbash = "5.3.9-r1"\nmusl = "1.2.6-r3"\nmusl-dev = "1.2.6-r4"',
    )
    path = write_lock(tmp_path, source)

    with pytest.raises(ValueError, match=r"pins musl 1\.2\.6-r3 but musl-dev 1\.2\.6-r4"):
        musl.load_musl_toolchain(path)


def test_lock_rejects_upstream_mismatch(tmp_path: Path) -> None:
    path = write_lock(tmp_path, lock_source(stable_musl="1.2.5-r3"))

    with pytest.raises(ValueError, match=r"does not match upstream musl 1\.2\.6"):
        musl.load_musl_toolchain(path)


def test_lock_rejects_an_unknown_channel(tmp_path: Path) -> None:
    source = lock_source().replace("[channels.edge]", "[channels.beta]") + "\n"
    source = source.replace("edge =", "beta =")
    path = write_lock(tmp_path, source)

    with pytest.raises(ValueError, match=r"channels is missing required keys: edge|channels has unknown keys: beta"):
        musl.load_musl_toolchain(path)


def test_lock_rejects_an_unsupported_adapter_kind(tmp_path: Path) -> None:
    path = write_lock(
        tmp_path,
        lock_source().replace('kind = "zzz"', 'kind = "unknown"'),
    )

    with pytest.raises(ValueError, match=r"adapters\.laputa-systems/zzz\.kind must be one of"):
        musl.load_musl_toolchain(path)


def test_lock_rejects_an_unverified_archive(tmp_path: Path) -> None:
    path = write_lock(
        tmp_path,
        lock_source().replace(f"sha256 = \"{'a' * 64}\"", 'sha256 = "not-a-checksum"'),
    )

    with pytest.raises(ValueError, match="must be a 64-hex sha256 checksum"):
        musl.load_musl_toolchain(path)


def test_lock_rejects_an_adapter_path_escape(tmp_path: Path) -> None:
    path = write_lock(
        tmp_path,
        lock_source().replace(
            'zig_version = "0.16.0"',
            'zig_version = "0.16.0"\nworkflow = "../escape.yml"',
        ),
    )

    with pytest.raises(ValueError, match="must be a repository-relative path"):
        musl.load_musl_toolchain(path)
