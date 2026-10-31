Prefix: STUF
Next: 29

### STUF-27 · P2 · mypypi sdist builds cannot find a Rust toolchain

- **Area:** Operability
- **Where:** `crates/mypypi/src/process_policy.rs:15-46` (allowlist), `:62-72` (`scratch_environment` rewrites `HOME`), `crates/mypypi/src/build.rs:397-410` (`build_environment`).
- **Problem:** Build and validate subprocesses run with a cleared environment containing only `ALLOWED_VARIABLES`, and `HOME` is redirected to a fresh `stage/home`. `CARGO_HOME` and `RUSTUP_HOME` never pass through, so cargo and rustup fall back to the nonexistent `$HOME/.cargo` and `$HOME/.rustup`. Any sdist with a Rust backend (maturin, setuptools-rust) fails to resolve a toolchain that the host has installed, which the ecosystem smoke misses because it only builds `six` (pure Python).
- **Recommendation:** Pass `CARGO_HOME` and `RUSTUP_HOME` through when set, or seed the scratch home from the invoking user's toolchain directories so cargo and rustup find the existing installation. Cover it in `deploy/ecosystem-smoke.sh` with a Rust-backed sdist.
- **Fix with:** `os-boundaries`
- **Effort:** M

### STUF-28 · P3 · worker unified llms.txt ignores configured origins

- **Area:** Operability
- **Where:** `crates/stuff-worker/src/api.rs:347-352` (renders with `CatalogOrigins::default()`), `crates/stuff-worker/src/wasm.rs:41-42`, `crates/stuff-worker/src/entry.ts:153`.
- **Problem:** The CLI renders `llms.txt` with the configured base URLs, but the worker's unified `/llms.txt` hardcodes `CatalogOrigins::default()`, so `STUFF_ROUTES` hosts and a custom `STUFF_UNIFIED_HOST` still produce links to `crates.17166969.xyz`, `npm.17166969.xyz`, `pypi.17166969.xyz`, and `stuff.17166969.xyz`. A deployment serving mirrors on other hosts advertises unreachable URLs. `CatalogOrigins::new` has no parameter for the unified origin either.
- **Recommendation:** Take origins in the unified render entry point, derive them from `STUFF_ROUTES` and `STUFF_UNIFIED_HOST` in `entry.ts`, let `CatalogOrigins` carry the unified origin, and add a worker test with non-default hosts.
- **Fix with:** `single-source-of-truth`
- **Effort:** S
