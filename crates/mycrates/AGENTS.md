# AGENTS.md

`mycrates` is a self-hosted Cargo sparse registry and dependency archive in safe Rust. Build, test, and lint commands are the workspace ones in the root [AGENTS.md](../../AGENTS.md); each file in `tests/` is a target (`cargo test -p mycrates --test <name>`), and `tests/readme.rs` runs the README's `sh verified` blocks.

## Module map

- `src/model.rs`: core types `CrateName`, `Version`, `IndexEntry`, `IndexDep`, `PublishBody`; `src/version.rs` holds the SemVer `Version` and `VersionReq` it wraps.
- `src/config.rs`: `config.json` in the state directory, including `limits`.
- `src/error.rs`: the `Error` kinds and their exit codes.
- `src/json.rs`, `src/toml.rs`: thin `deser` wrappers that map parse errors to `Error`.
- `src/index.rs`: sparse index lines and `config.json`; shard paths come from `stuff-cargo-shard`.
- `src/crate_file.rs`: `scan_crate_staged`, the single archive gate for uploads and upstream downloads (gzip/tar integrity, entry types, path traversal, `name-version/` prefix, `Cargo.toml` name/version parity, size and ratio limits), without extracting to disk.
- `src/lockfile.rs`: Cargo.lock (v3/v4) parsing and source classification (crates.io, alternate registry, git, path, workspace).
- `src/pins.rs`: exact registry pins in `pins/<project>.toml`.
- `src/store.rs`: content-addressed blob store (`blobs/<sha256>.crate`), records (`native/`, `upstream/`), the exclusive store lock (`File::lock`), and SHA-256 verification.
- `src/adopt.rs`: rebuild records and blobs from a published site (`mycrates adopt`).
- `src/publish.rs`: generation rendering, the manifest-derived generation id, and activation through `stuff_store::Site`.
- `src/wire.rs`: framing for `PUT /api/v1/crates/new` (length-prefixed JSON plus tarball).
- `src/serve.rs`: the `serve-publish` HTTP service (`httparse`), constant-time token comparison, yank/unyank, and Cargo error JSON.
- `src/upstream.rs`: crates.io client, `dl` template expansion, bounded downloads, and the download SHA-256 check.
- `src/acquire.rs`: `plan`, `fetch`, and `verify`, including the trust chain check (lockfile checksum equals index `cksum`).
- `src/validate.rs`: offline validation of locked projects through Cargo proxy denial and source replacement.
- `src/cli.rs`, `src/cli/`: one handler per subcommand.
