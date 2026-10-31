# AGENTS.md

Monorepo for three static registries sharing one Rust serving platform.

All first-party registry logic is Rust. External ecosystems are
orchestrated as subprocesses, never reimplemented: npm/pnpm for Node
resolution and installs (pinned npm 12.2.1, pnpm 12.15.0), uv/CPython 3.15
for wheel builds and installs, native toolchains plus auditwheel/delocate
for repair.

## Layout

- `crates/mycrates`: Cargo sparse registry + archive (Rust, nightly-2026-09-15).
- `crates/mynpm`: npm archive (Rust binary `mynpm`).
- `crates/mypypi`: wheel index (Rust binary `mypypi`).
- `crates/h12tiny-*`: TLS client deps of mycrates, imported history, manifests
  inlined (no workspace root). Keep `src/` byte-identical to upstream.
- `crates/stuff-server`: native static file server (blocking httparse +
  std sockets; run with `--route PREFIX=DIR`, `--max-connections N`).
- `crates/stuff-worker`: Cloudflare Worker adapter; the wrangler/cf
  project lives next to the crate (`cf deploy --dry-run` validates).
- `deploy/`: pipeline checks (`check-pipeline-independence.sh`), npm fleet
  ingest (`ingest-npm-fleet.sh`),
  ecosystem smoke (`ecosystem-smoke.sh`), and R2 sync (`r2-sync.sh`;
  verify with `CF_LOCAL_STATE` via `test-r2-sync.sh`), R2 pull
  (`r2-pull.py`, verified by `test-r2-pull.sh`), and local fleet
  ingestion (`fleet-ingest.sh`).
- `tests/fixtures`: shared registry fixtures (lockfiles, configs,
  tarballs, wheels) kept after the TypeScript/Python removal.

## Commands

- Rust: `cargo build --workspace`, `cargo test --workspace`,
  `cargo clippy --locked --all-targets -- -D warnings`.
- Ecosystem smoke (Node 26 + pinned npm, uv + CPython 3.15, network):
  `deploy/ecosystem-smoke.sh`. `TMPDIR="$PWD/.work/tmp"` for campaign commands.
- Python: `uv run --locked --python 3.15 pytest -q -m "not network" tests`
  (retired with `tools/mypypi`; kept here only for historical reference).

## Invariants

- Apache-2.0 for stuff files; imported crates keep their own license fields.
- One registry's publication never rebuilds another's output.
- `#![forbid(unsafe_code)]` in Rust crates; no lifecycle scripts during
  acquire/validate; native-target checks stay.
- npm/pnpm/uv/CPython are orchestration dependencies for resolution,
  builds and install validation only. The registry application itself
  is Rust.
- No universal `Package` abstraction. Cargo/npm/Python semantics stay
  per-ecosystem; only serving/storage/generation code is shared.
- `tests/fixtures` are the parity corpus for lockfiles, packuments,
  indexes and archives. Do not delete fixtures while Rust tests use them.
