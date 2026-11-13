# mymirror

Builds the static artifact mirror: a flat set of deterministic `.zip` downloads for
binaries and scripts whose upstreams ship in incompatible formats and layouts.

Each manifest entry names an upstream URL, the upstream SHA-256, and an optional
keep list of files to retain. The builder downloads the upstream once (verifying
the SHA-256), extracts it, drops everything outside the keep list, and re-writes
the survivors as a deterministic DEFLATE zip with fixed timestamps, Unix file
types and permissions, and symlink targets that stay inside the archive root.
Consumers only ever handle zip files.

`stuff publish mirror` is the entry point; the manifest lives in
`mirror/manifest.toml` in this repository.

## Manifest

```toml
schema_version = 1

[[artifact]]
id = "node-v26.13.1-linux-x64-musl"
version = "26.13.1"
url = "https://nodejs.org/dist/v26.13.1/node-v26.13.1-linux-x64-musl.tar.xz"
sha256 = "c2ade3fc8a670f0b110eb68c020026daf1ac57b4fceed1ca471106fc144d8720"
strip_components = 1
keep = ["LICENSE", "bin/**", "lib/**"]
```

Every entry:

- `id`: path-safe slug (`^[a-z0-9][a-z0-9._-]{0,127}$`); the published file is `<id>.zip`.
- `version`: advertised in the catalog; bumping it is a version drop like in the other registries.
- `url`: `https` only, public host. The archive format comes from the URL path:
  `.tar.gz`/`.tgz`, `.tar.xz`/`.txz`, `.zip`, anything else is stored as a raw file.
- `sha256`: 64 lowercase hex characters of the upstream bytes; verified on every fetch.
- `strip_components`: leading path components to drop from every member (default 0).
- `keep`: glob patterns (`**`, `*`, `?`) matched against member paths after stripping;
  absent keeps everything. A keep list matching nothing fails the build.

## Published layout

One immutable generation, advanced through `current` like the other registries:

```
generations/<sha256 of the canonical payload>/
  <id>.zip          one zip per entry, members at their stripped, kept paths
  catalog.json      schemaVersion, registry, generation, artifacts (sha256/bytes/files/
                    upstream provenance), and a `packages` list for the shared drop guard
  SHA256SUMS        `<zip sha256>  <id>.zip`, sorted by file name
current             pointer to the live generation
```

The generation id is a hash of the manifest plus the base URL and renderer version,
so republishing an unchanged manifest is a no-op and changed inputs produce a fresh
generation. The published `catalog.json` is the drop-guard input: entries share the
other registries' `packages` shape, so a version bump requires `--allow-drops`,
exactly like bumping a package in npm or PyPI.

Upstream hard links are rejected; extended attributes, ACLs and ownership are not
preserved. Zip timestamps are pinned to 1980-01-01 for reproducibility. The decompressor
is pure Rust and capped at 1 GiB per artifact (raw upstream cap 2 GiB).

## Local behavior

Downloads are cached by upstream SHA-256 under the mirror cache directory and
re-verified on every reuse. Extracted members are held in memory; artifacts are
written to a staging directory before being moved into the generation.
