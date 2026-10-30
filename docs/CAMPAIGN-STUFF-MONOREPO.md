# Campaign: stuff monorepo

## Status

Goal: one `stuff` repo serving mycrates/mynpm/mypypi from shared Rust HTTP
serving + storage/generation code; per-registry pipelines stay independent.
S0 done: mycrates green at HEAD `f96961d` (cargo test all ok, 0 failed).
Accepted: S1, S2d, S2a, S2b, S2c (mypypi; ruff clean, pytest
163 passed with the same 2 pre-existing reds as source HEAD).
D1 done: histories reachable via merge parents, licenses present.
S3 done: stuff-store (site/blob/hash/fsync) + stuff-http (mime/cache/
route/condition) extracted; mycrates publishes through them, 125 passed
0 failed workspace-wide, clippy clean. S4 done: stuff-server on the
mycrates blocking stack; real cargo/npm/uv install from it, 135 passed
0 failed, clippy clean. S5 done: stuff-worker core (asset-map, 7 tests)
+ wasm glue compiles to wasm32 (debug + release 1.1 M pre-opt);
cf migrate validates wrangler.toml into committed cloudflare.config.ts
with 3 R2 bindings. S6 done: independence script green locally + CI job
wired, README/AGENTS.md current, `r2-sync.sh` uploads with the `current`
marker (proven against local R2 sim). S7 done: independent review (no P1,
5 P2 fixed: blob message contracts, content-length literals, fd-open
serving with symlink refusal, verify-staging-first activation, r2-sync
crash safety); full milestone suites green. Campaign complete.
Owner answers (2026-10-30): import after mycrates lands; Apache-2.0
everywhere; CF build-only, owner deploys via `cf`; old repos stay live.
Prescribed in flight: S2d subdir import (registry 0.6.0 predates local TLS
changes); empty workspace proves via `cargo metadata`.

## Charter

- Decide and keep going. Log a decision only when a later slice needs it.
- Proceed inside the execution grant; park anything needing missing authority.
- Hard stops (park, never push past): push/delete/archive; touching hosts,
  credentials, or machines outside this repo; adding a dependency outside the
  permitted list; a loop with no local check.
- Fix egregious breakage met on the way in its own commit when the fix is
  under 30 lines; otherwise log it and leave it.
- Parity before optimization. Budgets are logged, gated only on >50% regress.
- One writer by default. Parallel lanes only for the disjoint pairs in
  Sequence, in worktrees at `.work/wt/<lane>`, never `/tmp`.
- Workspace-wide changes (versions, lints, lockfiles, CI) are serial and
  coordinator-owned; import lanes never touch them.
- Every commit is Conventional Commit with workstream scope; item ID in body.
  Code comments never cite this doc.

## Decisions

Owner (goal text + 2026-10-30 answers):
- Unify infrastructure, not implementations. Keep all three CLI names; no
  unified `stuff add`; no universal `Package` type or shared validation.
- Shared: HTTP serving, R2/filesystem storage interface, atomic generation
  activation, deployment/rollback, Range/ETag streaming. Per registry:
  validation, resolution, index/packument metadata.
- One registry's publication never rebuilds or republishes another's output.
- Milestones: monorepo, shared read server, Cloudflare deployment.
- Native Cargo publish stays (CF 100 MB request cap blocks parity); CF
  covers the static read path first.
- Apache-2.0 for the whole monorepo (mynpm gains LICENSE + package field).
- CF worker ships built, owner deploys with `cf`; old repos stay live.
- Transport: same crates mycrates has, no tokio (owner, S4).
- Deploy: wrangler.toml is the source of truth; `cf migrate` generates the
  committed cloudflare.config.ts + wrangler.config.ts (validated S5).
  Entrypoint is worker-build output `build/worker/shim.mjs`. R2 layout per
  registry is the published site root; the `current` symlink becomes a
  marker object holding the generation id (strict `[A-Za-z0-9_-]{1,128}`).

Prescribed (evidence, overrulable):
- Import via `git fetch` + `git read-tree --prefix=` + merge; the `git
  subtree` subcommand is absent here (proven in `.work/tmp/prefix-demo`).
- Common interface is small: store immutable object, activate generation,
  resolve request, return object. Nothing wider.
- Native server uses the same crates mycrates has (httparse + std
  blocking sockets, thread-per-connection with a cap, per owner
  direction 2026-10-30); no async runtime. The hand-rolled `httparse`
  loop (`crates/mycrates/src/serve.rs:313-366`) stays write-path only.
- Keep in force: `#![forbid(unsafe_code)]`, no lifecycle scripts during
  acquire/validate, native-target build checks, exclusive file locks.
- h12tiny path deps stay on exact HEAD code via S2d subdir import: registry
  0.6.0 predates local TLS trust changes (proven by diff), so a version swap
  would regress behavior. Imported crates keep MIT license fields.

Delegated (bound + tie-break):
- Exact `stuff-*` crate split within `crates/`; tie-break: fewest crates
  that still separate http semantics, store, server, worker.
- `cf` deploy-config format; bound: must be valid `cf` input or park S5b.
- Wasm adapter details; tie-break: smallest code serving all three layouts.

## Baseline (snapshot 2026-10-30; mycrates tree actively edited, numbers may shift)

- mycrates: 30 dirty files mid serde->deser/semver migration; lib target
  failed once with E0599 (`Version::semver` removed in `src/model.rs`, caller
  remains); full `cargo test` passed minutes earlier on warm cache. HEAD
  state: green assumed, unverified by me. Loop: `cargo test --lib` 0.01 s
  test time; full suite completes in minutes warm.
- mynpm: `pnpm exec vitest run` 15.65 s test time; 1 consistent failure in
  `test/validate.test.ts` (pinned toolchain-mismatch, env-sensitive); 6
  failed once on a full `pnpm check` run (flaky). Loop: single-file
  packument run 1.7 s wall.
- mypypi: `uv run --locked --python 3.15 pytest -q -m "not network"
  tests` 3.73 s: 163 passed, 2 failed
  (`test_dependency_resolution_uses_python_315...`,
  `test_workflows_have_read_only_permissions...`), 1 skipped.
- Foundation proofs (rung 4, ran real code): dumb static server serves a
  mycrates generation through the `current` symlink as `application/json`
  (200 both paths); prefix import works without `git subtree`.
- Release budget (S2a, workspace = mycrates + 3 h12tiny crates):
  `cargo build --release --workspace` 10.6 s wall, `target/release` 137 M,
  `mycrates` binary 5.4 M. Gate: flag >50% growth in S3-S5 reviews.
- Release budget (S4, adds stuff-store/http/server): `target/release`
  148 M (+8%, under gate), `stuff-server` binary 560 K. Deliberate
  deviations from the TestServer reference: size+mtime etags (no
  per-request hashing) and directory index.html serving for PEP 503.
- Release budget (S5): wasm32 release `stuff_worker.wasm` 1.1 M before
  worker-build's wasm-opt; owner verifies the deployed bundle size.

## Sequence

- S0 prerequisite (owner, not a slice): mycrates green at HEAD. No slice
  starts on a red baseline.
- S1 skeleton. Owns: `Cargo.toml`, `tools/`, `.github/workflows`, `AGENTS.md`.
  Acceptance: `cargo metadata --format-version 1 --no-deps` green, CI and
  guide present (`build/test --workspace` take over at S2a). Gates the rest.
- S2a/b/c/d imports (parallel OK only for b+c: disjoint paths, shared
  nothing; a+d serial: shared `Cargo.toml`, path rewrite).
  S2a owns `crates/mycrates`; S2b owns `tools/mynpm`; S2c owns `tools/mypypi`;
  S2d owns `crates/h12tiny-*` (client-sync, client-normalize, tls-system
  subdirs with history; `workspace = true` inlined to h12tiny HEAD versions,
  `src/` untouched).
  Prereq: S1 (d after S2a's path lands, or with it; b+c anytime). Acceptance
  per slice: history present (`git log -- crates/mycrates` non-empty),
  suite passes from the new path with only baseline known-reds, Apache-2.0
  file present. Record release build time + size here as budget.
- S3 shared crates (serial; owns `crates/stuff-*`). Prereq: S2a. Acceptance:
  `cargo test -p stuff-store -p stuff-http`; mycrates store/publish/index
  suites pass against extracted code. Exit test for infra: S4 pilot below.
- S4 native static read server (owns `crates/stuff-server`; serial after S3).
  The end-to-end pilot. Acceptance: real clients install from it —
  `cargo_compat` (cargo fetch+build, mirrors cargo_native consume flow),
  `npm_compat` (npm install + npm ci, mirrors static-install seed flow),
  `test_stuff_server.py` (uv pip install from a published index),
  plus `http_compat` semantics (200/206/304/404/405/412/416, HEAD,
  etag/cache/range, `%2f`, dir index.html). All green.
- S5 worker + deploy config (owns `crates/stuff-worker`, `deploy/`).
  Prereq: S4. Acceptance: wasm bundle builds; asset-map tests map all
  three layouts (incl. `%2f` scoped routes); `cf` config parses if the tool
  is present. Real deploy is Needs environment.
- S6 pipeline independence + docs (owns `docs/`, CI scoping). Acceptance:
  publishing one registry in CI leaves other outputs byte-identical;
  README/AGENTS.md updated together with code.
- S7 close: remove reference shims, `ultrareview`, fix P1/P2, final report.
  Full suites at S4, S5, S7 milestones only; slices use their own command.

Failing item: diagnose cause first (root-cause), retry once with a changed
approach, then park on its branch with cause + retry condition. Shared-env
blocker gets one owner.

## Acceptance

- D1 monorepo: `git log --oneline -- <each path>` non-empty AND
  `cargo test` (crates/mycrates) AND `pnpm check` (tools/mynpm) AND
  `pytest -q -m "not network"` (tools/mypypi) green from new paths,
  baseline known-reds excepted.
  Fail: any NEW red vs source HEAD, history missing, license missing.
- D2 shared serving/storage: `cargo test --workspace` green AND S4 pilot
  clients green. Fail: behavior differs from pre-extraction output on the
  same fixtures (existing suites are the parity corpus).
- D3 native read server: serves mycrates sparse index + blobs, npm
  packuments + sha512 tarballs, pypi simple pages + wheels, with correct
  content-type/cache headers and `%2f` routes. Proved by the S4 compat
  tests (real cargo/npm/uv clients), not by curl spot checks.
- D4 Cloudflare: wasm bundle builds; layout mapping tests pass; `cf`
  deploy runs from Needs environment. Fail: any layout 404s in mapping.
- D5 independent pipelines: two consecutive single-registry publishes leave
  other generations untouched (hash compare). Fail: cross-registry rebuild.

## Needs environment

- `cf` deploy to a test zone + smoke (owner runs; executor ships config).
- Cross-arch wheel builds/validations (linux-aarch64, macos targets need
  native hosts); CI may cover, executor logs and moves on.
- CF 100 MB upload-limit check for native publish parity (deferred by design).

## Execution boundary and non-goals

Authorized: worktrees under `.work/wt/`; test/build commands; commits on the
integration branch; Hyper/workers-rs/wasm deps (perf impact measured and
logged). Forbidden: push, archive/delete repos, credentials/hosts outside the
repo, any other dependency, formatters, full-suite runs outside milestones.
Contract changes: none yet; the `current`-symlink generation layout is
preserved byte-for-byte for all three registries.
Non-goals: unified CLI, shared validation/resolution/metadata, Cargo
publish-over-CF, wrangler configs, archiving old repos, optimization work
before S7.

## Reference

Pre-import verified map (post-import: `crates/mycrates/*`,
`tools/mynpm/*`, `tools/mypypi/*`):
mycrates publish API `PUT /api/v1/crates/new` (`src/serve.rs:131`),
yank/unyank (`src/serve.rs:276`); sharding (`src/index.rs:5`); symlink guard
+ generation switch (`src/publish.rs:52-65,332-385`); store dirs/locks
(`src/store.rs:22-67,77-99`). mynpm packument render + `%2f`
(`src/packument.ts:137-158`); generation switch (`src/publish.ts:590`);
loopback server + audit (`src/validate.ts:212-238,1096-1121`). mypypi
promotion (`src/mypypi/store.py:488-506`); generation hash
(`src/mypypi/store.py:400-410`); project pages (`src/index.py:205`).
