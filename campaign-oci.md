# Campaign: OCI Distribution 1.1 registry (`myoci`)

## Status

- **Goal:** `stuff` serves and accepts OCI images as a fourth registry. Read-only first: import a multi-architecture image, publish an immutable generation, pull anonymously through the Worker and `myoci serve`, and pass the OCI read conformance suite. Then native, authenticated `docker push` through a separate blob writer, passing the push conformance suite.
- **State:** not started. Baseline `e914ba5`; the baseline is red in CI (see **Baseline**).
- **Accepted:** none.
- **Next:** P1.
- **Harness:** `make test-oci-conformance-fixture` is green against the checked-in fixture (29 rows); the executor inherits a working conformance gate.
- **Resume:** `Execute campaign-oci.md from item P1. Read the item, own only its files, and finish on its acceptance command.`

## Charter

- Decide and keep going. Record a decision only when a later slice depends on it; write it into **Decisions**, not into a log.
- The owner's execution grant covers local edits, local test runs, local R2 simulation (`CF_LOCAL_STATE`), and `.work/` scratch. It never covers pushes, deploys, bucket or DNS creation, live R2 writes, or secrets.
- Hard stops park the item, never the campaign: a live write, a boundary in **Execution boundary**, or a check that cannot run here. Put the proof on **Needs environment** with the command the owner runs.
- Parity before optimization. A10 and A11 are the only optimization items; nothing else may be moved ahead of them.
- One writer by default. Parallel lanes only for the disjoint pairs named in **Sequence**, in worktrees at `.work/wt/<lane>`, never `/tmp`; set `TMPDIR="$PWD/.work/tmp"` for every command that writes scratch.
- Every slice ends on its acceptance command and the narrowest affected suite, run on the integrated tree. The full suite runs at M1, M2, M3, and close.
- Red work never lands. A failing item gets root-cause first, then at most two fix attempts, then it is parked on its branch with the cause and retry condition.
- Fix anything egregiously slow or broken met on the way, in its own commit. Egregious means: a command hangs beyond twice its **Baseline** time; a warm loopback `/v2/` or manifest request exceeds 500 ms p95; a loopback blob read under 20 MiB/s. Record anything else and leave it.
- Every commit is a Conventional Commit whose scope is the workstream (`stuff-oci`, `myoci`, `stuff-catalog`, `stuff`, `worker`, `deploy`, `docs`). The item ID belongs in the body. Code comments never cite this doc.

## Decisions

### Owner (2026-11-01)

- **O1 · Scope.** Read-only Stage A lands first, then native authenticated push (Stage B) in this campaign. Both stages are in scope; a slice of B never starts before A's local acceptance passes.
- **O2 · Blob path.** Blobs are served by the Worker until parity. The 307 direct-CDN path is A11, adopted only if A10 measures at least a 10% improvement in warm p95 and full-pull wall time with non-overlapping p95 over five interleaved runs; otherwise A11 closes with the numbers and no code.
- **O3 · Exposure.** The registry is public and anonymous for reads. No private images, per-repository authorization, or token service. Writes use one registry token and no per-repository scopes.
- **O4 · Naming.** The registry is OCI, the CLI is `myoci`, and Docker is a client, not the model.

### Prescribed

- **S1 · Crate split.** `crates/stuff-oci` is an I/O-free, `#![forbid(unsafe_code)]`, wasm-safe protocol core: digests, references, media types, manifest/index parsing, storage keys, request parsing, response planning, OCI errors, and upload-session tokens. `crates/myoci` is the CLI and native server. `stuff-worker` and `myoci serve` share `stuff-oci`, the same way `stuff-server` and the Worker share `stuff-http`. Reason: one protocol implementation; the wasm bundle must not inherit the CLI's HTTP client.
- **S2 · Protocol subset.** Implement exactly: `GET /v2/`; `GET`/`HEAD /v2/<name>/manifests/<tag|digest>`; `GET`/`HEAD /v2/<name>/blobs/<digest>` with single ranges; `GET /v2/<name>/tags/list` with `n`/`last`; `GET /v2/<name>/referrers/<digest>` with `artifactType` and `n`/`last`. Reject `_catalog`, schema 1, legacy v1, and upload/delete methods in Stage A with OCI error JSON. Stage B adds the upload, mount, and manifest PUT endpoints; delete endpoints stay unimplemented and the conformance delete flags stay off.
- **S3 · Media policy.** Accept OCI image manifest and index, Docker schema 2 manifest and list, OCI config, OCI gzip and zstd layers, Docker gzip layers, in-toto layers, and arbitrary `artifactType` values. Reject `application/vnd.docker.distribution.manifest.v1+json`, foreign/nondistributable layers (`application/vnd.docker.image.rootfs.foreign.*`, `application/vnd.oci.image.layer.nondistributable.*`), and every non-`sha256` digest. A zstd layer is stored and served unchanged.
- **S4 · Storage.** Metadata bucket `stuff-oci`:
  - `current` -> `generations/<id>`
  - `generations/<id>/repositories/<repo>/repository.json` (tags -> descriptor, all manifest descriptors)
  - `generations/<id>/repositories/<repo>/referrers/sha256/<subject>.json` (an OCI index)
  - `generations/<id>/blobs.json` (sorted digests reachable from this view)
  - `generations/<id>/catalog.json`, `index.html`, `llms.txt`
  - `oci/manifests/sha256/<hex>` (shared, immutable manifest bytes)
  - `manifests/<id>.json` (the sync/GC manifest, one per generation)
  Blob bucket `stuff-oci-blobs`: `sha256/<hex>` only, immutable, no pointer and no metadata. Reason: a public CDN domain can expose the blob bucket without exposing metadata; GC can prune it independently; a 1 GiB layer never enters the metadata generation.
- **S5 · Read cost.** A manifest request reads at most the generation pointer, one `repository.json`, and one manifest object. A blob request reads only `sha256/<hex>` from the blob bucket. No listing, no bucket scans, no hashing, no parsing on the serving path.
- **S6 · Tag and generation semantics.** A tag is a mutable reference inside an immutable generation. A publish or push that changes any tag, manifest, or referrer produces a new content-derived generation (`sha256` over the canonical repository view, first 24 hex). Re-publishing the identical view is a no-op. A tag move is activated atomically with `current`; rollback is `deploy/r2-rollback.sh stuff-oci <id>`.
- **S7 · Catalog.** `stuff_catalog::SCHEMA_VERSION` becomes 4 with `Registry::Oci`. One package per repository, one version per tag, `latest` = the tag named `latest` otherwise the lexicographically greatest tag, versions sorted lexicographically, `yanked=false`, no dependencies. `stuff ls`, `search`, `render`, `doctor`, the aggregate page, and `llms.txt` include OCI. Deploy the Worker and `stuff` CLI before publishing OCI: schema 4 is unreadable by older readers.
- **S8 · Import.** `myoci import --layout DIR --repository NAME [--tag TAG]` imports an OCI image layout natively. `--from REGISTRY/REPO:TAG` runs the pinned `oras` (`v1.3.6`, installed with `go install oras.land/oras/cmd/oras@v1.3.6`) into a scratch layout, then imports it. Import verifies every digest and size, walks indexes recursively, retains attestation children and `subject` referrers, and rejects a sparse index, a missing referenced blob, a mismatched digest/size, a foreign layer, or schema 1. The tag comes from `--tag`, else the layout's `org.opencontainers.image.ref.name`, else `latest`.
- **S9 · Push architecture.** The registry front owns the protocol, authentication, manifest validation, generation creation, and the `current` flip. A separate `myoci writer` owns only blob bytes: it accepts `PATCH`/`PUT` upload requests, streams to disk, verifies the digest, and stores `sha256/<hex>` in R2 through the existing `deploy/r2-put.py` / `deploy/r2-multipart.py` helpers. The front issues an HMAC-signed, one-hour, repo-bound session URL on the writer; the writer never accepts registry credentials. The Worker and Docker do not forward the `Authorization` header across hosts (verified), so the signed URL is the writer's only authentication. This is the bounded architecture for large layers; a Worker-body-limit upload path is not built.
- **S10 · Push authentication.** `Authorization: Basic base64("stuff:" + token)` on every write request to the front. The token is the Worker secret `STUFF_OCI_PUSH_TOKEN` (or `--push-token-file` locally). Missing or wrong credentials get 401 with `WWW-Authenticate: Basic realm="stuff-oci"`; comparison is constant-time over SHA-256 digests, as in `crates/mycrates/src/serve.rs`. The writer verifies only the HMAC session token (`STUFF_OCI_PUSH_SECRET`, shared with the front).
- **S11 · Manifest activation.** A manifest PUT is accepted only if every referenced blob exists in the blob bucket and every child manifest of an index exists in the manifest store. The new generation is built from the current view, written, and activated with `current` conditional on the ETag read at the start; on 412 rebuild and retry up to five times. A failed validation leaves the old generation current.
- **S12 · Sync and GC.** Each generation has `manifests/<id>.json`; the local publisher writes it with `r2-manifest.py`, and the Worker writes it during push from the ETags it receives. Blob GC is separate: `deploy/r2-gc-blobs.py` unions `blobs.json` from the retained meta generations and deletes unreferenced `sha256/*` from the blob bucket, with the existing GC's `--dry-run` preview.
- **S13 · Limits.** Repository name <= 255 characters and must match the distribution grammar; tag <= 128 and must match `[A-Za-z0-9_][A-Za-z0-9._-]*`; manifest body <= 4 MiB; a manifest descriptor <= 4 MiB; upload sessions live 1 hour; one upload <= 20 GiB. Oversize or malformed input is a loud 4xx with the OCI error code, never a truncated write.
- **S14 · Conformance gate.** Pin `opencontainers/distribution-spec` commit `97274622c11112caa21efb8c52acca3c6b8fa7f1`. Build the suite once into `.work/tools`. Apply `deploy/oci-conformance-report.patch` (the two-line nil guard for `blobs-sha256`/`blobs-sha512` in read-only mode) with `git apply --check` first; if it no longer applies, stop and record the upstream change. Stage A runs `OCI_API_PUSH=false` with `OCI_RO_DATA_*` filled from `myoci inspect --json`; Stage B runs with push on and deletes off. The gate passes only when every enabled API and data row in `results.yaml` is `pass` or `disabled`.
- **S15 · uv pin.** `deploy/uv-version` is the only source of the uv version (`0.13.2`). `crates/mypypi/src/build.rs` includes it, both CI setup-uv steps read it into `env.UV_VERSION`, and the Linux Dockerfile reads the same file. Reason: the code, container, README, and CI currently disagree, which is the baseline's ecosystem failure.
- **S16 · Worker build.** CI must run `npm run build` (which runs `build-wasm.sh` before `tsrs`) and then `npm run test:runtime`. `entry.ts` imports generated wasm that only `scripts/build.mjs` creates; `cf build` alone cannot work. Reason: the baseline worker failure.
- **S17 · Bug discipline.** A failing test is never weakened, skipped, or relabeled. P3 keeps the 5 s bound and fixes the Linux-only 30 s path. Every acceptance command in this doc is the pass condition; a subcommand that cannot run here goes to **Needs environment**.
- **S18 · Benchmarks.** `myoci bench` drives the HTTP API with the sync client across the matrix: manifest GET/HEAD at 1, 8, 32 concurrency; blobs of 1 MiB, 64 MiB, 256 MiB, 1 GiB; cold and warm; a full multi-platform pull and a repeated pull on a clean client. Five runs, interleaved baseline/candidate, report p50/p95/p99 and bytes per second. Cold on the deployed Worker means a freshly published blob digest (a new cache key), because the gateway cache key ignores query strings; record that method. Every number is recorded in `docs/oci-benchmarks.md` with the command and environment. No absolute target gates the campaign; A11's 10% rule is the only adoption gate.
- **S19 · Failure codes.** Reuse the shared exit-code contract: `2` usage/input, `3` integrity, `4` conflict, `5` limit, `6` network, `1` other. `myoci verify` exits 3 on any digest, size, tag, or reference mismatch; `myoci import` exits 3 on a bad source and 4 when a tag would be replaced with different bytes.
- **S20 · Local simulation.** Publish, sync, GC, and push-writer R2 tests use `CF_LOCAL_STATE` under `.work`; they never touch live R2. The Worker tests use Miniflare with seeded R2 bindings. Docker tests use 127.0.0.1, which Docker treats as an insecure registry without extra configuration.
- **S21 · Accept is advisory.** Manifest GET and HEAD return the stored media type and never 404 a manifest that exists because the request's `Accept` list omits that type. Reason: the conformance read-only data fetches each digest with one media type, and Docker/containerd accept either family; strict negotiation turns valid indexes into false 404s. The stored type is authoritative.

### Delegated

- **D1 · Internal names and error text.** The executor chooses function, module, and message names inside the prescribed contracts.
- **D2 · Fixture bytes.** `tests/fixtures/oci/` is checked in: a two-platform image index tagged `stable`, an untagged referrer with a `subject`, and every referenced blob. `tests/fixtures/oci/build.py --check` pins the bytes. The executor may extend it; it may not re-generate it with different bytes without updating the README digests and the scripts that use them.
- **D3 · Test split.** Unit tests live in the crate; cross-process and Docker checks live in `deploy/test-oci-*.sh`. The executor may merge two script checks into one item's command, not two items.
- **D4 · Upstream sample.** `docker.io/library/alpine:3.20` is the network import sample; pin its index digest in the script when a mutable tag would make the check flaky.
- **D5 · Ordering inside a wave.** The executor may reorder items that touch disjoint files; it may not move an optimization item earlier or a prerequisite later.

## Baseline

Baseline revision `e914ba5`, 2026-11-01.

Known red in CI (run `41314993528`, `main`):

| Job | Failure | Root cause |
| --- | --- | --- |
| `rust (ubuntu-24.04)` | `mynpm::process::tests::output_overflow_kills_the_child_before_joining_readers` fails at `crates/mynpm/src/process.rs:274` after 30.00 s | Linux-only: the overflow is observed only after the child exits; macOS passes in 0.21 s |
| `ecosystem-smoke` (all 3) | `mypypi: uv 0.13.2 is required; found 0.13.5` | CI installs 0.13.5; code, README, and container pin 0.13.2 (`crates/mypypi/src/build.rs:21`, `crates/mypypi/README.md:58`, `deploy/mypypi-linux.Dockerfile:4`, `.github/workflows/ci.yml:54,72`) |
| `pipeline-independence` | same uv failure | same |
| `worker-runtime` | `[UNRESOLVED_IMPORT] Could not resolve './build/wasm/stuff_worker.js'` | `.github/workflows/ci.yml:100` runs `cf build`, which skips `scripts/build.mjs:19` (`build-wasm.sh`); `entry.ts:12-13` imports generated files |

The `rust (macos-26)` job was cancelled, not failed.

Loop timings measured on this host (warm `target/`, 2026-11-01):

| Command | Time |
| --- | --- |
| `cargo test -q -p mypypi --lib` | 2.6 s |
| `cargo test -q -p mynpm --lib` | 8.0 s (the overflow test fails in a Linux container; see P3) |
| `cargo test -q -p stuff-catalog -p stuff-server` | 10.2 s |
| `cargo test -q --workspace` | 67.8 s |
| `npm run build` in `crates/stuff-worker` | 1.8 s warm |
| `make worker-test` | 3.7 s warm |
| `oras cp docker.io/library/alpine:3.20 --to-oci-layout` | 2.7 s (network) |
| Docker pull from a static fixture registry | 0.2 s |
| OCI conformance build (modules cached) / read-only run | 1.5 s / 0.5 s |
| `make test-oci-conformance-fixture` (measured harness, checked in) | 0.6 s after the tool build; 29 rows pass |
| `deploy/test-r2-sync.sh 10` / `50` | 43 s / 185 s |
| `make test-r2` (`SYNC_OBJECTS=1100`) | did not finish in 20 min; the 1100-object sync dominates. Use `SYNC_OBJECTS=10` for slice checks, `make test-r2 SYNC_OBJECTS=50` at M2, and the full 1100 run at close |

Toolchain: `rust-toolchain.toml` nightly-2026-09-15 (rustc 1.100.0-nightly), Node 26.12.1, npm 12.2.1, uv 0.13.2, Go 1.27.3, Docker client 27.3.1 against daemon 29.8.2 (containerd image store), oras 1.3.6.

Proved before writing the plan:

- A multi-platform OCI layout built by `docker buildx` pulls with Docker 29 from a static server that only resolves tag -> digest and serves blobs.
- Docker 29 follows a 307 blob redirect to a different host and verifies the layer digest.
- Docker pushes the whole blob to a cross-host upload `Location`, does not forward `Authorization` there, then PUTs the manifest to the original registry; `docker login` Basic auth works on the front.
- The read-only conformance suite passes against the checked-in fixture when the report patch is applied; without the patch, the read-only report writer panics on `blobs-sha256`/`blobs-sha512`. The harness, patch, fixture oracle, and fixture are checked in and green (`make test-oci-conformance-fixture`).

## Sequence

Three waves, then close. One writer by default; two at most, and only in these configurations, after `perf-gate-ready -s` reports headroom:

- After A3: writer 1 runs A4, then A6, then A8 serially; writer 2 runs A5, then A7. A5 and A7 own disjoint trees (`crates/stuff-worker/**` and `crates/stuff/**`).
- After B1: writer 1 runs B2, then B4, then B5; writer 2 runs B3. Only B2 may edit `crates/stuff-oci/**`; B3 reports a needed core fix instead of editing it.

A4, A6, and A8 share `Makefile`, so they never run in parallel. P1 and P2 share `.github/workflows/ci.yml`, so they never run in parallel.

Milestones run the wider suites; slices do not:

- **M1 after A4:** `cargo test -q --workspace`.
- **M2 after A9:** `cargo test -q --workspace`, `cargo clippy -q --workspace --all-targets -- -D warnings`, `make test-r2 SYNC_OBJECTS=50`, `make pipecheck`.
- **M3 after B5:** the M2 set plus `make test-oci-conformance` and `make test-oci-conformance-push`.
- **Close:** `make test-r2` at the default 1100 objects, `cargo test --workspace`, both conformance targets, and `make oci-bench`.

### P0 — ground

**P1 · uv single pin · S**
Owns: `deploy/uv-version`, `crates/mypypi/src/build.rs`, `crates/mypypi/README.md`, `deploy/mypypi-linux.Dockerfile`, `deploy/mypypi-container.sh`, `.github/workflows/ci.yml`, `deploy/check-uv-pin.sh`.
Prereq: none.
Acceptance: `deploy/check-uv-pin.sh && cargo test -q -p mypypi --lib`. The check fails if any of the four files names a uv version different from `deploy/uv-version`.

**P2 · worker bundle build · S**
Owns: `.github/workflows/ci.yml`, `crates/stuff-worker/README.md`.
Prereq: none.
Acceptance: `rm -rf crates/stuff-worker/build crates/stuff-worker/.cloudflare && make worker-test && make worker-check` from a clean generated state. The build must run the wasm step before `tsrs`; the runtime suite must pass.

**P3 · mynpm overflow on Linux · M**
Owns: `crates/mynpm/src/process.rs`, `deploy/test-linux.sh`.
Prereq: none.
Acceptance: `cargo test -q -p mynpm --lib process::tests` on the host, and the same test inside the repo's Linux container (`deploy/test-linux.sh cargo test -q -p mynpm --lib process::tests`) with the overflow test under 5 s. Root-cause the drain/kill ordering before changing code; do not relax the assert.

### A — read-only registry

**A1 · protocol core · L**
Owns: `crates/stuff-oci/**`, root `Cargo.toml`, `Cargo.lock`.
Prereq: P1-P3.
Work: digest, repository/tag/reference grammar, media types, descriptor, manifest/index parse and validate, `repository.json` view, `blobs.json`, referrer index build and filter, `/v2` request parse, response plans, and the OCI error table. No I/O, no `std::fs`, no `std::net`; Stage B adds the upload and session plans in B1.
Acceptance: `cargo test -q -p stuff-oci && cargo clippy -q -p stuff-oci --all-targets -- -D warnings`. The test table must cover every row of the **Read API parity** table below.

**A2 · import into state · M**
Owns: `crates/myoci/**`, `tests/fixtures/oci/**`, `deploy/test-oci-import.sh`, root `Cargo.toml`, `Cargo.lock`.
Prereq: A1.
Work: `myoci init`, `import --layout`, `import --from` (oras orchestration), `inspect --json` (used by the conformance script), state layout, digest/size verification, tag replacement rules, rejection cases from S8.
Acceptance: `cargo test -q -p myoci` (offline; the checked-in fixture covers a two-platform index and a referrer with a `subject`) and `deploy/test-oci-import.sh` (network; `alpine:3.20` at its pinned digest).

**A3 · publish and catalog · L**
Owns: `crates/myoci/**`, `crates/stuff-catalog/**`, root `Cargo.toml`, `Cargo.lock`.
Prereq: A2.
Work: generation build/verify/activate through `stuff_store::Site`, shared manifest store, referrer indexes, `blobs.json`, schema 4 and `Registry::Oci`, HTML and `llms.txt`, `myoci verify`, `myoci adopt`, idempotent republish, tag removal.
Acceptance: `cargo test -q -p myoci -p stuff-catalog`. Assert the exact site tree, schema 4 bytes, no-op republish, a tag move producing a new generation, a removed tag disappearing, and `verify` failing on a tampered blob, manifest, tag, and referrer.

**A4 · native read server and real pulls · L**
Owns: `crates/myoci/**`, `deploy/test-oci-pull.sh`, `Makefile`.
Prereq: A3.
Work: `myoci serve --site-dir DIR`; GET/HEAD; ranges; OCI headers; cache policy; 405 for writes; thread-per-connection with a connection cap and the same admission/timeout shape as `stuff-server`.
Acceptance: `make test-oci-pull`. It starts the server on an ephemeral port, asserts the curl matrix (ping, HEAD, 206 bytes, tags pagination, referrers with and without `artifactType`, 404/400 JSON), then `docker pull` by tag and digest (assert the arm64 child), then `oras manifest fetch` and `oras blob fetch`. Under 90 s warm.

**A5 · Worker read path · L** *(lane with A7 after A3)*
Owns: `crates/stuff-worker/**`, `crates/stuff-oci/**` only for fixes.
Prereq: A1, A3.
Work: OCI host branch before `stuff_http::plan_route`; `OCI` and `OCI_BLOBS` bindings; bounded R2 reads; `Docker-Content-Digest`, media types, ranges, 405 JSON; Miniflare fixtures for the A4 matrix.
Acceptance: `make worker-test && make worker-check`. The runtime suite must cover ping, tag and digest manifests with mixed `Accept` lists, blob get/HEAD/range, tags pagination, referrers, 404/400/405, and the cache-control header per route.

**A6 · read conformance gate · M**
Owns: `deploy/oci-conformance.sh`, `deploy/oci-conformance-report.patch`, `deploy/oci-fixture-registry.py`, `tests/fixtures/oci/**`, `Makefile`.
Prereq: A4.
Work: the harness, patch, oracle, and fixture are checked in and green against the fixture (`make test-oci-conformance-fixture`). Add the `myoci serve` target: start the server on the fixture site, fill RO data from `myoci inspect --json`, run the pinned suite read-only, and parse `results.yaml` with `jaq --from yaml`.
Acceptance: `make test-oci-conformance`. Every enabled API/data row is `pass` or `disabled`; a missing `results.yaml` or a patch that no longer applies fails. Re-running without a rebuild is under 10 s.

**A7 · CLI, catalog, and doctor · M** *(lane with A5)*
Owns: `crates/stuff/**`, `crates/stuff-catalog/**` only if A3 left a gap.
Prereq: A3.
Work: `--registry oci`, `--oci-dir`, live origin `https://oci.17166969.xyz` (with `STUFF_OCI_BASE` override), `ls`, `search`, `render`, `doctor` (ping, manifest by tag, config blob hash, catalog), aggregate origins.
Acceptance: `cargo test -q -p stuff` and `deploy/test-oci-stuff.sh` against the A3 fixture site: `stuff ls --registry oci --oci-dir <site>`, `stuff render --registry oci --oci-dir <site> --output .work/tmp/oci.html` (assert the repo name), `stuff doctor --oci-dir <site>`.

**A8 · publish and sync · L**
Owns: `deploy/publish`, `deploy/publish-myoci.sh`, `deploy/r2-sync-blobs.sh`, `deploy/r2-gc-blobs.py`, `deploy/test-r2-gc-blobs.py`, `deploy/test-oci-publish.sh`, `deploy/check-pipeline-independence.sh`, `deploy/deployed-smoke.sh`, `deploy/ecosystem-smoke.sh`, `Makefile`, `.github/workflows/ci.yml`.
Prereq: A3, A4.
Work: `deploy/publish oci`; adoption; lock; blobs-first sync, then metadata; per-generation manifest; blob GC; pipeline independence for `stuff-oci`/`stuff-oci-blobs`; `make deploy` includes OCI only after the owner provisions (C1/Needs environment).
Acceptance: `deploy/test-oci-publish.sh` (offline, `CF_LOCAL_STATE`: publish -> sync meta and blobs -> GC keeps referenced blobs and drops an unreferenced one -> rollback re-points `current`), `deploy/test-r2-gc-blobs.py`, and `deploy/test-r2-sync.sh 10` (43 s). `make test-r2 SYNC_OBJECTS=50` and `make pipecheck` run at M2; the full 1100-object `make test-r2` runs at close.

**A9 · aggregate page · S**
Owns: `crates/stuff-worker/cloudflare.config.ts`, `crates/stuff-worker/entry.ts`, `crates/stuff-worker/src/api.rs`, `crates/stuff-catalog/**` only for origins, `crates/stuff-worker/test/runtime.test.mjs`.
Prereq: A5, A7.
Work: add `oci.17166969.xyz` to domains and `STUFF_ROUTES`; fetch four catalogs on the unified host; render and link OCI; name the registry in notices; `llms.txt` origin.
Acceptance: `make worker-test && make worker-check`, with the unified page and `/llms.txt` listing OCI and its origin.

**A10 · benchmark harness and numbers · M**
Owns: `crates/myoci/**` (bench), `deploy/oci-bench.sh`, `docs/oci-benchmarks.md`, `Makefile`.
Prereq: A4, A9.
Work: S18 harness and report.
Acceptance: `deploy/oci-bench.sh --base-url http://127.0.0.1:<port> --repo example/app --tag stable` runs the matrix and rewrites `docs/oci-benchmarks.md`; `make oci-bench` runs the same with the default local fixture. The file contains the command, host, and all percentiles.

**A11 · direct CDN blobs · M — conditional**
Owns: `crates/stuff-worker/**`, `deploy/publish-myoci.sh`, `cloudflare.config.ts`.
Prereq: A10.
Work: when `STUFF_OCI_BLOB_ORIGIN` is set, blob GET/HEAD returns 307 to `<origin>/sha256/<hex>` with `Docker-Content-Digest`; otherwise serve from `OCI_BLOBS`.
Acceptance: `deploy/test-oci-redirect.sh` (two local servers; Docker pulls through the 307 and the blob server sees the request) plus the A10 comparison in `docs/oci-benchmarks.md`. Adopt only under O2's 10% rule; if not adopted, close the item with the numbers and no code.

### B — native push

**B1 · upload protocol and sessions · M**
Owns: `crates/stuff-oci/**`.
Prereq: A1.
Work: upload request parsing (`POST`, `PATCH`, `PUT`, `GET` status, `DELETE` cancel, `mount`, `from`), response plans, HMAC-SHA256 session token encode/verify/expiry, manifest PUT validation, referrers update on `subject`, auth challenge payloads, error codes.
Acceptance: `cargo test -q -p stuff-oci`. Tampered tokens, expired tokens, wrong repo, wrong digest, oversize manifest, and missing blob must each fail with the prescribed code.

**B2 · push front and writer · L** *(lane with B3)*
Owns: `crates/myoci/**`, `deploy/test-oci-push.sh`, `Makefile`.
Prereq: B1, A4.
Work: `myoci serve` push mode and finalize; `myoci writer` (local and R2 sinks); signed locations; streaming to a temp file; digest check; generation rebuild and `current` flip under a lock.
Acceptance: `make test-oci-push`. Two local processes: front and writer. `docker login`, `docker push` a two-platform image with a 1 GiB layer, `docker pull` it back by digest, `oras attach` a referrer and fetch referrers, cross-repo mount, 401/400/409 cases, and a re-push that is a no-op. Under 2 min warm.

**B3 · Worker push front · L** *(lane with B2)*
Owns: `crates/stuff-worker/**`, `crates/stuff-worker/cloudflare.config.ts`, `crates/stuff-oci/**` only for fixes.
Prereq: B1.
Work: auth; session POST with writer `Location`; mount; manifest PUT; blob HEADs; shared manifest write; new generation from the current view; `manifests/<id>.json`; `current` conditional flip with retry; referrers update.
Acceptance: `make worker-test && make worker-check`. Miniflare cases: 401, session Location, mount hit/miss, manifest finalize with pre-seeded blobs, missing blob -> 400, concurrent flip -> retry, `current` unchanged on failure.

**B4 · writer deployment · M**
Owns: `deploy/push-writer.sh`, `deploy/test-oci-push-r2.sh`, `docs/PUBLISHING.md` only for the secrets table.
Prereq: B2.
Work: launch script, env contract, temp-file lifecycle, R2 multipart for large blobs through the existing helpers, local simulation test.
Acceptance: `deploy/test-oci-push-r2.sh` (offline `CF_LOCAL_STATE`: a 1 GiB and a small blob through the writer land in the fake blob bucket with correct sizes/ETags; the blob GC keeps them). Live host is **Needs environment**.

**B5 · push conformance gate · M**
Owns: `deploy/oci-conformance.sh`, `Makefile`.
Prereq: B2, B3.
Work: push mode against `myoci serve` + writer with `OCI_API_PUSH=true`, deletes off, tag params off; clean state per run.
Acceptance: `make test-oci-conformance-push`. Every enabled API/data row is `pass` or `disabled`; no `fail` and no panic.

### C — close

**C1 · harden, document, close · M**
Owns: `README.md`, `docs/PUBLISHING.md`, `AGENTS.md`, `crates/myoci/README.md`, `crates/stuff-oci/README.md`, `docs/oci-benchmarks.md`, `TODO.md`.
Prereq: A1-A11, B1-B5.
Work: delete dead code; update the module map, publishing, verification, rollback, GC, secrets, and limits; add the OCI row to the catalog/renderer notes; run `ultrareview`; fix P1/P2.
Acceptance: `cargo test -q --workspace && cargo clippy -q --workspace --all-targets -- -D warnings && make test-r2 && make test-oci-conformance && make test-oci-conformance-push && make oci-bench`, then the final report. The live checks stay on **Needs environment**; they do not hold the campaign open.

## Reference

### Read API parity

The reference is the OCI Distribution Specification 1.1 and its conformance suite; zot is the behavioral reference for repository semantics, not a source to port.

| Request | Required behavior | A1 test | End-to-end |
| --- | --- | --- | --- |
| `GET /v2/` | 200, `Docker-Distribution-Api-Version: registry/2.0` | `ping` | A4 curl, A5 Miniflare, A6 |
| `GET`/`HEAD /v2/<name>/manifests/<tag>` | resolve tag -> descriptor; stored media type; `Docker-Content-Digest`; 404 `MANIFEST_UNKNOWN` | `manifest_by_tag`, `manifest_head_by_tag` | A4 Docker/ORAS, A5, A6 |
| `GET`/`HEAD /v2/<name>/manifests/<digest>` | descriptor lookup; 404; no body read on HEAD | `manifest_by_digest`, `manifest_head_by_digest` | A4, A5, A6 |
| `Accept` handling | serve the stored type even when the list omits it (S21) | `manifest_stored_type` | A4, A5, A6 |
| `GET`/`HEAD /v2/<name>/blobs/<digest>` | blob bucket; `Docker-Content-Digest`; range 206/416; 404 `BLOB_UNKNOWN` | `blob_get`, `blob_head`, `blob_range` | A4, A5, A6 |
| `GET /v2/<name>/tags/list` | sorted tags; `n`/`last`; `Link` when more; 404 `NAME_UNKNOWN` | `tags_list`, `tags_pagination` | A4, A5, A6 |
| `GET /v2/<name>/referrers/<digest>` | precomputed index; `artifactType` filter; empty index for unknown subject; pagination | `referrers`, `referrers_filter` | A4, A5, A6 |
| Any other `/v2` path | 404 `NAME_UNKNOWN`; writes 405 `UNSUPPORTED` in Stage A | `unsupported` | A4, A5 |
| Malformed name/digest/tag | 400 with the matching code | `invalid_input` | A4, A5, A6 |

### Push API parity

| Request | Required behavior | B1 test | End-to-end |
| --- | --- | --- | --- |
| `POST /v2/<name>/blobs/uploads/` | 202; signed writer `Location`; `Docker-Upload-UUID`; mount hit 201 | `upload_start`, `mount` | B2 Docker, B3, B5 |
| `PATCH`/`PUT` upload | 202/201; streamed; digest and size verified; 400 `DIGEST_INVALID` on mismatch | writer tests | B2, B4, B5 |
| `GET`/`DELETE` upload | status 204 with `Range`; cancel 204 | `upload_status`, `upload_cancel` | B2, B5 |
| `PUT /v2/<name>/manifests/<tag|digest>` | 201; digest; blob/child existence; new generation; conflict retry | `manifest_put` | B2, B3, B5 |
| Auth | Basic on every write; 401 challenge; constant-time | `auth` | B2, B3, B5 |

### Code map

- Protocol core to add: `crates/stuff-oci/src/{digest,reference,media,manifest,view,referrers,request,response,error,session}.rs`.
- CLI/server to add: `crates/myoci/src/{cli,import,publish,serve,writer,bench}.rs`.
- Catalog: `crates/stuff-catalog/src/model.rs:6` (`SCHEMA_VERSION`), `:10` (`Registry`), `html.rs:66`, `llms.rs:11` (`CatalogOrigins`).
- Store: `crates/stuff-store/src/site.rs` (`Site::publish`, activation, staging), `generation.rs` (id grammar).
- HTTP: `crates/stuff-http/src/route.rs` (`plan_route`), `cache.rs`, `condition.rs`.
- Worker: `crates/stuff-worker/src/map.rs:67` (`map_asset`), `src/api.rs` (plan JSON), `entry.ts` (fetch), `cloudflare.config.ts` (routes/bindings/domains).
- CLI: `crates/stuff/src/cli.rs` (flags), `source.rs` (live/local catalogs), `doctor.rs`.
- Publish/sync/GC: `deploy/publish:60` (registry dispatch), `deploy/r2-sync.sh:156` (shared-store skip), `deploy/r2-gc.py:157` (retention), `deploy/shared-store-paths.txt`, `deploy/r2-manifest.py`.

### Prior art

- Protocol and edge reference: <https://github.com/cloudflare/serverless-registry> (large-push limits; do not port the TypeScript application).
- Conformance: <https://github.com/opencontainers/distribution-spec/tree/main/conformance> (pin S14).
- Repository semantics and conformance discipline: <https://github.com/project-zot/zot>.
- Earlier Workers/R2 prototype: <https://github.com/chainguard-dev/crow-registry>.
- Rust implementation reference: <https://github.com/trow-registry/trow>.
- Client and copy tool: <https://github.com/oras-project/oras>.
- Cloudflare request-body and cache-object limits: <https://developers.cloudflare.com/workers/platform/limits/>.
- Client-side lazy pulling (out of scope server-side): <https://github.com/containerd/stargz-snapshotter>, <https://github.com/dragonflyoss/nydus>.

## Acceptance

Campaign-level deliverables, each with the proof and the distinct failure condition:

1. **A multi-platform image is imported and published locally.** `cargo test -q -p myoci` against the checked-in fixture + `make test-oci-pull`. Fails on any digest/size mismatch, a missing child manifest, a foreign layer, or a pull digest mismatch. The network sample (`alpine:3.20` through `oras`) is real-world interop proof on **Needs environment** when the network is absent.
2. **Docker 29 and ORAS pull by tag and digest.** `make test-oci-pull`. Fails on a wrong architecture, a missing header, a range that is not 206 with the requested bytes, or a pull of a digest that was not published.
3. **The read conformance suite passes read-only.** `make test-oci-conformance`. Fails on any `fail` row, a missing `results.yaml`, or a patch that no longer applies.
4. **The catalog is derived from published state and includes OCI.** `cargo test -q -p myoci -p stuff-catalog -p stuff` + `deploy/test-oci-stuff.sh`. Fails if a tag or manifest appears that the generation does not contain, or if the aggregate render omits OCI.
5. **Publication is atomic, independent, and reversible.** `deploy/test-oci-publish.sh`. Fails if another bucket changes, if `current` moves on a failed verification, or if rollback does not restore a retained generation.
6. **GC preserves referenced blobs.** `deploy/test-r2-gc-blobs.py`. Fails if a blob referenced by a retained generation is deleted or an unreferenced blob survives a non-dry run.
7. **Authenticated native push works and large layers are safe.** `make test-oci-push` + `make test-oci-conformance-push`. Fails on an unauthenticated write, a manifest accepted with a missing blob, a 1 GiB layer failure, a lost concurrent push, or a non-pass conformance row.
8. **Numbers are recorded.** `make oci-bench` writes `docs/oci-benchmarks.md`. Fails if a matrix row is missing or the file lacks the command and environment.
9. **Partial failure is loud.** Any item whose acceptance does not run here is listed under **Needs environment** with the exact command; it is not marked accepted.

## Needs environment

The owner runs these; they never block local completion.

- Create buckets `stuff-oci` and `stuff-oci-blobs`; create custom domains `oci.17166969.xyz` and `oci-blobs.17166969.xyz`; add `oci.17166969.xyz` to the Worker domains.
- Set Worker secrets `STUFF_OCI_PUSH_TOKEN`, `STUFF_OCI_PUSH_SECRET`, `STUFF_OCI_WRITER_ORIGIN`, and (after A11) `STUFF_OCI_BLOB_ORIGIN`.
- Provision the writer host with TLS and `R2_S3_*` scoped to the two buckets; run `deploy/push-writer.sh`.
- `make deploy` after the schema 4 ordering rule (Worker and CLI before publishing OCI), then `make smoke-deployed`.
- Live read conformance: `OCI_REGISTRY=oci.17166969.xyz OCI_TLS=enabled deploy/oci-conformance.sh --tag <published-tag> --manifest <digest>... --blob <digest>... --referrer <subject>` against the published image; live push conformance with `--push`.
- Docker 29 on a clean machine, containerd 2.x, and BuildKit on Linux: pull and push the live image. ORAS live: `oras manifest fetch` and `oras blob fetch`.
- A 1 GiB push to the deployed writer, then a pull; and the A10 deployed matrix (cold/warm/cache/concurrency/full pull).
- Cloudflare Cache Rule for `oci-blobs.17166969.xyz/sha256/*` (Cache Everything, immutable TTL), Smart Tiered Cache, and the 512 MB cacheable-object cap observation.
- Live `make gc` / `deploy/r2-gc-blobs.py` dry run, then the real run.
- Push the integration branch and confirm CI green (the campaign never pushes).

## Execution boundary and non-goals

Authorized: local edits and tests; `.work/` scratch and worktrees; `CF_LOCAL_STATE` simulation; Miniflare; Docker/ORAS against 127.0.0.1; network reads for `oras`, the conformance build, and the sample image; CI edits in P1, P2, A8, and B4 only.

Forbidden: any push; any `cf` write, deploy, publish, rollback, or live GC; creating buckets, domains, secrets, or hosts; deleting anything outside `.work`; weakening or deleting tests; `/tmp` scratch; committing the conformance patch upstream.

Non-goals: private images and per-repo authorization; a token service; pull-through or proxy mode; Windows foreign/nondistributable layers; schema 1 and registry v1; `_catalog`; blob/manifest/tag delete; sparse indexes; non-sha256 digests; server-side lazy-pull formats; image conversion, decompression, or recompression on any path; a database or Durable Object in the read path; a Durable Object upload session; replacing npm/pnpm/uv orchestration; Docker Hub pull-through.

Contract changes to surface in the close report: catalog schema 4 and `registry:"oci"`; new CLI flags and `--oci-dir`; new hosts, buckets, Worker bindings, and secrets; `deploy/publish oci`; `stuff-oci` and `myoci` public APIs; the writer's HTTP surface and HMAC session token.
