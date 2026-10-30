Prefix: STUF
Next: 9

### STUF-4 · P2 · error models diverge across the three publish binaries

- **Area:** Errors
- **Where:** `crates/mycrates/src/error.rs:4-11`, `crates/mycrates/src/error.rs:40-62`, `crates/mycrates/src/error.rs:67-76`, `crates/mynpm/src/error.rs:6-13`, `crates/mynpm/src/error.rs:42-50`, `crates/mynpm/src/error.rs:56-65`, `crates/mypypi/src/error.rs:4-52`, `crates/mypypi/src/error.rs:127-129`, `crates/stuff/src/main.rs:20-34`
- **Problem:** the same failure classes exit differently per binary (usage/input: 2/2/1-always-1; integrity: 3 vs Corrupt-1 vs 1; conflict: 4 vs 1 vs 1), `Limit`/`Network` fold lossily into `Operation` and `Integrity` into `Corrupt` at the `stuff-store` boundary, and `stuff doctor` stringifies typed errors. Callers cannot distinguish usage vs conflict vs transient failures across registries, which breaks scripting and retry policy.
- **Recommendation:** adopt one exit-code and conversion contract across the publish binaries (or document the per-binary contract in `README.md`/`AGENTS.md`) and pin it with tests; preserve `Limit`/`Network` through `stuff-store` conversions instead of folding them into `Operation`.
- **Fix with:** error-model
- **Effort:** M

### STUF-5 · P3 · mypypi staging crash window leaks orphan dirs permanently

- **Area:** Data
- **Where:** `crates/mypypi/src/store.rs:446-451`, `crates/mypypi/src/store.rs:107-133`
- **Problem:** staging dirs are created at `:447` and the `.mypypi-owner` marker is written afterwards at `:450`; `recover_staging()` only removes dirs whose marker reads exactly `"mypypi\n"` and skips the rest forever. A crash between the two calls (or a foreign dir with the same prefix) leaks an orphan staging dir on every subsequent publish. Window proven by code walk; the leak not executed.
- **Recommendation:** make recovery robust to the window, e.g. remove prefix-matching dirs with absent/invalid markers once older than a bound, and fsync the marker immediately after writing it.
- **Fix with:** durable-state
- **Effort:** S

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
