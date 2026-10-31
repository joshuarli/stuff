Prefix: STUF
Next: 29

### STUF-28 · P3 · worker unified llms.txt ignores configured origins

- **Area:** Operability
- **Where:** `crates/stuff-worker/src/api.rs:347-352` (renders with `CatalogOrigins::default()`), `crates/stuff-worker/src/wasm.rs:41-42`, `crates/stuff-worker/src/entry.ts:153`.
- **Problem:** The CLI renders `llms.txt` with the configured base URLs, but the worker's unified `/llms.txt` hardcodes `CatalogOrigins::default()`, so `STUFF_ROUTES` hosts and a custom `STUFF_UNIFIED_HOST` still produce links to `crates.17166969.xyz`, `npm.17166969.xyz`, `pypi.17166969.xyz`, and `stuff.17166969.xyz`. A deployment serving mirrors on other hosts advertises unreachable URLs. `CatalogOrigins::new` has no parameter for the unified origin either.
- **Recommendation:** Take origins in the unified render entry point, derive them from `STUFF_ROUTES` and `STUFF_UNIFIED_HOST` in `entry.ts`, let `CatalogOrigins` carry the unified origin, and add a worker test with non-default hosts.
- **Fix with:** `single-source-of-truth`
- **Effort:** S
