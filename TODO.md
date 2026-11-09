Prefix: STUF
Next: 64

### STUF-45 · P2 · mypypi sdist builds run third-party code with the publisher's filesystem

- **Area:** Security
- **Where:** `crates/mypypi/src/build.rs:488-510`, `crates/mypypi/src/process_policy.rs:15-72`, `crates/stuff/src/publish/pypi.rs`
- **Problem:** `uv build` runs the sdist's build backend with an allowlisted environment, scratch homes, and a process-group timeout, but no filesystem or network isolation: it runs as the publisher and can read `~/.ssh`, the `cf` keyring, and other credential files by absolute path. On macOS the build runs on the host. Prebuilt wheels are now required by default in `stuff publish pypi`, guarded by `--allow-sdist`. `--require-build-hashes` and `--build-constraints` exist but `stuff publish pypi` passes neither. Inputs are exact pins, so this is supply-chain exposure, not an open hole.
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

### STUF-62 · P3 · A one-crate publish still rewrites every index file under the new generation

- **Area:** Performance
- **Where:** `crates/stuff/src/publish/r2/sync.rs:161` (generation-scoped `generations/<id>/` prefix), `crates/stuff/src/publish/cargo/run.rs`
- **Problem:** Every index file is rewritten under the new generation's prefix, so a publish uploads about 1220 objects (4.9 MB) for an 18.6 KiB crate; the upload grows with the registry rather than with the change. Concurrent uploads and live-snapshot fetches (32 workers) cut the upload from about 495 s to 15 s and the dry run from about 360 s to 45 s, so this is now a cost in R2 writes and bytes, not minutes. The HTTP client has no idle connection pool, so each request still pays a TCP and TLS handshake.
- **Recommendation:** Decide whether unchanged index files can be shared across generations (content-addressed, like blobs) so a one-crate publish uploads only the changed index files. Optionally add an idle pool to `h12tiny-client-sync` if request count stays high.
- **Fix with:** architect
- **Effort:** M

### STUF-63 · P3 · Evaluate migrating stuff-worker from wasm-bindgen to WIT/component model

- **Area:** Architecture
- **Where:** `crates/stuff-worker/src/wasm.rs`, `crates/stuff-worker/entry.ts`, `crates/stuff-worker/scripts/build-wasm.sh`, `crates/stuff-worker/scripts/build.mjs`, `crates/stuff-worker/Cargo.toml`, `crates/stuff-worker/cloudflare.config.ts`
- **Problem:** `stuff-worker` uses `#[wasm_bindgen]` (~24 synchronous exports, mostly JSON strings/primitives) with `wasm-bindgen --target web` glue consumed by a custom JS/R2/fetch/cache host (`entry.ts`) and a custom build pipeline (no `workers-rs`). CLI/crate schema skew previously broke `make deploy` (fixed by auto-syncing `wasm-bindgen-cli` to `Cargo.lock` in `build-wasm.sh`), but the boundary contract remains implicit in attributes/glue. A WIT/component-model interface would make the Rust↔host contract explicit, at the cost of migration and an unproven end-to-end deploy/runtime path for this worker’s exact Cloudflare toolchain (Wrangler/`cf`, bundling, R2/cache bindings).
- **Recommendation:** Time-box a spike, not a rewrite: define WIT for 2–3 representative exports, generate bindings, and prove the end-to-end Cloudflare deploy/runtime path in this repo’s toolchain. Decide contract style up front (JSON strings in WIT vs typed records/variants). Stop if deployment/bindings integration is not cleanly supported.
- **Fix with:** architect
- **Effort:** M
