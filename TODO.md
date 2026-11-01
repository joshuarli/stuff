Prefix: STUF
Next: 50

### STUF-45 · P3 · mypypi sdist builds run third-party code with the publisher's filesystem

- **Area:** Security
- **Where:** `crates/mypypi/src/build.rs:488-510`, `crates/mypypi/src/process_policy.rs:15-72`, `deploy/publish-mypypi.sh:105`
- **Problem:** `uv build` runs the sdist's build backend with an allowlisted environment, scratch homes, and a process-group timeout, but no filesystem or network isolation: it runs as the publisher and can read `~/.ssh`, the `cf` keyring, and other credential files by absolute path. On macOS the build runs on the host. `--require-build-hashes` and `--build-constraints` exist but `publish-mypypi.sh` passes neither. Inputs are exact pins, so this is supply-chain exposure, not an open hole.
- **Recommendation:** Pass `--require-build-hashes` from the publish helper and decide whether macOS sdist builds move to a credential-free user or account.
- **Fix with:** security
- **Effort:** S

### STUF-48 · P4 · Dead code and no-op statements across the registries

- **Area:** Dead code
- **Where:** `crates/stuff-worker/src/map.rs:14-25`, `crates/stuff-worker/src/lib.rs:9-11`, `crates/mycrates/src/model.rs:186`, `crates/mycrates/src/store.rs:99`, `crates/mycrates/src/store.rs:121-123`, `crates/mypypi/src/build.rs:117-119`, `crates/mypypi/src/store.rs:784`, `crates/mypypi/src/store.rs:849-862`, `crates/mypypi/src/process_policy.rs:160-162`, `crates/mynpm/src/validate.rs:1925`, `crates/mynpm/src/validate.rs:1971`, `crates/mynpm/src/validate.rs:2064`, `crates/mycrates/.github/workflows/ci.yml`
- **Problem:** `map::RouteKind` is never read and shadows `stuff_http::RouteKind` through a public re-export. `UpstreamIndexLine` has no references, `try_lock_exclusive` is test-only, and the `.upload.` cleanup in `blobs/` serves a layout only a `legacy` test creates. mypypi's `void_is_zip_check`, `let _ = rows`, an identity `Kind` match, and a one-line `redact_credentials` wrapper do nothing, nor do mynpm's `let _ = &mut args`-style lines. The imported mycrates workflow never runs (GitHub reads only the root `.github/`) and names toolchain 1.99.1.
- **Recommendation:** Delete them.
- **Fix with:** delete
- **Effort:** S

### STUF-49 · P4 · stuff doctor panics on a non-ASCII crate name

- **Area:** Errors
- **Where:** `crates/stuff-cargo-shard/src/lib.rs:12-17`, `crates/stuff/src/doctor.rs:107`
- **Problem:** `shard_for_name` slices by byte index, so a multibyte character at a slice boundary panics. `stuff doctor` feeds it names from a remote or local catalog, so a corrupt catalog crashes the health check instead of failing one check.
- **Recommendation:** Return `Option` or reject non-ASCII names before sharding.
- **Fix with:** error-model
- **Effort:** S
