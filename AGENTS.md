# AGENTS.md

Monorepo for three static registries sharing one Rust serving platform.

## Layout

- `crates/mycrates`: Cargo sparse registry + archive (Rust, nightly-2026-09-15).
- `crates/h12tiny-*`: TLS client deps of mycrates, imported history, manifests
  inlined (no workspace root). Keep `src/` byte-identical to upstream.
- `tools/mynpm`: npm archive (TypeScript 7 ESM, Node 26, pnpm 12.15.0).
- `tools/mypypi`: wheel index (CPython 3.15, uv).
- `crates/stuff-*`: shared Rust serving/storage (from S3 on).
- `deploy/`: Cloudflare `cf` config (from S5 on).

## Commands

- Rust: `cargo build --workspace`, `cargo test --workspace`,
  `cargo clippy --locked --all-targets -- -D warnings` (per crate from S2).
- npm: `cd tools/mynpm`, `pnpm install --frozen-lockfile --ignore-scripts`,
  `pnpm check`. `TMPDIR="$PWD/.work/tmp"` for campaign commands.
- Python: `cd tools/mypypi`,
  `uv run --locked --python 3.15 pytest -q -m "not network" tests`.

## Invariants

- Apache-2.0 for stuff files; imported crates keep their own license fields.
- One registry's publication never rebuilds another's output.
- `#![forbid(unsafe_code)]` in Rust crates; no lifecycle scripts during
  acquire/validate; native-target checks stay.
- Nested `tools/*/.github/workflows` are inert references for area tests;
  real CI lives in `.github/workflows`.
