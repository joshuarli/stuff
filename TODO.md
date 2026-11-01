Prefix: STUF
Next: 50

### STUF-45 · P3 · mypypi sdist builds run third-party code with the publisher's filesystem

- **Area:** Security
- **Where:** `crates/mypypi/src/build.rs:488-510`, `crates/mypypi/src/process_policy.rs:15-72`, `deploy/publish-mypypi.sh:105`
- **Problem:** `uv build` runs the sdist's build backend with an allowlisted environment, scratch homes, and a process-group timeout, but no filesystem or network isolation: it runs as the publisher and can read `~/.ssh`, the `cf` keyring, and other credential files by absolute path. On macOS the build runs on the host. `--require-build-hashes` and `--build-constraints` exist but `publish-mypypi.sh` passes neither. Inputs are exact pins, so this is supply-chain exposure, not an open hole.
- **Recommendation:** Pass `--require-build-hashes` from the publish helper and decide whether macOS sdist builds move to a credential-free user or account.
- **Blocked:** `--require-build-hashes` without a hash-pinned build-constraints file fails every build (`Hash-checking is required, but no trusted hash was provided for: setuptools`), so turning it on needs a maintained constraints file; macOS isolation needs an owner decision on a credential-free user or account. Unblock: supply the constraints file and the account decision.
- **Fix with:** security
- **Effort:** S

### STUF-50 · P4 · Smoke script and fixtures repeat the manager pins

- **Area:** Config
- **Where:** `deploy/ecosystem-smoke.sh:6`, `deploy/ecosystem-smoke.sh:47`, `deploy/ecosystem-smoke.sh:54`, `crates/mynpm/tests/inventory_identity.rs:34`
- **Problem:** `Manager::pinned_version` is now the one place the npm and pnpm versions live, but the smoke script still compares pnpm against a literal `12.15.0` and names npm `12.2.1` in its requirements, and one inventory test builds a literal `pnpm@12.15.0` pin. Bumping the pin silently skips the pnpm smoke flow instead of failing, and the test keeps validating the old pin.
- **Recommendation:** Print the pins from the `mynpm` binary (for example a `mynpm pins` command used by the smoke script) and build the test pin with `Manager::Pnpm.pinned_version()`.
- **Fix with:** single-source-of-truth
- **Effort:** S

### STUF-51 · P4 · Only sdist builds check the pinned uv version

- **Area:** Config
- **Where:** `crates/mypypi/src/build.rs:28`, `crates/mypypi/src/add.rs:185`, `crates/mypypi/src/validate.rs:604`, `crates/mypypi/src/validate.rs:690`
- **Problem:** `UV_VERSION` is checked inside `build_inner`, but `mypypi add` resolves pins with uv and `validate` installs with uv before any build runs. A host with another uv can rewrite the catalog and then fail its first build, leaving state resolved by the unpinned version.
- **Recommendation:** Require the pinned uv once at CLI start for `add`, `build`, and `validate`.
- **Fix with:** single-source-of-truth
- **Effort:** S
