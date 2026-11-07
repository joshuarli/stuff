Prefix: STUF
Next: 62

### STUF-45 · P3 · mypypi sdist builds run third-party code with the publisher's filesystem

- **Area:** Security
- **Where:** `crates/mypypi/src/build.rs:488-510`, `crates/mypypi/src/process_policy.rs:15-72`, `crates/stuff/src/publish/pypi.rs`
- **Problem:** `uv build` runs the sdist's build backend with an allowlisted environment, scratch homes, and a process-group timeout, but no filesystem or network isolation: it runs as the publisher and can read `~/.ssh`, the `cf` keyring, and other credential files by absolute path. On macOS the build runs on the host. `--require-build-hashes` and `--build-constraints` exist but `stuff publish pypi` passes neither. Inputs are exact pins, so this is supply-chain exposure, not an open hole.
- **Recommendation:** Pass `--require-build-hashes` from `stuff publish pypi` and decide whether macOS sdist builds move to a credential-free user or account.
- **Blocked:** `--require-build-hashes` without a hash-pinned build-constraints file fails every build (`Hash-checking is required, but no trusted hash was provided for: setuptools`), so turning it on needs a maintained constraints file; macOS isolation needs an owner decision on a credential-free user or account. Unblock: supply the constraints file and the account decision.
- **Fix with:** security
- **Effort:** S

### STUF-58 · P3 · Publish the LLVM 23.1.5 canonical musl image and its digest

- **Area:** Deploy
- **Where:** `repomod/musl-toolchain.toml:210`, `repomod/docker/musl/Dockerfile:7`
- **Problem:** The lock and canonical builder now pin `llvm-musl-23.1.5-4c2254e`, but `[image].digest` still names the published image built with `23.1.0-rc2`. An image-mode builder would silently keep the old toolchain. Nothing uses image mode today, and the local token has no `write:packages`, so publication was deferred.
- **Recommendation:** Build `docker/musl/Dockerfile` for `linux/amd64` and `linux/arm64`, run its smoke test, publish the manifest to `ghcr.io/joshuarli/musl-toolchain`, and replace `[image].digest` with the new multi-arch manifest digest.
- **Fix with:** none
- **Effort:** S

### STUF-60 · P3 · mynpm cannot publish a first-party npm package

- **Area:** npm
- **Where:** `crates/mynpm/`, `stuff publish npm --lockfile PATH --project ID`, `docs/PUBLISHING.md`
- **Problem:** `mynpm` only mirrors upstream packages named in a lockfile, so a package that exists nowhere upstream cannot be published. Niceforge's Core depends on `@stats/producer` 0.1.0 from `~/d/stats/packages/producer`, which is on neither npmjs nor npm.17166969.xyz. With no registry to install it from, Niceforge had to vendor its packed build into `apps/forge/vendor/stats-producer` (niceforge `050b263`) so a clean checkout or CI VM could run `pnpm install`.
- **Recommendation:** Add a native npm publish path, `stuff publish npm --package DIR` or a packed `.tgz`, that hosts a first-party package beside the mirror under the same immutable-generation and conflict rules. Then publish `@stats/producer`, point Niceforge at the version through an `@stats:registry` `.npmrc` line, and delete the vendored copy.
- **Fix with:** do
- **Effort:** M
