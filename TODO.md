Prefix: STUF
Next: 9

### STUF-8 · P3 · mycrates noop check loops over a stub that always returns empty

- **Area:** Dead code
- **Where:** `crates/mycrates/src/publish.rs:69-79`
- **Problem:** `generation_matches` loops over `self.blobs_under(gen_dir)`, but `blobs_under` unconditionally returns `Ok(Vec::new())`, so the loop suggests blob existence is verified on the noop path when nothing is. Harmless today — proven by probe: `publish()` re-syncs every blob from the store (`:94-99`) before staging, so a missing blob errors before the check runs (empty-state republish after blob prune still reports the same generation id with `noop: true`, correctly). The stub will rot or mislead the next reader.
- **Recommendation:** implement the check from the manifest (expected blob digests vs `site/blobs/`) or delete the stub and the loop.
- **Fix with:** dead-code-archaeology
- **Effort:** S
