Prefix: STUF
Next: 27

### STUF-10 · P1 · An interrupted mycrates adopt makes the next publish treat partial state as complete

- **Area:** Data
- **Where:** `deploy/publish:86-93` (init before adopt), `deploy/publish:116` (adoption test); `crates/mycrates/src/adopt.rs:22-70` (per-record writes, one final verify); `deploy/publish-mycrates.sh:92-96` (restore, rebuild, sync).
- **Problem:** `adopt_mycrates` runs `mycrates init`, which creates `state/native`, `state/upstream`, and `state/blobs`, before `adopt` writes records one at a time. Any failure after the first record leaves a non-empty state, and the next `deploy/publish mycrates` tests only whether any record file exists, skips adoption, and publishes only that subset plus the caller's `CRATES`. `publish-mycrates.sh` then syncs and promotes it. `docs/PUBLISHING.md:71` calls exactly this outcome unacceptable. The mynpm and mypypi branches guard against a non-empty site; mycrates has no equivalent guard.
- **Recommendation:** Adopt into a sibling staging directory, run `adopt` and `verify` there, then rename into place and write a completion marker. Gate adoption on the marker, not on file existence.
- **Fix with:** `durable-state`
- **Effort:** M

### STUF-11 · P1 · Native servers buffer unbounded request and response bodies

- **Area:** Security
- **Where:** `crates/stuff-server/src/main.rs:99` (`read_until`), `:106` (cap checked after the read), `:282-287` (non-atomic admission); `crates/mycrates/src/serve.rs:369-378` (same read loop), `:123` (`fs::read` of a full blob), `:432-436` (same admission check).
- **Problem:** `read_until` grows the line until a newline or EOF, so the 8 KiB check never runs while a client streams a line without `\n`; the 30 s timeout is per read, not per request. A local probe against `stuff-server` grew RSS from 2.4 MB to 70 MB with one 64 MiB line and to 135 MB while a second connection was open; 64 connections can OOM the process. `serve-publish` also reads a whole blob (up to 1 GiB by default) into memory per request and admits connections with a load-then-add race. The binary accepts any `--bind` value, so off-loopback exposure is supported; the default loopback bind lowers but does not remove the risk.
- **Recommendation:** Read headers through a hard-bounded adapter (`take`/`fill_buf` with a cap), add a total request deadline, stream blob files with `io::copy`, and use `compare_exchange` for admission.
- **Fix with:** `lifecycle-concurrency`
- **Effort:** M

### STUF-12 · P2 · mypypi subprocess capture deadlocks on output larger than the pipe buffer

- **Area:** Concurrency
- **Where:** `crates/mypypi/src/process_policy.rs:207-233` (pipes, `try_wait` loop, reads only after exit); callers at `crates/mypypi/src/build.rs:619`, `crates/mypypi/src/validate.rs:676`, `crates/mypypi/src/add.rs:199`.
- **Problem:** `run_capture` polls `try_wait` without draining stdout or stderr. A child that writes more than the pipe capacity blocks in `write(2)` and never exits, so the parent spins to the 300/600/1800 s deadline and reports a timeout instead of the real build or resolver error. A probe with a child writing 200 KB timed out at 2 s; the implementation never drained the pipe while the child ran. Output is also uncapped and non-UTF-8 bytes are silently discarded.
- **Recommendation:** Drain both pipes on reader threads (as `mynpm/src/process.rs` already does) or use `wait_with_output` behind the deadline; cap captured bytes per stream and decode with `from_utf8_lossy`.
- **Fix with:** `lifecycle-concurrency`
- **Effort:** M

### STUF-13 · P2 · Untrusted Python build backends inherit the publisher's environment and HOME

- **Area:** Security
- **Where:** `crates/mypypi/src/build.rs:458-462` (`std::env::vars()` when no env is supplied), `crates/mypypi/src/process_policy.rs:10-36,61` (denylist only), `crates/mypypi/src/add.rs:197`, `crates/mypypi/src/validate.rs:670`; `crates/mypypi/src/build.rs:172-178` (bare `tar` extraction).
- **Problem:** `uv build` and `uv pip compile` run an sdist's build backend with every variable not on a 27-name `UV_*`/`PIP_*`/`PYTHON*` denylist, including `R2_S3_*`, `AWS_*`, `*_TOKEN`, and the operator's `HOME`. A compromised or typo-squatted sdist can read publisher secrets and cost the registry its R2 credentials when `--promote=s3` or multipart is in use. The same boundary is bypassed by the bare `tar -xzf` used to inspect sdists: no sanitized environment, no timeout, and no expanded-size cap, so a decompression bomb can exhaust disk before the post-hoc member checks run. Host tar rejected `..` and symlink escapes in a probe.
- **Recommendation:** Build a closed allowlist environment (`PATH`, scratch `HOME`/`TMPDIR`, locale, TLS material, target flags), run `tar` through the same sanitized, bounded process policy, and list/reject members and cap expanded bytes before extraction.
- **Fix with:** `security`
- **Effort:** M

### STUF-14 · P2 · The documented registry exit-code contract is not implemented consistently

- **Area:** Errors
- **Where:** `README.md:22-25`; `crates/mynpm/src/acquire.rs:1336-1353` (worker errors flattened to strings and re-wrapped as `Operation`); `crates/mynpm/src/error.rs:14,84` (`Cancelled` never constructed); `crates/mypypi/src/error.rs:9-29,131-140` (no `Limit` or `Network`, `MissingArtifact` maps to 2); `crates/mycrates/src/validate.rs:241,257` (cargo failures map to 1); `crates/stuff/src/doctor.rs:124-133` and `crates/stuff/src/main.rs` (all `stuff` errors exit 2; doctor's documented 2 is unreachable).
- **Problem:** The shared contract is 2 usage/input, 3 integrity, 4 conflict, 5 limit, 6 network, 1 other, plus 130 for a cancelled `mynpm` run. `mynpm fetch` turns integrity and conflict failures into 1, and its cancellation paths never construct `Cancelled`. `mypypi` cannot emit 5 or 6, and network fetch failures exit 2. `mycrates validate` maps a locked-fetch failure to 1. `stuff` maps every failure to 2 while `doctor` can only exit 1. The same corrupt state therefore produces different codes depending on the command, and a retry wrapper cannot distinguish a bad flag from a network outage.
- **Recommendation:** Give each CLI one error category set and one mapping boundary: usage/input 2, integrity/corrupt 3, conflict 4, limit 5, network 6, other 1, and `mynpm` 130 for cancellation. Preserve the original error through worker joins, add `Limit`/`Network` where missing, and construct `Cancelled` on the cancellation paths. For `stuff`, map usage to 2, corrupt or integrity to 3, fetch or network to 6, and other to 1. `doctor` exits 1 when any check fails and 2 for a usage error. Test the mapping per category.
- **Fix with:** `error-model`
- **Effort:** M

### STUF-15 · P2 · A changed npm lockfile is silently discarded when only dropped fields differ

- **Area:** Data
- **Where:** `crates/mynpm/src/inventory.rs:1274-1296` (`project_stable_string`), `:1564-1570` (`merge_project`); consumers at `crates/mynpm/src/acquire.rs:564` and `crates/mynpm/src/validate.rs:1132-1139`; contract at `crates/mynpm/README.md:16-19`.
- **Problem:** The project identity includes each package's key, name, version, integrity, resolved URL, and source kind, but omits `platform` (`os`/`cpu`/`libc`), `flags`, `aliases`, and the project snapshot. `merge_project` returns the old record when the stable string matches, so a lockfile regenerated with new platform constraints or optional flags is treated as unchanged even though the README says a changed lockfile is recorded as a new import. `plan` then uses the stale platform to decide hosted versus metadata-only, and `validate` uses the stale flags to require or skip packages.
- **Recommendation:** Include the omitted fields and a snapshot digest in the project identity. Use the RFC's accepted append-only migration: a first import after the change appends a duplicate record for every project, so verify duplicate handling before release.
- **Fix with:** `durable-state`
- **Effort:** S
- **Design:** [docs/rfc/STUF-15-import-identity.md](docs/rfc/STUF-15-import-identity.md)

### STUF-16 · P2 · Publish concurrency guards can delete another writer's spool and double-admit writers

- **Area:** Concurrency
- **Where:** `crates/mycrates/src/store.rs:101-112,545-560` (`.upload.*` reclamation by PID); `crates/mycrates/src/store.rs:448-449` (`verify` calls recovery without the lock), `crates/mycrates/src/publish.rs:82` (publish calls it before any lock it later takes); `crates/mycrates/src/serve.rs:274` (the HTTP publish path takes the lock after writing the spool); `deploy/publish-lock.sh:9-26` (`mkdir` lock with `rm -rf` reclaim).
- **Problem:** `recover_staging` treats every `.upload.*` file owned by another PID as stale. `Store::verify` calls it without the store lock, and `Publisher::publish` calls it as its first step, so a concurrent verify or publish can unlink an in-flight upload spool and turn `cargo publish` into a 400. Separately, two waiters that see an empty or stale lock directory can both `rm -rf` it before either re-creates it, so both can acquire the "lock" and run publish helpers against one state directory; a stale PID that is later reused blocks every publish forever.
- **Recommendation:** Keep upload spools in a directory the GC never scans, or reclaim only by age under the lock. Replace the directory lock with an advisory file lock or an atomic rename-steal, and test concurrent publish/verify and two lock waiters.
- **Fix with:** `lifecycle-concurrency`
- **Effort:** M

### STUF-17 · P2 · Failed deploy steps leave a localhost base URL or an unusable live site

- **Area:** Operability
- **Where:** `deploy/publish-mycrates.sh:64,92` (local URL set, restored only on the success path); `deploy/publish:100-107,126-141` (adopt writes into the live site); `deploy/r2-pull.py:121-122` (refuses a non-empty destination); `docs/PUBLISHING.md:71`.
- **Problem:** A failed `cargo publish` exits without restoring `public_base_url`, leaving state and the current site pointing every crate download at `http://127.0.0.1:8080`. A later manual `r2-sync.sh` promotes that generation and takes the registry down. The mynpm and mypypi adopt paths pull straight into `$SITE_DIR`; a failed pull leaves a non-empty directory without `current`, the next publish refuses to touch it, and the docs tell the operator not to delete it without providing a repair path.
- **Recommendation:** Restore the public URL in the cleanup trap and rebuild the site on every exit. Pull and adopt into a sibling staging directory, rename on success, and print the failure path and safe removal instruction.
- **Fix with:** `durable-state`
- **Effort:** S

### STUF-18 · P2 · The mypypi container writes state and site paths the publish wrapper never syncs

- **Area:** Config
- **Where:** `deploy/mypypi-container.sh:14-17` (`.work/registry-pub/*` defaults); `deploy/publish-mypypi.sh:26-31` (`$STUFF_STATE_HOME/mypypi/*`); `deploy/publish-mypypi.sh:34-39` (legacy-state refusal); `README.md` multi-platform recipe.
- **Problem:** The README runs `deploy/mypypi-container.sh` without env overrides for each Linux platform and says all platforms share one catalog, state, and site. With the defaults, the container writes `.work/registry-pub/*` while `deploy/publish mypypi` syncs `$STUFF_STATE_HOME/mypypi/*`. Linux wheels are never promoted, or an operator syncs the wrong site and promotes a generation missing the macOS wheels. The container also creates the `.work/registry-pub/state` directory that `publish-mypypi.sh` refuses as legacy.
- **Recommendation:** Default the container to the same `$STUFF_STATE_HOME/mypypi/*` paths as the wrapper (computed on the host and bind-mounted), and remove the `.work/registry-pub` defaults.
- **Fix with:** `single-source-of-truth`
- **Effort:** S

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

### STUF-23 · P3 · Deployment verification can print OK without verifying

- **Area:** Tests
- **Where:** `deploy/deployed-smoke.sh:27-28` (`first_pkg` swallows every failure); `deploy/test-r2-sync.sh:67` (symlink check reads through the link); `deploy/test-r2-pull.sh` (no Makefile or workflow caller); `.github/workflows/ci.yml:104-106`.
- **Problem:** `first_pkg` sends `stuff ls` stderr to `/dev/null` and ends with `|| true`, so a missing `jaq`, a failed `stuff ls`, or an unexpected JSON shape becomes `skip: registry is empty` and the script still ends with `OK`. `test-r2-sync.sh` checks `$(sed -n 's/^current$//p' "$SITE/current") = ""`; `sed` follows the symlink and reads the generation's file, so the comparison always passes and the intended "current is still a symlink" assertion does not exist. `test-r2-pull.sh` is cited by AGENTS but never run.
- **Recommendation:** Fail when `jaq` or `stuff ls` fails, replace the vacuous check with `[ -L "$SITE/current" ]`, and wire both shell tests into the existing `make test-r2` target.
- **Fix with:** `test-audit`
- **Effort:** S

### STUF-24 · P3 · mycrates add writes pins that fetch can never accept

- **Area:** Errors
- **Where:** `crates/mycrates/src/cli/add.rs:60-66`; `crates/mycrates/src/acquire.rs:135-140`; `crates/mycrates/src/cli.rs:135` (advertises `add`).
- **Problem:** `mycrates add` writes a pin with `checksum: ""`. `fetch` compares the pin checksum against the upstream index and returns an integrity error on the first mismatch, so every pin produced by the command is unusable and one such pin aborts the whole fetch. There is no `--checksum` flag and nothing resolves the checksum before writing.
- **Recommendation:** Resolve the checksum from the configured upstream index before writing the pin, or remove `add` from the parser and help so it cannot produce dead state.
- **Fix with:** `none`
- **Effort:** M

### STUF-25 · P3 · stuff accepts flags no command reads and advertises positionals it rejects

- **Area:** Types
- **Where:** `crates/stuff/src/cli.rs:102,202-206` (`--remote` parsed and never read), `crates/stuff/src/main.rs:257-286` (`render` ignores `--json`), `crates/stuff/src/cli.rs:219,222-223` (help promises `[sources...]`), `:175-177` (`diff` accepts and ignores the base flags).
- **Problem:** The flat `Parsed` struct is handed to every command and each arm hand-rejects a subset of flags. `doctor --remote` is a no-op that the README and `deploy/deployed-smoke.sh` present as meaningful; `render --output x.html --json` writes HTML and exits 0; `diff` accepts base-URL flags it never uses. The help advertises `search ... [sources...]` and `render ... [sources...]` even though the parser rejects every positional except the single search query. Callers cannot tell from a signature which flags a command honors.
- **Recommendation:** Give each command its own argument type so unsupported flags cannot be parsed, or add the missing rejections per arm. Remove `--remote` or make it select remote mode explicitly, and fix the help text.
- **Fix with:** `api-surface-and-module-boundaries`
- **Effort:** M

### STUF-26 · P3 · The shared-store alias skip list is duplicated between r2-sync and the worker

- **Area:** Architecture
- **Where:** `crates/stuff-worker/src/api.rs:135-157` (alias map and the "keep the two lists in agreement" comment); `deploy/r2-sync.sh:132-137` (`generations/*/tarballs/*` skip list); `crates/stuff-worker/test/runtime.test.mjs:100-112` (worker side only).
- **Problem:** The worker resolves generation-scoped `wheels/`, `tarballs/`, and `-/tarballs/` paths to the shared store because `r2-sync.sh` does not upload those objects. The two policy lists live in different languages and no test compares them. A change to the sync skip list or the alias map silently produces either duplicate uploads or a 404 for an artifact that exists. The source comment is the only signal that the lists must agree.
- **Recommendation:** Put the generation-scoped shared-store paths in one data source used by both the sync plan and the worker alias test, or add a drift test that feeds the same fixture through `r2-plan.py` and `alias_key_for`.
- **Fix with:** `single-source-of-truth`
- **Effort:** S
