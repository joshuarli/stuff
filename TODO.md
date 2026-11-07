Prefix: STUF
Next: 63

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

### STUF-62 · P2 · Publishing one crate re-uploads the whole generation one object at a time

- **Area:** Performance
- **Where:** `crates/stuff/src/publish/r2/sync.rs:355` (`execute`'s serial `for entry in upload_order(...)`), `crates/stuff/src/publish/r2/sync.rs:161` (generation-scoped `generations/<id>/` prefix), `crates/stuff/src/publish/cargo/run.rs`
- **Problem:** Publishing stats-producer 0.1.1 (one 18.6 KiB crate) to mycrates took over seven minutes. The dry run reported `uploaded 1223 objects (5475578 bytes), skipped 1932 objects`: every index file is rewritten under the new generation's prefix, so the upload grows with the registry rather than with the change. A `sample` of the live publish showed the main thread in `r2::sync::execute` → `R2Client::put_object`, blocked in `h12tiny_client_sync::read_response`, with about 0.4% CPU and one TCP connection open: each object is a full HTTPS round trip to R2, made one after another. The dry run also spent minutes fetching the live snapshot before building.
- **Recommendation:** Measure the per-object round trip and the snapshot fetch first. Then upload with bounded concurrency over kept-alive connections, keeping manifest-last ordering and the conditional `current` promotion. Separately decide whether unchanged index files can be shared across generations (content-addressed, like blobs) so a one-crate publish uploads only the changed index files.
- **Fix with:** make-it-fast
- **Effort:** M
