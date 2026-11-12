Prefix: STUF
Next: 75

New tickets use STUF. RPMD tickets were filed in repomod before it moved into stuff; they keep their IDs.

Evidence for the 2026-11-11 ultrareview refers to `5ab165462999f71c11788ea9a2c6b0278e8ae071`. Priorities are reviewer-assigned. Executed probes used local fixtures; source-only conclusions and unavailable runtime checks are identified below. The checked-in Worker configuration leaves network push disabled, so the push findings describe the optional capability, not observed production incidents.

### STUF-45 · P3 · Python builds still reach the network and build hashes are unwired

- **Area:** Security
- **Where:** `crates/mypypi/src/build.rs` (`build_source_file`), `crates/stuff/src/publish/pypi.rs:299-300`
- **Problem:** Sdist backends and native wheel imports now run inside the sandbox described in the mypypi README. The remaining exposure is that the build keeps network access to resolve `build-system.requires`, so a hostile backend can send out what it can read (its scratch and the downloaded source), and that `stuff publish pypi` passes no build constraints or `--require-build-hashes`, so build dependency selection is not pinned. macOS uses `sandbox-exec` because a separate unprivileged account could not be provisioned unattended; a VM remains the stronger alternative if that contract is ever required.
- **Recommendation:** Maintain a checked-in hash-pinned build-constraints file with a refresh procedure, wire it through `BuildOptions` before enabling `--require-build-hashes`, and then resolve build requirements in a separate network stage so the backend itself runs offline.
- **Fix with:** security
- **Effort:** M

### STUF-62 · P3 · A one-crate publish rewrites unchanged index files under a new generation

- **Area:** Performance
- **Where:** `crates/stuff/src/publish/r2/sync.rs:161-165`, `crates/stuff/src/publish/r2/sync.rs:200-205`, `crates/stuff/src/publish/cargo/run.rs`, `crates/h12tiny-client-sync/src/lib.rs:130`, `crates/h12tiny-client-sync/src/lib.rs:1017-1022`
- **Problem:** Every index file gets a new generation-prefixed key, and upload planning compares complete keys, so unchanged index bytes require new R2 objects. Work scales with registry size rather than only the changed crate. The HTTP client opens a new connection per dispatch and requests `Connection: close`. A 2026-11-11 pass reconfirmed that mechanism from `plan` in `sync.rs` and did not migrate storage: there is no representative retained registry here, and the draft still requires object counts, R2 operations, and cold/warm publication time before a format change. Imported h12tiny sources must remain byte-identical to upstream.
- **Recommendation:** Evaluate sharing immutable index bytes while preserving generation-local lookup, rollback, and legacy readers. Measure object counts and end-to-end publication cost before adopting a new storage format. Any client pooling change belongs upstream. The RFC defines the decision and required evidence.
- **Fix with:** architect
- **Effort:** M
- **Design:** [STUF-62: Share unchanged index bytes](docs/rfc/STUF-62-shared-index-bytes.md)
