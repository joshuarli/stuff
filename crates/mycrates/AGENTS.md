# AGENTS.md

Guidance and reference for coding agents working on `mycrates`.

## Architecture & Layout

`mycrates` is a lean, self-hosted Cargo sparse registry and dependency archive written in safe Rust.

- `src/lib.rs` / `src/main.rs`: Crate entry points. Unsafe code is strictly forbidden (`#![forbid(unsafe_code)]`).
- `src/model.rs`: Core types: `CrateName`, `Version`, `IndexEntry`, `IndexDep`, `PublishBody`.
- `src/index.rs`: Sparse registry index formatting, sharding (`1/x`, `2/xx`, `3/x/xxx`, `xx/xx/xxxx`), and `config.json`.
- `src/crate_file.rs`: Streaming structural archive scan (`scan_crate`) verifying gzip/tar integrity, prefixes, path traversal, `Cargo.toml` name/version parity, and size limits without extracting to disk.
- `src/lockfile.rs`: Parsing of Cargo.lock (v3/v4) and source classification (crates.io, alternate registry, git, path, workspace).
- `src/pins.rs`: Exact registry pin storage in `pins/<project>.toml`.
- `src/store.rs`: Content-addressed blob store (`blobs/<sha256>.crate`), atomic ingest, records (`native/`, `upstream/`), exclusing locking using std `File::lock`, and SHA-256 verification.
- `src/publish.rs`: Static generation renderer (`SiteGen`), manifest-derived canonical generation ID calculation, staging, atomic symlink switching (`current`), and crash recovery.
- `src/wire.rs`: Framing decoding for `PUT /api/v1/crates/new` (length-prefixed JSON + length-prefixed tarball).
- `src/serve.rs`: HTTP publish service (hand-rolled with `httparse`), constant-time token comparison, yank/unyank routes, and Cargo error JSON responses.
- `src/upstream.rs`: Upstream crates.io client, `dl` template expansion, bounded download streaming, and trust chain verification.
- `src/acquire.rs`: `plan`, `fetch`, and `verify` implementation.
- `src/validate.rs`: Offline verification of locked projects using Cargo proxy denial and source replacement.
- `src/cli/`: Subcommand handlers: `init`, `add`, `import-lock`, `plan`, `fetch`, `publish`, `serve-publish`, `verify`, `doctor`, `validate`.

## Development Commands

- Build: `cargo build`
- Unit and integration tests: `cargo test`
- Focused test targets:
  - `cargo test --lib`
  - `cargo test --test index_render`
  - `cargo test --test crate_file`
  - `cargo test --test lockfile`
  - `cargo test --test store`
  - `cargo test --test publish_gen`
  - `cargo test --test serve`
  - `cargo test --test cargo_native`
  - `cargo test --test acquire`
  - `cargo test --test cargo_archive`
  - `cargo test --test readme`
- Linter: `cargo clippy --locked --all-targets -- -D warnings`
