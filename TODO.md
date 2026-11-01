Prefix: STUF
Next: 50

### STUF-32 · P2 · State recovery from R2 fails on any multipart object

- **Area:** Data
- **Where:** `deploy/r2-pull.py:144-148`, `deploy/r2-sync.sh:229-243`, `deploy/publish:121`, `deploy/publish:149`, `deploy/publish:162`
- **Problem:** `download()` fails when a listing ETag contains `-`. `r2-sync.sh` uploads every object over 300 MB as multipart, so the first such wheel or tarball makes every adoption fail, and `deploy/publish` refuses to publish without adopted state. Recovery breaks exactly when the state is lost. No local object exceeds 300 MB today (`fd -S +300m` over the state home found none), so this is not yet hit. Adoption also never checks the pulled tree against `manifests/<gen>.json`, so for mypypi a missing object is adopted silently.
- **Recommendation:** Verify multipart objects with `r2_common.multipart_etag(path, PART_SIZE)` against the listing ETag, and after the pull check every entry of the generation's manifest (key, size, ETag).
- **Fix with:** error-model
- **Effort:** S

### STUF-34 · P2 · mynpm fetch discards every staged download on one failure or a late deadline

- **Area:** Operability
- **Where:** `crates/mynpm/src/acquire.rs:1307-1369`, `crates/mynpm/src/limits.rs:9-12`
- **Problem:** Workers stage tarballs and keep results in memory; `promote_artifact` and `update_inventory` run only after every worker finishes. The first worker error, or `Instant::now() >= deadline` after all tasks succeeded, removes the staging directory and returns, so nothing downloaded in that run is kept. A fleet fetch (six targets, up to the 10 GiB budget) that hits one bad package or finishes after the one-hour limit re-downloads everything, and a fleet that needs more than an hour can never complete. mycrates' fetch keeps finished crates across a failed run; mynpm does not.
- **Recommendation:** Promote each artifact as soon as it verifies, record inventory updates on every exit path, and check the deadline only before starting a task.
- **Fix with:** durable-state
- **Effort:** M

### STUF-35 · P3 · Generation activation, locking, and staging recovery are reimplemented per registry

- **Area:** Architecture
- **Where:** `crates/stuff-store/src/site.rs:109-130`, `crates/stuff-store/src/site.rs:158-203`, `crates/mynpm/src/publish.rs:746`, `crates/mynpm/src/publish.rs:860-970`, `crates/mypypi/src/store.rs:511-580`, `crates/mycrates/src/publish.rs:161`
- **Problem:** `Site::activate` has one production caller, mycrates. mynpm and mypypi each implement their own generation rename, `current` swap, and temp names, and each registry has its own lock type, and they disagree on verify-before-switch and on refusing to replace an immutable generation. `Site::recover_staging` removes only `.staging*` and `.current.tmp*`, which mynpm (`.mynpm-*.tmp`, `.mynpm-generation-*`, `.current-*`) and mypypi (`.mypypi-current-*`) never use, so their crash leftovers accumulate. `activate` itself leaves a generation that fails verification in place, after which the same content-addressed id is refused until removed by hand, and it joins an unvalidated `generation_id` into a path. The `current` pointer and generation-id grammar are parsed five more times with different strictness (`crates/stuff-server/src/serve.rs:47`, `crates/stuff/src/source.rs:182`, `crates/stuff-worker/src/entry.ts:104`, `crates/stuff-worker/src/map.rs:69`, `crates/stuff-worker/src/api.rs:222-227`).
- **Recommendation:** Give `stuff-store` the extend-current publish shape mynpm and mypypi need, move both onto it with one OS-released lock (mypypi's `file_lock` is the model) and one temp-name prefix, quarantine a generation that fails verification, and export one generation-id parser for the server, CLI, and worker.
- **Fix with:** architect
- **Effort:** L

### STUF-36 · P3 · mynpm and mypypi publish helpers keep running unlocked after INT or TERM

- **Area:** Concurrency
- **Where:** `deploy/publish-mynpm.sh:42`, `deploy/publish-mypypi.sh:43`, `deploy/publish-mycrates.sh:88-91`
- **Problem:** `trap 'release_publish_lock' EXIT INT TERM HUP` releases the lock and returns, so after a signal aimed at the script (a CI cancel, `timeout`) the remaining sync, promote, and prune run without the lock and the script exits 0. Reproduced with a script sourcing `publish-lock.sh` and the same trap: after `kill -TERM` it printed the lock as free, ran the next step, and exited 0. The mycrates helper and `deploy/publish` already exit from their signal traps.
- **Recommendation:** Release on EXIT only and add `trap 'exit 130' INT`, `trap 'exit 143' TERM`, `trap 'exit 129' HUP`, as `publish-mycrates.sh` does.
- **Fix with:** lifecycle-concurrency
- **Effort:** S

### STUF-37 · P3 · mycrates serve-publish drops connections silently and reports server faults as 400

- **Area:** Errors
- **Where:** `crates/mycrates/src/serve.rs:275-278`, `crates/mycrates/src/serve.rs:310-327`, `crates/mycrates/src/serve.rs:349-353`, `crates/mycrates/src/serve.rs:389-391`, `crates/mycrates/src/serve.rs:507`
- **Problem:** `?` on `lock_exclusive`, `ingest_staged_file`, and `parse_packaged_manifest` returns from `handle_connection`, and `run_server` discards the error with `let _ =`, so Cargo gets an empty reply (a manifest with a `path` dependency that passes the archive scan is one trigger). Every other branch maps `Operation`, `Integrity`, and `Input` alike to 400, including disk-full, and the server logs nothing. The blob is ingested before the name and index checks, so a rejected upload leaves an orphan blob in `state/blobs` that nothing removes.
- **Recommendation:** Add one `Error`-to-status mapping (Input 400, Integrity 422, Conflict 409, Limit 413, Network 502, Operation 500), convert every `Err` into a response with a stderr line, and validate name, version, and manifest before ingest.
- **Fix with:** error-model
- **Effort:** S

### STUF-38 · P3 · mycrates serve-publish reuses fetch limits, so four idle sockets block publishing

- **Area:** Security
- **Where:** `crates/mycrates/src/cli/serve_publish.rs:73`, `crates/mycrates/src/cli/serve_publish.rs:82`, `crates/mycrates/src/serve.rs:406`, `crates/mycrates/src/serve.rs:500`
- **Problem:** The upload deadline is `limits.fetch_timeout_secs.max(30)` and admission uses `limits.concurrent_requests` (the fetch worker count, default 4). Admission happens before the header read and before auth, so four idle connections make every request 503 for 30 s at a time, and tuning fetch changes upload behavior. The documented nginx and Caddy setups proxy only the write API, which limits exposure. Constructed from the code, not run.
- **Recommendation:** Give `serve-publish` its own connection and upload-timeout limits, and authenticate before admitting a slot for writes.
- **Fix with:** security
- **Effort:** S

### STUF-39 · P3 · mycrates fetch holds the exclusive store lock across network I/O

- **Area:** Concurrency
- **Where:** `crates/mycrates/src/acquire.rs:70`, `crates/mycrates/src/acquire.rs:147-172`, `crates/mycrates/src/upstream.rs:67`, `crates/mycrates/src/upstream.rs:211`, `crates/mycrates/src/store.rs:147-192`
- **Problem:** `fetch` takes `lock_exclusive` before planning and holds it up to `fetch_timeout_secs` (default 3600), so `serve-publish` and `publish` block with no timeout. The deadline is checked only between items and the per-read timeout is set with `let _ = set_read_timeout(...)`, so one slow download can outlast the limit. The `shared` mutex is held through ingest and record write, serializing every worker's disk work, and `check_collision_in` scans the whole record directory per write (O(N²) for N crates).
- **Recommendation:** Download unlocked into staging, take the lock only to ingest and write records, release `shared` before ingest, propagate the read-timeout error, and index the collision check by normalized name.
- **Fix with:** lifecycle-concurrency
- **Effort:** M

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
