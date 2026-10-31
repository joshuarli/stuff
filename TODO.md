Prefix: STUF
Next: 27

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

