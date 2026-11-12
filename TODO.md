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

### STUF-58 · P3 · Publish the LLVM 23.1.5 canonical musl image and its digest

- **Area:** Build
- **Where:** `fleet/policy/musl-toolchain.toml:80`, `docker/musl/Dockerfile:7`
- **Problem:** The existing handoff records a lock/builder pin of `llvm-musl-23.1.5-4c2254e` with an image digest still built from `23.1.0-rc2`, so an image-mode builder would retain the old toolchain. It also records that image mode has no users and the local token lacks `write:packages`. The pins and credentials were not reverified by this review; retain the ticket until the pins and a published digest show completion. RPMD-85 tracks the wider musl rollout.
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

### RPMD-85 · P3 · Reconcile the remaining musl rollout and environment qualification

- **Area:** Operability
- **Where:** `fleet/policy/musl-toolchain.toml:132`, `repomod/cmd/musl.py:91`, `docs/policy-development.md:282`
- **Problem:** The closed musl implementation work left owner-run canaries, shared-image consumer migration, and some environment qualification unresolved. Those external outcomes were not rechecked in this repository review, so completion must not be inferred from the presence of the lock and image recipe. This carries forward existing deferred work rather than asserting a new fleet defect. The draft below preserves the recovery pointers and owner deferrals from the removed campaign record.
- **Recommendation:** Recheck the named external outcomes before deciding what remains. Agree on the conventional-consumer migration scope, then follow the publication and canary gate in the policy guide. Keep owner-deferred Chromium/Node work and scheduling out of the run unless explicitly reopened. Delete this item after the remaining scope is completed or deliberately declined and its durable decisions are recorded.
- **Fix with:** `none`
- **Effort:** M (days)
- **Design:** [Musl rollout scope and qualification](docs/rfc/RPMD-85-musl-rollout.md)

### STUF-74 · P3 · Replace BTreeMap and BTreeSet in the registry crates

- **Area:** Policy
- **Where:** `crates/mypypi` (222), `crates/mynpm` (136), `crates/stuff` (105), `crates/myoci` (49), `crates/stuff-catalog` (46), `crates/stuff-oci` (41), `crates/stuff-worker` (24); counts are `rust-collections` findings at `2cf1489`
- **Problem:** repomod's `rust-collections` rule bans `BTreeMap` and `BTreeSet` (623 sites outside `crates/h12tiny-*`). These crates use ordered collections where iteration order reaches published bytes: catalogs (`catalog.json`, `index.html`, `llms.txt`), sparse index files, generation identities, fingerprints, CLI output, and golden tests. A plain swap to hash collections would change output order. The 3 sites in `crates/h12tiny-tls-system` must change upstream, because those sources stay byte-identical to the imported revision.
- **Recommendation:** Replace them with `rustc_hash::{FxHashMap, FxHashSet}` from `rustc-hash` 2, which is already in `Cargo.lock`, added only to crates that need it. Wherever order is observable, sort explicitly at that point (collect into a `Vec` and sort by the old key order, or serialize a sorted view with `serialize_with`). Use a sorted `Vec` where a BTree served range queries, `first`/`last`, or sorted dedup. Do not edit golden files or expected outputs. Work crate by crate. Done when `uv run repomod lint --repo joshuarli/stuff --checkout "$PWD" --rule rust-collections` reports only the 3 h12tiny sites, and `cargo test --workspace`, `cargo clippy --all-targets -- -D warnings`, and `make test-mycrates` (the Python parity oracle for published bytes) all pass.
- **Fix with:** worker
- **Effort:** L
