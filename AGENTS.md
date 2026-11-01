# AGENTS.md

Monorepo for four static registries sharing one Rust serving platform.

All first-party registry logic is Rust. External ecosystems are
orchestrated as subprocesses, never reimplemented: npm/pnpm for Node
resolution and installs (pinned npm 12.2.1, pnpm 12.15.0), uv/CPython 3.15
for wheel builds and installs, native toolchains plus auditwheel/delocate
for repair.

## Layout

- `crates/mycrates`: Cargo sparse registry + archive; module map in
  [crates/mycrates/AGENTS.md](crates/mycrates/AGENTS.md).
- `crates/mynpm`: npm archive (Rust binary `mynpm`).
- `crates/mypypi`: wheel index (Rust binary `mypypi`).
- `crates/stuff-oci`: OCI Distribution 1.1 protocol core (no I/O, wasm-safe):
  digests, references, media types, manifest/index parsing, storage keys,
  request parsing, response plans, upload sessions, and OCI errors.
- `crates/myoci`: OCI registry CLI and native server: import, inspect, publish,
  verify, adopt, tag, `serve`, `writer`, and `bench`; layout in
  [crates/myoci/README.md](crates/myoci/README.md).
- `crates/h12tiny-*`: HTTP/TLS client used by every registry CLI and `stuff`,
  imported with history from upstream `dcdf4f2`, manifests inlined. Keep
  `src/` byte-identical to upstream.
- `crates/stuff-store`: generation activation (`Site`, used by mycrates; mynpm
  and mypypi activate their own), immutable blob sync, durable writes.
- `crates/stuff-http`: content types, cache policy, path resolution, conditionals, ranges.
- `crates/stuff-catalog`: normalized catalog rendering, HTML, and `llms.txt`.
  The page search is a vendored MiniSearch in `web/` bundled into the
  committed `assets/search.js`.
- `crates/stuff-cargo-shard`: Cargo sparse-index shard paths shared by
  `mycrates` and the `stuff` CLI.
- `crates/stuff-server`: native static file server (blocking httparse +
  std sockets; run with `--route PREFIX=DIR`, `--max-connections N`).
- `crates/stuff-worker`: Cloudflare Worker adapter; build and architecture in
  [crates/stuff-worker/README.md](crates/stuff-worker/README.md).
- `deploy/`: pipeline checks (`check-pipeline-independence.sh`), npm fleet
  ingest (`ingest-npm-fleet.sh`),
  ecosystem smoke (`ecosystem-smoke.sh`), and R2 sync (`r2-sync.sh`;
  verify with `CF_LOCAL_STATE` via `test-r2-sync.sh`), R2 pull
  (`r2-pull.py`, verified by `test-r2-pull.sh`), local fleet
  ingestion (`fleet-ingest.sh`), OCI publish (`publish-myoci.sh`), the blob
  writer (`myoci-writer.sh`), and the pinned conformance harness
  (`oci-conformance.sh`).
- `tests/fixtures`: shared registry fixtures (lockfiles, configs,
  tarballs, wheels, and the OCI image layout at `tests/fixtures/oci/layout`).
- Publishing and operations: [docs/PUBLISHING.md](docs/PUBLISHING.md).
- Open tickets: [TODO.md](TODO.md).

## Commands

- Rust: `cargo build --workspace`, `cargo test --workspace`,
  `cargo clippy --locked --all-targets -- -D warnings`.
- One toolchain for the workspace: `rust-toolchain.toml` (nightly-2026-09-15).
- Ecosystem smoke (Node 26 + pinned npm, uv + CPython 3.15, network):
  `deploy/ecosystem-smoke.sh`. `TMPDIR="$PWD/.work/tmp"` for campaign commands.
- Deploy scripts, offline: `make test-r2` (local fakes and the R2 simulation),
  `make test-publish-lock`; `make test-r2-live` is the opt-in real-R2 check.
  OCI: `make test-oci-conformance` (read), `make test-oci-conformance-push`,
  `make test-oci-pull`, `make test-oci-push`, `make test-oci-publish`,
  and `make oci-bench`.
- Worker: `make worker-test` (bundle under workerd), `make worker-check`.
- Search: after editing `crates/stuff-catalog/web/`, run `make search-bundle`
  (needs `npm ci` in `crates/stuff-worker`) and commit the bundle;
  `make test-search` runs its tests.

## Invariants

- Apache-2.0 for stuff files; imported crates keep their own license fields.
- One registry's publication never rebuilds another's output.
- `#![forbid(unsafe_code)]` in first-party Rust crates; no lifecycle scripts during
  acquire/validate; native-target checks stay.
- npm/pnpm/uv/CPython are orchestration dependencies for resolution,
  builds and install validation only. The registry application itself
  is Rust.
- No universal `Package` abstraction. Cargo/npm/Python semantics stay
  per-ecosystem; only serving/storage/generation code is shared.
- `tests/fixtures` are the parity corpus for lockfiles, packuments,
  indexes and archives. Do not delete fixtures while Rust tests use them.
- Published generation formats stay loadable: mycrates renderer 4, mynpm
  renderer 7 with inventory schemas 2 and 3, mypypi `mypypi-simple-html-v5`,
  and OCI catalog schema 4 with generation-local repository views. A format
  change updates the owning crate README and keeps older generations readable.
