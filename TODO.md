Prefix: STUF
Next: 50

### STUF-40 · P3 · Each publish re-hashes the whole artifact store

- **Area:** Performance
- **Where:** `crates/mycrates/src/publish.rs:93-98`, `crates/stuff-store/src/blob.rs:12-37`, `crates/mynpm/src/publish.rs:340-349`, `crates/mynpm/src/publish.rs:525`, `crates/mynpm/src/publish.rs:736`, `crates/mynpm/src/acquire.rs:927-935`
- **Problem:** mycrates calls `sync_blob` for every record before the no-op check, and `sync_immutable_file` hashes both the state and site copy (the same inode when hard-linked), under the exclusive store lock that `serve-publish` requests wait on. mynpm hashes every tarball at least twice per publish and loads each whole file into memory to do it. The cost grows with total corpus bytes: one SHA-256 pass over the live 181 MB, 1095-crate mycrates store takes about 0.5 s, so this is a scaling tax, not a current outage.
- **Recommendation:** Skip re-hashing when the destination is the same inode or already present with the expected size, stream mynpm hashes, and leave full verification to `verify` and `adopt`.
- **Fix with:** make-it-fast
- **Effort:** M

### STUF-41 · P3 · stuff-server serves dot-prefixed staging and lock files with immutable caching

- **Area:** Security
- **Where:** `crates/stuff-http/src/route.rs:97`, `crates/stuff-http/src/route.rs:165`, `crates/stuff-server/src/serve.rs:36-43`
- **Problem:** Any path outside `current/`, `simple/`, and `generations/` becomes `RouteScope::Site`, and `resolve_path` rejects only `.` and `..`. On the native server `/.staging.<pid>.<ns>.<n>/…` serves an unverified in-progress tree, and `/.publish.lock` and temp files are reachable. Every Site response is `public, max-age=31536000, immutable`, so a CDN can pin a transient file for a year. Traced by reading `plan_route`, `map_request`, and `respond`; not exercised.
- **Recommendation:** Allow only the shared-store roots (`blobs/`, `tarballs/`, `wheels/`, `manifests/`) in Site scope and return 404 for other paths, including any dot-prefixed segment.
- **Fix with:** security
- **Effort:** S

### STUF-42 · P3 · stuff-server slots can be pinned by slow readers, and an accept error exits the server

- **Area:** Concurrency
- **Where:** `crates/stuff-server/src/main.rs:108-127`, `crates/stuff-server/src/main.rs:182-184`, `crates/stuff-server/src/main.rs:312-333`
- **Problem:** The 30 s deadline covers only the header read; the body send has a per-write timeout only, so a client reading slowly holds one of the `--max-connections` slots indefinitely. Any non-`WouldBlock` `accept` error, including transient ones and `EMFILE`, returns from the loop and exits the process. A header parse failure closes the socket without a response, and `static STOP` is never set. `--max-connections` and its 503 path have no test.
- **Recommendation:** Add a total response deadline or minimum transfer rate, log and continue on transient accept errors, answer parse failures with 400, delete `STOP`, and add one overload test.
- **Fix with:** lifecycle-concurrency
- **Effort:** M

### STUF-43 · P3 · Worker range requests read R2 twice and query strings bypass the cache

- **Area:** Performance
- **Where:** `crates/stuff-worker/src/entry.ts:116`, `crates/stuff-worker/src/entry.ts:229-287`, `crates/stuff-worker/src/entry.ts:317`, `crates/stuff-worker/src/entry.ts:333`
- **Problem:** Every lookup is a full `store.get(key)`; a ranged response then issues a second `store.get(key, {range})` and drops the first body, and pip fetches wheel metadata with Range. A pointer route that misses costs up to three R2 reads. 404s carry no `cache-control`, and both cache keys include the query string, so `?x=<random>` reaches R2 on every request. Read from the code; edge behavior for 404 and range caching is unmeasured.
- **Recommendation:** Pass the range on the first `get` (or `head` first), drop the query string from cache keys, and give 404s a short `cache-control`.
- **Fix with:** make-it-fast
- **Effort:** S

### STUF-44 · P3 · mynpm leaves npm and pnpm children running and workspaces behind

- **Area:** Concurrency
- **Where:** `crates/mynpm/src/process.rs:130-136`, `crates/mynpm/src/process.rs:178-199`, `crates/mynpm/src/error.rs:84`, `crates/mynpm/src/validate.rs:1994`, `crates/mynpm/src/validate.rs:2037-2040`, `crates/mynpm/src/validate.rs:2176`
- **Problem:** Children run in their own process group and mynpm installs no signal handling, so Ctrl-C stops mynpm but leaves `npm ci` or `pnpm install` running and the `mynpm-validate-*` workspace on disk; nothing sweeps old workspaces. `Error::Cancelled` (exit 130) has one producer, the fetch deadline, so a timeout reports the interrupt code. On output overflow, `run_process` joins the reader threads before killing the child, so the join blocks until the child exits on its own and the timeout no longer bounds it. The validate `TMPDIR` is derived from the process's `current_dir()/.work/tmp` while the workspace uses `options.cwd`.
- **Recommendation:** Kill the group before joining readers, rename the deadline error to a limit error with its own code, sweep stale validate workspaces at start, and derive the child `TMPDIR` from the workspace root.
- **Fix with:** lifecycle-concurrency
- **Effort:** M

### STUF-45 · P3 · mypypi sdist builds run third-party code with the publisher's filesystem

- **Area:** Security
- **Where:** `crates/mypypi/src/build.rs:488-510`, `crates/mypypi/src/process_policy.rs:15-72`, `deploy/publish-mypypi.sh:105`
- **Problem:** `uv build` runs the sdist's build backend with an allowlisted environment, scratch homes, and a process-group timeout, but no filesystem or network isolation: it runs as the publisher and can read `~/.ssh`, the `cf` keyring, and other credential files by absolute path. On macOS the build runs on the host. `--require-build-hashes` and `--build-constraints` exist but `publish-mypypi.sh` passes neither. Inputs are exact pins, so this is supply-chain exposure, not an open hole.
- **Recommendation:** Pass `--require-build-hashes` from the publish helper and decide whether macOS sdist builds move to a credential-free user or account.
- **Fix with:** security
- **Effort:** S

### STUF-46 · P4 · mycrates native records store four index facts twice

- **Area:** Types
- **Where:** `crates/mycrates/src/store.rs:28-35`, `crates/mycrates/src/store.rs:348-349`, `crates/mycrates/src/publish.rs:216`, `crates/mycrates/src/publish.rs:321`, `crates/mycrates/src/adopt.rs:45-51`
- **Problem:** `NativeRecord` carries `name`, `vers`, `checksum`, and `yanked` beside `entry: IndexEntry`, which holds the same facts. `set_native_yank` updates both by hand, the index renders from `entry` while the catalog reads `rec.yanked`, and `load_native_records` never checks they agree. No drift path is reachable today.
- **Recommendation:** Check agreement on load and return `Integrity` on mismatch, then derive the duplicates from `entry` while keeping stored records readable.
- **Fix with:** make-invalid-states-unrepresentable
- **Effort:** M

### STUF-47 · P4 · Tool and origin pins live in several places

- **Area:** Config
- **Where:** `crates/mynpm/src/resolve.rs:13-14`, `crates/mynpm/src/resolve.rs:203`, `crates/mynpm/src/resolve.rs:316`, `crates/mynpm/src/resolve.rs:335`, `crates/mynpm/src/validate.rs:469`, `crates/mynpm/src/validate.rs:687`, `deploy/mypypi-linux.Dockerfile:4`, `crates/mycrates/rust-toolchain.toml`, `crates/stuff/src/source.rs:10-12`, `crates/stuff-catalog/src/llms.rs:5`
- **Problem:** Five call sites repeat the npm-or-pnpm version choice. uv is 0.13.2 in the Linux build container and 0.13.5 elsewhere, and no code checks it, so Linux wheels build under a different resolver than macOS. `crates/mycrates/rust-toolchain.toml` duplicates the workspace pin. The CLI and `stuff-catalog` each define the three default origins, though the CLI depends on `stuff-catalog`.
- **Recommendation:** Add `Manager::pinned_version()`, bump the Dockerfile uv to match, delete the mycrates toolchain copy, and have the CLI use the catalog constants.
- **Fix with:** single-source-of-truth
- **Effort:** S

### STUF-48 · P4 · Dead code and no-op statements across the registries

- **Area:** Dead code
- **Where:** `crates/stuff-worker/src/map.rs:14-25`, `crates/stuff-worker/src/lib.rs:9-11`, `crates/mycrates/src/model.rs:186`, `crates/mycrates/src/store.rs:99`, `crates/mycrates/src/store.rs:121-123`, `crates/mypypi/src/build.rs:117-119`, `crates/mypypi/src/store.rs:784`, `crates/mypypi/src/store.rs:849-862`, `crates/mypypi/src/process_policy.rs:160-162`, `crates/mynpm/src/validate.rs:1925`, `crates/mynpm/src/validate.rs:1971`, `crates/mynpm/src/validate.rs:2064`, `crates/mycrates/.github/workflows/ci.yml`
- **Problem:** `map::RouteKind` is never read and shadows `stuff_http::RouteKind` through a public re-export. `UpstreamIndexLine` has no references, `try_lock_exclusive` is test-only, and the `.upload.` cleanup in `blobs/` serves a layout only a `legacy` test creates. mypypi's `void_is_zip_check`, `let _ = rows`, an identity `Kind` match, and a one-line `redact_credentials` wrapper do nothing, nor do mynpm's `let _ = &mut args`-style lines. The imported mycrates workflow never runs (GitHub reads only the root `.github/`) and names toolchain 1.99.1.
- **Recommendation:** Delete them.
- **Fix with:** delete
- **Effort:** S

### STUF-49 · P4 · stuff doctor panics on a non-ASCII crate name

- **Area:** Errors
- **Where:** `crates/stuff-cargo-shard/src/lib.rs:12-17`, `crates/stuff/src/doctor.rs:107`
- **Problem:** `shard_for_name` slices by byte index, so a multibyte character at a slice boundary panics. `stuff doctor` feeds it names from a remote or local catalog, so a corrupt catalog crashes the health check instead of failing one check.
- **Recommendation:** Return `Option` or reject non-ASCII names before sharding.
- **Fix with:** error-model
- **Effort:** S
