# Rust port baseline (Phase 0) — COMPLETE

All three registries are now Rust. `tools/mynpm` (TypeScript) and
`tools/mypypi` (Python) were removed after parity was proven; their
fixtures live on under `tests/fixtures/`.

Date: 2026-10-30. Workspace green: `cargo test --workspace --locked` passes.
Known pre-existing reds (from CAMPAIGN doc, unchanged): mynpm `test/validate.test.ts`
toolchain-mismatch (1 consistent failure); mypypi 2 pre-existing failures
(`test_dependency_resolution_uses_python_315`, `test_workflows_have_read_only_permissions`).

## Parity corpus (source of truth for ports)

1. CLI surface: `tools/mynpm/src/cli.ts` (8 commands, global flags
   `--config/--state-dir/--site-dir/--target/--project/--manager/--client/--json/--smoke-executables`),
   exit codes `src/errors.ts` (usage/input=2, operation/conflict/corrupt=1, cancelled=130);
   `tools/mypypi/src/mypypi/cli.py` (5 commands `add/plan/build/validate/publish`,
   `--catalog/--state-dir/--config/--platform/--output/--build-index/...`).
2. Registry trees + hashes: `publish.ts` / `store.py` generation layout
   (`generations/<id>/`, `current` symlink, `manifest.json`, content hashes);
   `deploy/check-pipeline-independence.sh` asserts byte-identical noop publishes.
3. Lockfiles: `test/fixtures/locks/` (npm v2/v3, pnpm v9 multi-doc) and
   `packages.toml` catalog fixtures; resolution fixtures via `uv pip compile`.
4. Index metadata: `packument.ts` renderer v3 (`current/-/tarballs/...`),
   PEP 503/658 simple pages (`index.py`), wheel METADATA/WHEEL/RECORD checks.
5. Generation IDs: content-derived sha256 prefixes; `stuff-store` activation
   (verify-before-switch, noop when `current` already verifies).
6. Failure cases: credentialed URLs, git/local/external sources, integrity
   conflicts, corrupt staging recovery, concurrent ops (covered by
   `validate.test.ts`, `test_store.py`, `test_publication.py`).
7. Live installs: `validate.ts` loopback + `npm ci`/`pnpm install --frozen-lockfile`;
   `tests/integration/` (`pip install`, `stuff-server` serve, `uv` resolution).

Bytes compared where deterministic; env-specific fields (timestamps, absolute
paths, Node patch versions) normalized per-test. Generation format changes
require a compatibility note here.

## Parity evidence (2026-10-30, this machine)

- `cargo test --workspace --locked`: green; `cargo clippy --locked
  --all-targets -- -D warnings`: clean.
- mynpm E2E with real npm 12.2.1 / Node v26.12.1: `add is-odd@3.0.1`
  (both targets) → `fetch` (2 artifacts, real registry.npmjs.org) →
  `publish` (deterministic generation, byte-identical on republish) →
  `verify` → `validate --client npm` (real `npm ci` from a loopback
  static server, lock-graph and tarball audits green). `plan --json`,
  `doctor`, pnpm `import-lock` likewise exercised.
- mypypi E2E with uv / CPython 3.15.1: `add six==1.16.0` →
  `plan`/`build` (4 targets) → `validate` (host platform; RECORD,
  isolated venv install + import test) → `publish` → real
  `uv pip install` from the published index + `import six` OK.
  Cross-platform validation correctly refuses non-native targets.
- `deploy/check-pipeline-independence.sh`: mycrates + mypypi publishes
  leave each other untouched; both republishes are noops.
- `deploy/ecosystem-smoke.sh`: both flows above, scripted for CI.
- Known env notes: npm 12 defaults `allow-remote` to `none`; the
  migrated loopback tarball URLs stay installable because the static
  server keeps them under the configured registry origin (same-origin
  exemption). pnpm 12.15.0 is CI-pinned; local runs without it skip
  the pnpm leg.
