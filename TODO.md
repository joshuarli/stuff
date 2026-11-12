Prefix: STUF
Next: 75

New tickets use STUF. RPMD tickets were filed in repomod before it moved into stuff; they keep their IDs.

Evidence for the 2026-11-11 ultrareview refers to `5ab165462999f71c11788ea9a2c6b0278e8ae071`. Priorities are reviewer-assigned. Executed probes used local fixtures; source-only conclusions and unavailable runtime checks are identified below. The checked-in Worker configuration leaves network push disabled, so the push findings describe the optional capability, not observed production incidents.

### STUF-45 · P2 · Python builds and wheel imports run with the publisher's filesystem access

- **Area:** Security
- **Where:** `crates/mypypi/src/validate.rs:666-719`, `crates/mypypi/src/process_policy.rs:62-71`, `crates/mypypi/src/publish.rs:391-416`, `crates/stuff/src/publish/pypi.rs:139-148`, `crates/stuff/src/publish/pypi.rs:240-246`
- **Problem:** Sdist backends and default native-target wheel validation execute as the publisher. Validation installs a prebuilt wheel and runs `python -I -c '__import__(...)'` even without `--allow-sdist`. Scratch homes and an allowlisted environment do not isolate filesystem or network access. An offline probe installed a harmless wheel with real `uv` and the source-derived environment policy; its isolated-mode import read a test sentinel outside its scratch home. That proof used CPython 3.12.14, not the unavailable Rust dispatcher/CPython 3.15 gate. Exact pins and operator-selected inputs limit exposure; they do not contain package code. Build hashes remain a separate control, currently unwired in `stuff publish pypi`, requiring maintained hash-pinned constraints.
- **Recommendation:** Apply one credential-free execution boundary to build backends and import validation, retaining native validation and artifact hash checks. Keep credentials with the publication process. Choose the host isolation mechanism in the linked RFC and prove that both an sdist and a prebuilt wheel cannot read a publisher-owned sentinel. Wire build constraints/hashes separately once their trusted input is available.
- **Fix with:** security
- **Effort:** M
- **Design:** [STUF-45: Isolate Python package execution](docs/rfc/STUF-45-python-execution.md)

### STUF-62 · P3 · A one-crate publish rewrites unchanged index files under a new generation

- **Area:** Performance
- **Where:** `crates/stuff/src/publish/r2/sync.rs:161-165`, `crates/stuff/src/publish/r2/sync.rs:200-205`, `crates/stuff/src/publish/cargo/run.rs`, `crates/h12tiny-client-sync/src/lib.rs:130`, `crates/h12tiny-client-sync/src/lib.rs:1017-1022`
- **Problem:** Every index file gets a new generation-prefixed key, and upload planning compares complete keys, so unchanged index bytes require new R2 objects. Work scales with registry size rather than only the changed crate. The HTTP client opens a new connection per dispatch and requests `Connection: close`. A 2026-11-11 pass reconfirmed that mechanism from `plan` in `sync.rs` and did not migrate storage: there is no representative retained registry here, and the draft still requires object counts, R2 operations, and cold/warm publication time before a format change. Imported h12tiny sources must remain byte-identical to upstream.
- **Recommendation:** Evaluate sharing immutable index bytes while preserving generation-local lookup, rollback, and legacy readers. Measure object counts and end-to-end publication cost before adopting a new storage format. Any client pooling change belongs upstream. The RFC defines the decision and required evidence.
- **Fix with:** architect
- **Effort:** M
- **Design:** [STUF-62: Share unchanged index bytes](docs/rfc/STUF-62-shared-index-bytes.md)

### RPMD-85 · P3 · Reconcile the remaining musl rollout and environment qualification

- **Area:** Operability
- **Where:** `fleet/policy/musl-toolchain.toml:132`, `repomod/cmd/musl.py:91`, `docs/policy-development.md:282`
- **Problem:** The closed musl implementation work left owner-run canaries, shared-image consumer migration, and some environment qualification unresolved. The published index is `ghcr.io/joshuarli/musl-toolchain@sha256:a9802a6f0658ad344fd6b2577891194019ab86de1a4d494e32c9bbec820e863e`. A 2026-11-11 recheck, recorded in the draft, found no image-mode consumers, `tm` main not compiling, the recorded `python-build` musl builds and smokes green with only a duplicate release tag failing publish, and tansu pull request 11 merged. Chromium and Node proofs stay deferred. Completion must not be inferred from the lock and image recipe.
- **Recommendation:** Recheck the named external outcomes before deciding what remains. Agree on the conventional-consumer migration scope, then follow the publication and canary gate in the policy guide. Keep owner-deferred Chromium/Node work and scheduling out of the run unless explicitly reopened. Delete this item after the remaining scope is completed or deliberately declined and its durable decisions are recorded.
- **Fix with:** `none`
- **Effort:** M (days)
- **Design:** [Musl rollout scope and qualification](docs/rfc/RPMD-85-musl-rollout.md)
