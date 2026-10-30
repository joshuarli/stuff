Prefix: STUF
Next: 9

### STUF-6 · P3 · Cargo index sharding is implemented twice

- **Area:** Config
- **Where:** `crates/mycrates/src/index.rs:5-20`, `crates/stuff/src/doctor.rs:99-107`
- **Problem:** `shard_for_name` and `cargo_shard` encode the same rule (including the `3/x/xxx` case) in two crates. They agree today for ASCII crate names, but a change in one silently breaks the other's index cross-checks. Duplication proven by reading; divergence unproven.
- **Recommendation:** delete `cargo_shard` and reuse `mycrates::index::shard_for_name` from `doctor`.
- **Fix with:** single-source-of-truth
- **Effort:** S

### STUF-7 · P3 · catalog types admit states the domain forbids

- **Area:** Types
- **Where:** `crates/stuff-catalog/src/model.rs:57-77`, `crates/stuff-catalog/src/model.rs:202-208`, `crates/stuff-catalog/src/model.rs:442-444`
- **Problem:** `Package`/`VersionEntry` fields are all `pub`, so direct struct literals can build empty names, empty/duplicate versions, `latest`-not-in-`versions`, or a non-cargo `source` — all rejected by `parse_catalog_json` but not by construction. The `cargo_`/`npm_`/`pypi_` constructors validate, and no current caller bypasses them, so this is future risk, not a live defect. Separately, `pypi_catalog` collapses per-file yank into per-version yank with `all()`, silently dropping partial-yank information.
- **Recommendation:** privatize the fields behind the existing validated constructors (or document the invariant where construction happens), and record the per-file vs per-version yank semantics explicitly.
- **Fix with:** make-invalid-states-unrepresentable
- **Effort:** S

### STUF-8 · P3 · mycrates noop check loops over a stub that always returns empty

- **Area:** Dead code
- **Where:** `crates/mycrates/src/publish.rs:69-79`
- **Problem:** `generation_matches` loops over `self.blobs_under(gen_dir)`, but `blobs_under` unconditionally returns `Ok(Vec::new())`, so the loop suggests blob existence is verified on the noop path when nothing is. Harmless today — proven by probe: `publish()` re-syncs every blob from the store (`:94-99`) before staging, so a missing blob errors before the check runs (empty-state republish after blob prune still reports the same generation id with `noop: true`, correctly). The stub will rot or mislead the next reader.
- **Recommendation:** implement the check from the manifest (expected blob digests vs `site/blobs/`) or delete the stub and the loop.
- **Fix with:** dead-code-archaeology
- **Effort:** S
