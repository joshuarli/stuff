# AGENTS.md

Four static registries share one Rust serving platform and the `stuff` publisher.
The Python 3.11 standard-library `mycrates` package remains an archive/maintenance tool and publication parity oracle.

## Commands

- Build/test/lint: `cargo build --workspace`, `cargo test --workspace`, `cargo clippy --locked --all-targets -- -D warnings`.
- Rust toolchain: `rust-toolchain.toml`; Worker package/tool pins: `crates/stuff-worker/package.json` and `Cargo.lock`.
- Python oracle: `make test-mycrates` (needs Cargo for the native publication proof).
- Search: `make test-search`; after changing `crates/stuff-catalog/web/`, run `make search-bundle` and commit `assets/search.js` (install Worker dependencies first).
- Worker: `make worker-test`, `make worker-check`; build instructions are in its README below.
- Offline maintenance: `make test-r2`, `make test-publish-lock`; `make test-r2-live` accesses real R2 and is opt-in.
- Ecosystem integration: `deploy/ecosystem-smoke.sh` needs the pinned package managers, CPython, native repair tools, and network. Use `TMPDIR="$PWD/.work/tmp"` for campaign commands.
- OCI checks: `make test-oci-conformance`, `make test-oci-conformance-push`, `make test-oci-pull`, `make test-oci-push`, `make test-oci-publish`, `make oci-bench`.

## Topic map

| Topic | Owner |
| --- | --- |
| Publishing, credentials, state, deployment, recovery | [docs/PUBLISHING.md](docs/PUBLISHING.md); native implementation in `crates/stuff/src/publish/` |
| Cargo archive and Python oracle | [crates/mycrates/AGENTS.md](crates/mycrates/AGENTS.md) |
| Cargo formats and shared catalog schema | [docs/mycrates-contracts.md](docs/mycrates-contracts.md) |
| Legacy Cargo state migration | [docs/mycrates-migration.md](docs/mycrates-migration.md) |
| npm inventory and publication | [crates/mynpm/README.md](crates/mynpm/README.md) |
| Wheel acquisition and validation | [crates/mypypi/README.md](crates/mypypi/README.md) |
| OCI protocol and storage | [docs/oci-design.md](docs/oci-design.md); [protocol API](crates/stuff-oci/README.md) |
| OCI CLI, server, writer, benchmarks | [crates/myoci/README.md](crates/myoci/README.md) |
| Worker host, WASM build, runtime tests | [crates/stuff-worker/README.md](crates/stuff-worker/README.md) |
| Shared storage and generation activation | `crates/stuff-store/src/`; `Site` is used by myoci, while mynpm/mypypi activate their own sites |
| Shared HTTP policy and native adapter | `crates/stuff-http/src/`, `crates/stuff-server/src/` |
| Catalog rendering and search | `crates/stuff-catalog/src/`, `crates/stuff-catalog/web/` |
| URL and Cargo shard parsing | `crates/stuff-url/src/lib.rs`, `crates/stuff-cargo-shard/src/lib.rs` |
| Maintenance helpers and offline fakes | `deploy/`; commands in `Makefile` |
| Deferred fixes and draft designs | [TODO.md](TODO.md) and its linked RFCs |

## Invariants

- `stuff publish cargo|npm|pypi|oci` is the publication entry point; the Python Cargo writer stays only as a parity oracle.
- One registry's publication never rebuilds another registry's output.
- Keep package semantics per ecosystem; share serving/storage/generation code without a universal `Package` abstraction.
- First-party Rust forbids unsafe code; Python mycrates uses only the standard library.
- Keep npm lifecycle scripts disabled during acquisition and validation; retain native-target execution checks for wheels.
- Preserve published generation readability; format contracts belong in the owning registry docs.
- Keep fixtures while parity consumers use them, including the executable examples in `crates/mycrates/README.md`.
- Keep `crates/h12tiny-*` sources byte-identical to imported upstream `dcdf4f2`; change upstream before updating the import.
- Stuff files use Apache-2.0; imported crates retain their license fields.
