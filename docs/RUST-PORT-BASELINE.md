# Rust port baseline (Phase 0)

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
paths) normalized per-test. Generation format changes require migration notes.

Deployment (Cloudflare/R2) is explicitly last per owner direction; no worker or
`r2-sync.sh` changes in port slices.
