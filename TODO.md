Prefix: STUF
Next: 27

### STUF-14 · P2 · The documented registry exit-code contract is not implemented consistently

- **Area:** Errors
- **Where:** `README.md:22-25`; `crates/mynpm/src/acquire.rs:1336-1353` (worker errors flattened to strings and re-wrapped as `Operation`); `crates/mynpm/src/error.rs:14,84` (`Cancelled` never constructed); `crates/mypypi/src/error.rs:9-29,131-140` (no `Limit` or `Network`, `MissingArtifact` maps to 2); `crates/mycrates/src/validate.rs:241,257` (cargo failures map to 1); `crates/stuff/src/doctor.rs:124-133` and `crates/stuff/src/main.rs` (all `stuff` errors exit 2; doctor's documented 2 is unreachable).
- **Problem:** The shared contract is 2 usage/input, 3 integrity, 4 conflict, 5 limit, 6 network, 1 other, plus 130 for a cancelled `mynpm` run. `mynpm fetch` turns integrity and conflict failures into 1, and its cancellation paths never construct `Cancelled`. `mypypi` cannot emit 5 or 6, and network fetch failures exit 2. `mycrates validate` maps a locked-fetch failure to 1. `stuff` maps every failure to 2 while `doctor` can only exit 1. The same corrupt state therefore produces different codes depending on the command, and a retry wrapper cannot distinguish a bad flag from a network outage.
- **Recommendation:** Give each CLI one error category set and one mapping boundary: usage/input 2, integrity/corrupt 3, conflict 4, limit 5, network 6, other 1, and `mynpm` 130 for cancellation. Preserve the original error through worker joins, add `Limit`/`Network` where missing, and construct `Cancelled` on the cancellation paths. For `stuff`, map usage to 2, corrupt or integrity to 3, fetch or network to 6, and other to 1. `doctor` exits 1 when any check fails and 2 for a usage error. Test the mapping per category.
- **Fix with:** `error-model`
- **Effort:** M

### STUF-16 · P2 · Publish concurrency guards can delete another writer's spool and double-admit writers

- **Area:** Concurrency
- **Where:** `crates/mycrates/src/store.rs:101-112,545-560` (`.upload.*` reclamation by PID); `crates/mycrates/src/store.rs:448-449` (`verify` calls recovery without the lock), `crates/mycrates/src/publish.rs:82` (publish calls it before any lock it later takes); `crates/mycrates/src/serve.rs:274` (the HTTP publish path takes the lock after writing the spool); `deploy/publish-lock.sh:9-26` (`mkdir` lock with `rm -rf` reclaim).
- **Problem:** `recover_staging` treats every `.upload.*` file owned by another PID as stale. `Store::verify` calls it without the store lock, and `Publisher::publish` calls it as its first step, so a concurrent verify or publish can unlink an in-flight upload spool and turn `cargo publish` into a 400. Separately, two waiters that see an empty or stale lock directory can both `rm -rf` it before either re-creates it, so both can acquire the "lock" and run publish helpers against one state directory; a stale PID that is later reused blocks every publish forever.
- **Recommendation:** Keep upload spools in a directory the GC never scans, or reclaim only by age under the lock. Replace the directory lock with an advisory file lock or an atomic rename-steal, and test concurrent publish/verify and two lock waiters.
- **Fix with:** `lifecycle-concurrency`
- **Effort:** M

### STUF-20 · P3 · llms.txt ignores the configured registry origin

- **Area:** Config
- **Where:** `crates/stuff-catalog/src/llms.rs:10-14,32,45,57,175-177`; publishers at `crates/mycrates/src/publish.rs:212`, `crates/mynpm/src/publish.rs:806`, `crates/mypypi/src/store.rs:509`; config at `crates/mycrates/src/cli/config.rs:52-59` and `crates/mynpm/src/config.rs:218`; verification at `crates/stuff/src/doctor.rs:281`.
- **Problem:** `render_llms_txt` hardcodes the three `17166969.xyz` origins and cannot render the public base URL each publisher already holds. A deployment at another origin publishes Cargo/npm/pip instructions that point clients at somebody else's registry. `doctor` compares the served document against the same origin-blind renderer, so it cannot detect the mismatch.
- **Recommendation:** Pass the origin (or a registry-to-origin map) into the renderer from each publisher's configured public base URL. Treat the resulting generation-id change as a format change under the AGENTS invariant.
- **Fix with:** `single-source-of-truth`
- **Effort:** M

### STUF-21 · P3 · stuff doctor misresolves advertised local inputs and silently narrows scope

- **Area:** Operability
- **Where:** `crates/stuff/src/doctor.rs:170-175` (generation directory resolution), `:55-57` (skip other registries), `:440,470,497` (sample roots); `crates/stuff/src/cli.rs:225-227` (advertises site, generation, or catalog.json).
- **Problem:** `stuff --help` says `--cargo-dir/--npm-dir/--pypi-dir` accept a site, generation, or `catalog.json`. Passing a generation directory makes `generation_dir_for` return its parent (`<site>/generations`), so `llms.txt`, index, and manifest checks all read the wrong path and report false failures. Passing a catalog file appends `catalog.json` to the file path. Supplying one `--*-dir` also silently drops the other two registries, so `doctor --cargo-dir ... --expected-generation ID` can exit 0 while npm and pypi are never checked.
- **Recommendation:** Resolve the input once to `(site_root, generation_dir)` and use that pair for every check and sample. Either check the unprovided registries remotely or print an explicit skip and refuse `--expected-generation` when any registry is skipped.
- **Fix with:** `none`
- **Effort:** S

### STUF-22 · P3 · The native server and the worker have drifted route and cache semantics

- **Area:** Architecture
- **Where:** `crates/stuff-server/src/serve.rs:29-56,74-87`; `crates/stuff-worker/src/map.rs:121,132-145`; `crates/stuff-worker/src/entry.ts:214-227`; `crates/stuff-http/src/cache.rs:23-32`.
- **Problem:** The worker maps `/index.html`, `/catalog.json`, `/llms.txt`, and `/simple/...` through the current pointer; the native server only aliases `/llms.txt`, so the same root URL is 200 on one and 404 on the other. The server also serves a slash-less directory (`/current/simple/proj`) as `index.html` while the worker returns 404, serves `/generations/<nonexistent>/wheels/<file>` with a one-year immutable 200 while the worker checks the generation catalog and returns 404, and labels the npm digest path `/-/tarballs/<digest>.tgz` as a 60-second pointer while the worker serves it as immutable. `README.md` says the digest object is served with immutable caching.
- **Recommendation:** Put the alias set, trailing-slash rule, generation-existence gate, and route-kind classification in one `stuff-http` function used by both adapters. Classify `-/tarballs/` and `tarballs/` as immutable.
- **Fix with:** `single-source-of-truth`
- **Effort:** M


### STUF-25 · P3 · stuff accepts flags no command reads and advertises positionals it rejects

- **Area:** Types
- **Where:** `crates/stuff/src/cli.rs:102,202-206` (`--remote` parsed and never read), `crates/stuff/src/main.rs:257-286` (`render` ignores `--json`), `crates/stuff/src/cli.rs:219,222-223` (help promises `[sources...]`), `:175-177` (`diff` accepts and ignores the base flags).
- **Problem:** The flat `Parsed` struct is handed to every command and each arm hand-rejects a subset of flags. `doctor --remote` is a no-op that the README and `deploy/deployed-smoke.sh` present as meaningful; `render --output x.html --json` writes HTML and exits 0; `diff` accepts base-URL flags it never uses. The help advertises `search ... [sources...]` and `render ... [sources...]` even though the parser rejects every positional except the single search query. Callers cannot tell from a signature which flags a command honors.
- **Recommendation:** Give each command its own argument type so unsupported flags cannot be parsed, or add the missing rejections per arm. Remove `--remote` or make it select remote mode explicitly, and fix the help text.
- **Fix with:** `api-surface-and-module-boundaries`
- **Effort:** M

