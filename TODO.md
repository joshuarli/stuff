Prefix: STUF
Next: 62

### STUF-45 · P3 · mypypi sdist builds run third-party code with the publisher's filesystem

- **Area:** Security
- **Where:** `crates/mypypi/src/build.rs:488-510`, `crates/mypypi/src/process_policy.rs:15-72`, `deploy/publish-mypypi.sh:105`
- **Problem:** `uv build` runs the sdist's build backend with an allowlisted environment, scratch homes, and a process-group timeout, but no filesystem or network isolation: it runs as the publisher and can read `~/.ssh`, the `cf` keyring, and other credential files by absolute path. On macOS the build runs on the host. `--require-build-hashes` and `--build-constraints` exist but `publish-mypypi.sh` passes neither. Inputs are exact pins, so this is supply-chain exposure, not an open hole.
- **Recommendation:** Pass `--require-build-hashes` from the publish helper and decide whether macOS sdist builds move to a credential-free user or account.
- **Blocked:** `--require-build-hashes` without a hash-pinned build-constraints file fails every build (`Hash-checking is required, but no trusted hash was provided for: setuptools`), so turning it on needs a maintained constraints file; macOS isolation needs an owner decision on a credential-free user or account. Unblock: supply the constraints file and the account decision.
- **Fix with:** security
- **Effort:** S

### STUF-57 · P3 · OCI adoption under the local cf simulation stalls for over ten minutes

- **Area:** Deploy
- **Where:** `deploy/publish:207-258` (`adopt_oci`), `deploy/test-oci-publish.sh:107-110`
- **Problem:** In CI run 41935480336 (ubuntu-24.04, job `worker-runtime`), the final step of `deploy/test-oci-publish.sh`, `STUFF_STATE_HOME=$WORK/home-adopt deploy/publish oci --republish`, printed `adopting the live publication` and produced nothing for ten minutes before the job timed out. The earlier publish, republish, GC, tag-move and rollback steps of the same test against the same fixture finished in about two minutes together, so this is a stall, not slowness. The cause has not been located: the suspects are `r2-pull.py` listing or fetching the meta bucket through `cf --local`, the per-blob `cf r2 objects get` loop, or the republish that follows adoption. CI no longer runs the test (stuff `527abaf`); it needs workerd, which does not run on musl hosts.
- **Recommendation:** Run `deploy/test-oci-publish.sh` on macOS with `sh -x` on `deploy/publish`, find the step that stops making progress, and fix it with a failing reproduction first.
- **Fix with:** root-cause
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

### STUF-61 · P2 · Publishing a native crate that depends on another native crate fails verification

- **Area:** Cargo
- **Where:** `crates/stuff/src/publish/cargo/run.rs:45-67` (dry-run staging), `crates/stuff/src/publish/cargo/run.rs:191-240` (`SiteBuild::run` blob materialization), `crates/stuff/src/publish/cargo/server.rs:216`
- **Problem:** `stuff publish cargo -p stats-producer --registry mycrates --dry-run` in `~/d/stats` at `35f5c52` (stats-producer 0.1.1, which depends on the native `h12tiny-client-sync` 0.7.0 from mycrates) fails in `cargo publish` verification: `failed to download from http://127.0.0.1:<port>/blobs/4ef7862202992a23100a6347155bda3cab6a237cd161a4d3998ccdaebbb65207.crate ... got 404`. That blob is live (`https://crates.17166969.xyz/blobs/4ef786….crate` returns 200) and present locally in both `state/blobs/` and `site/blobs/`, and the local promoted generation equals the live one (`2fa92d16d1bad37f9ff5cae9`). Without `--registry mycrates`, the publish server rejects the same dependency with `422 integrity: dependency 'h12tiny-client-sync' uses unsupported registry 'sparse+https://crates.17166969.xyz/current/registry/'`. The non-dry-run path was not tried.
- **Recommendation:** Reproduce with the command above, establish whether the server site is built before cargo's verification downloads dependencies (so native blobs are missing), and fix it with a failing test for a native crate depending on another native crate. Then publish stats-producer 0.1.1 so tansu can drop its `[patch.mycrates]` path to a sibling checkout.
- **Fix with:** root-cause
- **Effort:** S
