# Handoff: verify native-to-native crate publishes

## Goal and authority

Fix `mycrates` so `cargo publish --registry mycrates` can verify a crate that depends on another crate in the native registry. Today that verification fails with a 401 while downloading the dependency's blob, so the dependent crate has to be published with `--no-verify`.

- **Required behavior:** publishing a crate whose dependency is already in `current/registry/` succeeds with normal cargo verification. Write authentication is unchanged: no token, no publish.
- **Recommended behavior:** the token protects writes; reads follow the registry's `auth_required` value, and the generated `current/registry/config.json` reports the same value so cargo knows whether to send a token. Default `false` keeps published reads open.
- **Permitted:** edit `crates/mycrates/src` and `crates/mycrates/tests`, `crates/mycrates/README.md`, `docs/PUBLISHING.md`, and `deploy/publish-mycrates.sh` if the chosen design needs it; run tests and clippy; commit locally.
- **Non-goals:** do not republish `age`/`age-core`, sync R2, rotate tokens, change Worker routing, or try to make the public static site enforce read auth. Do not publish test packages to the live registry.
- **Acceptance:** an integration test publishes a native dependency and a dependent crate without `--no-verify`, then consumes both. It fails on the current HEAD and passes after the fix. `deploy/publish-mycrates.sh` needs no `--no-verify` escape hatch.

## Workspace

- Repository: `/Users/josh/d/stuff`, branch `main`, HEAD `37777b9` (`docs: add publishing checklist`), clean tree at handoff.
- Toolchain: `nightly-2026-09-15` (see `rust-toolchain.toml`).
- Live publication state: `~/.local/state/stuff/mycrates` at generation `60d0eb12dd768fa21e11d1fe`. It contains native `age 0.12.1` and `age-core 0.12.0`; leave it alone.
- Primary docs: `README.md`, `docs/PUBLISHING.md`, `crates/mycrates/README.md`. The deploy helper is `deploy/publish-mycrates.sh`; its lock wrapper is `deploy/publish-lock.sh`.

## Decisions and evidence

**Root cause** (verified against the current source):

- `crates/mycrates/src/index.rs:22-30`: `native_config_json` hardcodes `"auth-required":false` and takes only the base URL. `mirror_config_json` (`:32-35`) emits only `dl`, which is fine for the public crates.io mirror.
- `crates/mycrates/src/serve.rs:112-129`: the first request gate requires the token for **every** request when a token file is configured (`cfg.auth_required || cfg.token.is_some()`), including GET/HEAD blob downloads. The PUT-specific gate at `:132-147` already requires the token when configured.
- `crates/mycrates/src/publish.rs:105-118`: renders both config files; `native_config_json` gets no auth value from `config.auth_required`.
- `deploy/publish-mycrates.sh`: starts `serve-publish` with `--token-file`, sets `public_base_url` to the publish server, and the generated index's `dl` therefore points at that server. Cargo fetches a native dependency's `.crate` from there without an `Authorization` header because the index advertises `auth-required:false`, and the read gate rejects it with 401.

**Reproduction** (observed on 2026-10-31, throwaway state/site):

1. Publish native `rt-child 0.1.0` with `cargo publish --registry mycrates`.
2. Publish `rt-parent 0.1.0` with `rt-child = { version = "0.1.0", registry = "mycrates" }`.
3. `cargo publish -p rt-parent --registry mycrates` fails during `Verifying`:

```
failed to download from `http://127.0.0.1:8080/blobs/<sha256>.crate`
... got 401
body: {"errors":[{"detail":"unauthorized"}]}
```

`cargo publish --no-verify` succeeds. A consumer then resolves both crates from a different base URL and builds, so the packaged `registry-index` pointing at the transaction-local index does not break consumption. The only missing piece is the blob GET during verification.

**Design decision to settle:** `auth_required` currently has no effect on advertised reads and only affects the publish server's write gate when no token is configured. Pick one:

- **Recommended:** plumb `config.auth_required` into `native_config_json` (emit `"auth-required":<bool>`), and make the read gate depend on `cfg.auth_required`, not on `token.is_some()`. Writes still require the token whenever one is configured. This makes both modes self-consistent for cargo. Note that a `true` value is only meaningful where reads are actually protected; the public Worker does not enforce read auth, so the deployed registry must stay `false`.
- **Smaller:** keep the generated config `false` forever, drop `token.is_some()` from the read gate, and document `auth_required` as a write-only setting (or remove it). Simpler, but leaves the existing true-mode tests and README misleading.

Do not "fix" this by adding `--no-verify` to `publish-mycrates.sh`; that hides the failure and leaves consumers of the published crate untested.

## Verified progress

- No code changes in `stuff` yet; the repository is clean.
- Native `age 0.12.1` and `age-core 0.12.0` are published and consumed successfully by the vault build, so the artifact path is proven; the failure is confined to publish-time verification of a dependent crate.
- Throwaway end-to-end experiment confirmed: dependent publish works with `--no-verify`, the index records `"registry":null` for the native dependency, and a consumer at a different URL resolves it. The experiment's state and servers were deleted.

## Next action

1. Settle the `auth_required` semantics above.
2. Implement the chosen behavior. If plumbing: add the parameter to `native_config_json`, pass `config.auth_required` from `publish.rs`, update `Publisher::new` callers (`serve.rs`, `cli/publish.rs`, `tests/support/fixture.rs`, tests), and make the read gate use `cfg.auth_required`.
3. Add tests:
   - Unit: `native_config_json` emits `"auth-required":true` and `false` accordingly.
   - Server: with a token and `auth_required:false`, PUT requires the token and an unauthenticated GET of a published blob returns 200; with `auth_required:true`, GET without a token returns 401 and with the token returns 200.
   - End-to-end: add a native-dependency case to `crates/mycrates/tests/publish_deps_e2e.rs` (reuse `make_crate_project`, `CargoEnv`, and the `registry = "mycrates"` dependency form) that publishes the dependency, publishes the dependent crate **without** `--no-verify`, and then resolves/builds a consumer. Exercise a second base URL for the consumer if practical, to keep the transaction-local `registry-index` from regressing.
4. Run `cargo test -p mycrates` and `cargo clippy --locked --all-targets -- -D warnings` in `~/d/stuff`.
5. Update the mycrates README auth/private-registry section and `docs/PUBLISHING.md` to state what the token protects and how `auth_required` behaves.
6. Commit one coherent change. Do not run a live publish from this task.

**Report:** the chosen semantics, files changed, the failing test result on the old code and the passing result after, checks run, and any remaining limitation (especially that the public static site cannot enforce read auth).
