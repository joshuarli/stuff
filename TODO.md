Prefix: STUF
Next: 74

Evidence for the 2026-11-11 ultrareview refers to `5ab165462999f71c11788ea9a2c6b0278e8ae071`. Priorities are reviewer-assigned. Executed probes used local fixtures; source-only conclusions and unavailable runtime checks are identified below. The checked-in Worker configuration leaves network push disabled, so the push findings describe the optional capability, not observed production incidents.

### STUF-45 · P2 · Python builds and wheel imports run with the publisher's filesystem access

- **Area:** Security
- **Where:** `crates/mypypi/src/validate.rs:666-719`, `crates/mypypi/src/process_policy.rs:62-71`, `crates/mypypi/src/publish.rs:391-416`, `crates/stuff/src/publish/pypi.rs:139-148`, `crates/stuff/src/publish/pypi.rs:240-246`
- **Problem:** Sdist backends and default native-target wheel validation execute as the publisher. Validation installs a prebuilt wheel and runs `python -I -c '__import__(...)'` even without `--allow-sdist`. Scratch homes and an allowlisted environment do not isolate filesystem or network access. An offline probe installed a harmless wheel with real `uv` and the source-derived environment policy; its isolated-mode import read a test sentinel outside its scratch home. That proof used CPython 3.12.14, not the unavailable Rust dispatcher/CPython 3.15 gate. Exact pins and operator-selected inputs limit exposure; they do not contain package code. Build hashes remain a separate control, currently unwired in `stuff publish pypi`, requiring maintained hash-pinned constraints.
- **Recommendation:** Apply one credential-free execution boundary to build backends and import validation, retaining native validation and artifact hash checks. Keep credentials with the publication process. Choose the host isolation mechanism in the linked RFC and prove that both an sdist and a prebuilt wheel cannot read a publisher-owned sentinel. Wire build constraints/hashes separately once their trusted input is available.
- **Fix with:** security
- **Effort:** M
- **Design:** [STUF-45: Isolate Python package execution](docs/rfc/STUF-45-python-execution.md)

### STUF-70 · P2 · Cached wheels block the next unrelated Python publication

- **Area:** Data
- **Where:** `crates/stuff/src/publish/pypi.rs:104-146`, `crates/stuff/src/publish/pypi.rs:161-204`, `crates/mypypi/src/cli.rs:336-357`, `crates/mypypi/src/cli.rs:404-414`, `crates/mypypi/src/publish.rs:255-266`
- **Problem:** Each invocation resolves a fresh catalog, but planning and validation scan the permanent `state/pypi/wheels` directory. Both reject cached package/version entries absent from that invocation's catalog. After A is published, unrelated B therefore fails on A's leftover wheel. `pull_live` only reads or links cache entries; it neither scopes the candidates nor unions their catalog. A cold dry-run can populate the same permanent directory and obstruct a later publish. Two independent source reviews confirmed the complete path; an end-to-end Rust reproduction remains unproven because the toolchain was unavailable.
- **Recommendation:** Separate reusable downloaded bytes from the selected candidate set. Plan and validate only wheels selected by the resolved invocation catalog, while preserving remote published artifacts through the existing merge. In the fake-S3 harness, publish A then B with the same state/cache homes, require both in the active generation, retain A's cache, and require repeating B to be a no-op. Cover a preceding cold dry-run as well.
- **Fix with:** isolate-effects
- **Effort:** M

### STUF-71 · P2 · Subprocess cleanup does not own descendants and pipe lifetime

- **Area:** Concurrency
- **Where:** `crates/mypypi/src/process_policy.rs:296-307`, `crates/mypypi/src/process_policy.rs:350-378`, `crates/mynpm/src/process.rs:57-65`, `crates/mynpm/src/process.rs:108-126`, `crates/mynpm/src/process.rs:212-222`
- **Problem:** The Python runner joins output readers without a deadline after the direct child exits. A descendant holding either pipe prevents EOF and bypasses the timeout loop. An OS probe confirmed that `sh -c 'sleep 30 & exit 0'` leaves both pipes open after its parent exits. The npm runner bounds reader waiting but invokes `/bin/kill -TERM -<pgid>` without `--`; that exact argv failed on this Linux host and left the test group alive. Its fallback kills only the direct child. These probes exercise the real OS boundaries; the Rust wrappers were not compiled.
- **Recommendation:** Own the child, group, and readers through bounded completion on every exit path. Bound pipe draining after direct-child exit, terminate remaining owned descendants, and use/check valid negative-PGID signaling. Preserve partial diagnostics. Verify an early-exiting parent with inherited pipes and a descendant ignoring TERM; require bounded return and no surviving owned PIDs after success, timeout, or output overflow.
- **Fix with:** lifecycle-concurrency
- **Effort:** M

### STUF-58 · P3 · Publish the LLVM 23.1.5 canonical musl image and its digest

- **Area:** Build
- **Where:** External `repomod/musl-toolchain.toml:210`, `repomod/docker/musl/Dockerfile:7`
- **Problem:** The existing handoff records a lock/builder pin of `llvm-musl-23.1.5-4c2254e` with an image digest still built from `23.1.0-rc2`, so an image-mode builder would retain the old toolchain. It also records that image mode has no users and the local token lacks `write:packages`. These files and credentials are outside this repository and were not reverified by this review; retain the ticket until its owning repository supplies evidence of completion.
- **Recommendation:** Reverify the pins and consumers in `repomod`, build/test the canonical image for `linux/amd64` and `linux/arm64`, publish its multi-arch manifest, and update the locked digest there.
- **Fix with:** none
- **Effort:** S

### STUF-62 · P3 · A one-crate publish rewrites unchanged index files under a new generation

- **Area:** Performance
- **Where:** `crates/stuff/src/publish/r2/sync.rs:161-165`, `crates/stuff/src/publish/r2/sync.rs:200-205`, `crates/stuff/src/publish/cargo/run.rs`, `crates/h12tiny-client-sync/src/lib.rs:130`, `crates/h12tiny-client-sync/src/lib.rs:1017-1022`
- **Problem:** Every index file gets a new generation-prefixed key, and upload planning compares complete keys, so unchanged index bytes require new R2 objects. Work scales with registry size rather than only the changed crate. The HTTP client opens a new connection per dispatch and requests `Connection: close`. Earlier timing/object-count measurements were not rerun and are omitted from this current source-backed assessment. Imported h12tiny sources must remain byte-identical to upstream.
- **Recommendation:** Evaluate sharing immutable index bytes while preserving generation-local lookup, rollback, and legacy readers. Measure object counts and end-to-end publication cost before adopting a new storage format. Any client pooling change belongs upstream. The RFC defines the decision and required evidence.
- **Fix with:** architect
- **Effort:** M
- **Design:** [STUF-62: Share unchanged index bytes](docs/rfc/STUF-62-shared-index-bytes.md)

### STUF-63 · P3 · Evaluate an explicit Worker WASM interface

- **Area:** Architecture
- **Where:** `crates/stuff-worker/src/wasm.rs`, `crates/stuff-worker/entry.ts`, `crates/stuff-worker/scripts/build-wasm.sh`, `crates/stuff-worker/scripts/build.mjs`, `crates/stuff-worker/Cargo.toml`
- **Problem:** The Worker exposes synchronous wasm-bindgen exports carrying JSON strings and primitives to a custom R2/fetch/cache host. The build script now synchronizes `wasm-bindgen-cli` with `Cargo.lock`, resolving the prior executable/schema version skew. Interface shape still lives in Rust exports and TypeScript callers. A WIT/component-model migration has no demonstrated build/deploy/runtime path in this repository and is not a prerequisite for correcting its current host-boundary bugs.
- **Recommendation:** Time-box a spike over representative routing, response-plan, and catalog exports. Compare an explicit contract on the existing ABI with a component-model prototype; require the existing Cloudflare build and runtime path to work before choosing migration. Keep the current implementation unless the spike demonstrates a concrete improvement.
- **Fix with:** architect
- **Effort:** M
- **Design:** [STUF-63: Evaluate the Worker interface](docs/rfc/STUF-63-worker-interface.md)
