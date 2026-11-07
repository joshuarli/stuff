# Publisher campaign

## Status

**Goal:** make one Rust executable, `stuff`, own the complete publication transaction for the cargo, npm, PyPI, and OCI registries. The live R2 generation is authoritative; publishing needs no stuff checkout, no distributed shell helper, and no Python runtime.

**Preflight:** ready. Baseline captured on 2026-11-06 (macOS arm64, 18 cores, load average 4.29). No slices are accepted; the campaign has not started.

**Red:** none in the baseline suites. The environment-gated proofs are listed under **Needs environment**.

**Next:** G1. Then G2, G3, and the R and C workstreams. N2-N4 wait for R4 and N1.

**Resume:** "Execute `docs/CAMPAIGN-PUBLISHER.md` from G1. Work serially except for the parallel pairs named in **Sequence**. Commit locally; do not push or write to a real registry. Stop at the hard stops in the charter."

First command:

```sh
cd ~/repos/stuff && git status --short && cargo test -p stuff
```

## Charter

- Decide and keep going. Record a decision in this document only when a later slice depends on it.
- The owner's grant is local: edit `joshuarli/stuff`, `laputa-systems/h12tiny`, `laputa-systems/smolith`, `joshuarli/tansu`, `joshuarli/pytest-parallel`, and `joshuarli/usr`; run their local tests and builds; create worktrees under `.work/wt/<lane>`; use `.work/tmp` for scratch. Do not modify canonical checkouts other than a clean fast-forward if the owner asks. No `/tmp` or `$TMPDIR` work.
- Hard stops, each parks the item: a push, a pull request, a release tag or workflow dispatch, a real R2/crates.io/npm/PyPI/OCI write, a credential or Cloudflare-permission change, deletion of user state, or a loop that cannot check anything. Put the proof on **Needs environment** with the command the owner would run.
- Fix an egregious problem met on the way only when the slice caused it: a broken test, a stale reference, or a malformed artifact the slice writes. Put it in its own commit. Record anything else and leave it. No general cleanup and no formatter runs.
- Parity before optimization. The Python implementations are the behavioral oracle until the close deletes them. A slice is accepted only when its parity command and the narrowest affected suite pass on the integrated tree.
- One writer by default. Parallel lanes are allowed only for the disjoint pairs named in **Sequence**; each lane works in its own worktree and the coordinator owns `crates/stuff/src/publish/mod.rs`, `crates/stuff/src/cli.rs`, `crates/stuff/Cargo.toml`, and `Makefile`.
- Every commit is a Conventional Commit with `publish` as the scope for stuff and the repository's normal scope elsewhere. Put the item ID in the body. Code comments never cite this document.
- Time-box each item at three attempts. On the third failure, park it with the failing command, the diff or output, and a retry condition. Never retry an unchanged failure.
- Run the full suite at M1, M2, M3, M4, and the close. The focused loop is `cargo test -p stuff`; the full parity loop is `make test-stuff-parity`. Report before running anything expected to exceed ten minutes.
- Performance floors: a focused Rust test target finishes under 60 seconds on the baseline host; `make test-stuff` under 120 seconds; `make test-stuff-parity` under 600 seconds. The existing Python R2 suite is 434.5 seconds, so run it at milestones, not per slice.
- Use `stuff-store::Site` and `stuff-store::FileLock` for generation staging, activation, and local serialization. Do not add a second site or lock implementation.
- After each accepted slice, run the parent steering checkpoint in the orchestrate campaign reference. Rewrite **Status** only when the ruling changes; git history is the log.

## Decisions

### Owner-decided (2026-11-06)

- **D1 — One executable.** `stuff` owns publishing for all four registries. `mynpm`, `mypypi`, and `myoci` become linked libraries; they are not installed as publishing tools. The native Cargo publication path moves into `stuff`. Reverses only if the owner splits the binary.
- **D2 — Live snapshot is authoritative.** A publish reads the live generation, manifest, and metadata; prepares the requested packages against it; validates the complete result; uploads missing objects; promotes conditionally; reads back and reports. Local data is an optimization. A competing publisher is never overwritten.
- **D3 — Credentials.** Use `R2_S3_*` directly. `cf` remains an optional credential provider. SigV4, uploads, multipart, and promotion are implemented in Rust. No AWS SDK.
- **D4 — CLI surface.** `stuff publish cargo|npm|pypi|oci`, `stuff publish ... --dry-run`, `stuff doctor`, `stuff rollback <registry> <generation>`. `republish` is a separate maintenance operation. Routine publishing has no `--allow-drops` and no unconditional promotion.
- **D5 — Release assets.** Publish raw `stuff-<target>` binaries for the existing three targets plus `SHA256SUMS`, under the existing `release-<sha>` pre-release tags. No zip. The installer verifies the checksum and sets the executable bit.
- **D6 — Skill.** Reduce `stuff-registries` in `joshuarli/usr` to 30-50 lines. `stuff publish --help` and `stuff/docs/PUBLISHING.md` are authoritative.
- **D7 — No-op is first class.** If the requested packages, dependencies, and generated bytes are already published, report that nothing changed, promote nothing, and exit 0.
- **D8 — Order.** R2 transport and transaction engine; native Cargo publishing; npm/PyPI/OCI integration; release and consumers; delete the wrappers and rewrite the skill. Deletion happens only after offline parity and one real publication exercise.
- **D9 — Ecosystem tools stay.** Cargo, npm, uv, and native wheel-repair tools remain runtime prerequisites for their ecosystems. Python-only archival and maintenance facilities stay in the repository until a later campaign migrates them.
- **D10 — Versioning stays in the source repository.** `stuff` never bumps a version, commits, or creates a tag. `h12tiny`'s release script keeps owning the bump, crates.io publication, and tags.

### Prescribed

- **P1 — Campaign document.** This file is the only deliverable of the planning commit. Execution edits code in later commits.
- **P2 — Module layout.** Implement in `crates/stuff/src/publish/` initially: shared `config`, `paths`, `transaction`, `state`; `r2/`; `cargo/`; `npm.rs`, `pypi.rs`, `oci.rs`, `rollback.rs`. Do not create new workspace crates until a module needs its own dependency boundary.
- **P3 — Dependencies.** Add `hmac` 0.12, `md-5` 0.10, and `quick-xml` 0.38 (or the newest mirror version compatible with the pinned rustc and `sha2` 0.10); reuse `sha2`, `deser-json`, `deser-toml`, `h12tiny-client-sync`, `httparse`, `stuff-store`, `stuff-catalog`, and `stuff-oci`. The stuff mirror already serves these crates. Implement SigV4 over `hmac` + `sha2`; do not add an AWS SDK. Update `NOTICE` and record the release-binary size delta. Reason: the smallest tested primitives; hand-rolled HMAC/MD5/XML is rejected for an untrusted remote parser. Reverses if a dependency cannot meet the release build.

- **P4 — State paths.** Defaults: config at `$XDG_CONFIG_HOME/stuff/config.json`; disposable cache at `$XDG_CACHE_HOME/stuff/<registry>`; durable admin state at `$XDG_STATE_HOME/stuff/<registry>`. Credentials come from the environment or `cf`; never store them. Tests may set `STUFF_CONFIG_HOME`, `STUFF_CACHE_HOME`, and `STUFF_STATE_HOME`, but the publish API never requires them. Preserve and back up the existing mycrates state before migration; pins and upstream provenance stay durable local admin state. Do not make them disposable and do not add a private R2 snapshot in this campaign. Reverses if the owner requires state portability.
- **P5 — Live snapshot construction.** Pull the current generation, its manifest, and the shared stores into the cache; verify size and ETag; reconstruct the cargo records from the published index (the adopt path) and run the existing Rust publish functions for npm, PyPI, and OCI against the pulled site. Never rebuild the published set from a local subset.
- **P6 — Consumers.** Migrate all four source repositories, not only `h12tiny`. `h12tiny` is the pilot; `smolith`, `tansu`, and `pytest-parallel` follow before the wrappers are deleted. Reverses only by keeping the old wrappers for an unmigrated repository, which blocks the close.
- **P7 — Oracle.** Keep the Python implementations and their tests as the parity oracle until the close. Byte-identical outputs are proven by `diff -r`/`cmp`, not by semantic comparison. Reuse the existing fake S3 server; extracting it into `deploy/fake_s3.py` is allowed.
- **P8 — Exit codes and output.** 0 success or no-op; 2 usage/config; 3 conflict/stale generation; 4 integrity/verification; 5 transport/network. Document them in `--help`. `--json` emits a stable object with `registry`, `generation`, `status` (`published|unchanged|conflict`), `packages`, `uploaded_objects`, `uploaded_bytes`, and `skipped_objects`.
- **P9 — No-op.** Compare the requested inputs and the complete generated generation with the live snapshot. If identical, do not upload and do not promote; report `unchanged`.
- **P10 — Conflict.** On promotion `412`, re-read the live snapshot, re-plan the additive change, and retry up to three times. A non-additive or byte-conflicting request exits 3 without a plain-put fallback. Partial uploads are harmless because the manifest is uploaded last.
- **P11 — Maintenance.** `stuff republish <registry>` is the renderer/generation refresh. `--allow-drops`, repair, and unconditional promotion appear only on explicit maintenance or rollback paths, never on `publish`.
- **P12 — Release and installer.** `release.yml` uploads raw `stuff-<target>` and `SHA256SUMS`; the installer for `stuff` downloads the raw asset, verifies the SHA-256 from `SHA256SUMS`, and installs it executable. Keep the `release-<sha>` tag and the three-platform matrix.
- **P13 — Deletion boundary.** At the close, delete only the publishing wrappers the Rust CLI supersedes: `deploy/publish`, `deploy/publish-mycrates.sh`, `deploy/publish-mynpm.sh`, `deploy/publish-mypypi.sh`, `deploy/r2-sync.sh`, `deploy/r2-put.py`, `deploy/r2-multipart.py`, `deploy/r2-promote.py`, `deploy/r2-plan.py`, `deploy/r2-manifest.py`, `deploy/r2-catalog-guard.py`, `deploy/r2-temp-credentials.py`, `deploy/r2-s3-credentials.sh`, `deploy/r2-rollback.sh`, and any now-unused lock wrapper, after proving no reference remains. Keep `r2-pull.py`, `r2-gc.py`, `r2-gc-blobs.py`, `import-cargo-locks.sh`, `fleet-ingest.sh`, `mypypi-container.sh`, `myoci-writer.sh`, `prune-generations.sh`, and the Python mycrates acquisition/archival commands; they are maintenance or the oracle.
- **P14 — Version.** Keep `stuff` at 0.1.0. The release tag identifies the build.
- **P15 — Tests are the gate.** No slice is accepted on a claim alone. The parity command named in its item must pass, and the narrowest affected suite must be green.

### Delegated

- **Dg1 — Internal names and file splits** inside `publish/`, within P2's ownership map. The executor may split a module into files, not crates.
- **Dg2 — Human output wording** and the exact JSON key order, within P8's fields. Keep keys stable and values deterministic.
- **Dg3 — Commit boundaries** inside a slice: one coherent commit per change, item ID in the body.
- **Dg4 — Fixture placement** under `crates/stuff/tests/` or reuse of `crates/mycrates/tests/golden/`; never change an existing golden expectation.
- **Dg5 — Log verbosity**: default concise, optional `--verbose`. No persistent logs.

## Baseline

### Known red

None in the baseline suites. The following proofs are environment-gated and are not red: `make test-r2-live`, the Linux release builds, OCI pull/push/conformance, mypypi Linux repair, and real network acquisition/publication.

### Loop timings (2026-11-06, macOS arm64, 18 cores, load 4.29)

| Command | Time | Result |
| --- | --- | --- |
| `make test-mycrates` | 12.2 s | 176 tests, pass |
| `cargo test --workspace` | 27.9 s | 357 tests, pass |
| `make test` (composed) | 40.1 s | test-mycrates + workspace |
| `make test-publish-lock` | 10.6 s | pass |
| `make test-r2` | 434.5 s | 53 Python tests + shell sync matrix, pass |
| `make test-oci-publish` | 71.0 s | pass |
| `cargo build --release -p stuff` | 9.6 s warm | `target/release/stuff` 4,423,440 B |
| `cargo check -p mynpm -p mypypi -p myoci` | 4.8 s | pass |
| release assets `release-b1ee734` | - | zips 1,625,862 / 1,757,974 / 1,837,350 B; `SHA256SUMS` 302 B |

The Python R2 suite is the slow loop. The Rust parity target must stay under 600 seconds; per-slice work uses `cargo test -p stuff`.

### Code and artifact surface

- `deploy/`: 3,476 lines of runtime shell/Python and 3,570 lines of tests. The publishing wrappers account for 2,257 runtime lines.
- `crates/mycrates/`: 4,233 Python lines, of which 3,061 are the publish/adopt/store/server/render path; 176 tests and the golden corpus.
- Registry libraries: 25,420 Rust lines (`mynpm` 12,450, `mypypi` 9,037, `myoci` 3,933); all are `[lib]` crates.
- `crates/stuff/`: 2,590 Rust lines; existing commands are read-only.
- Golden corpus: `crates/mycrates/tests/golden/publish/` has 10 fixtures and `adopt-publish/` has the input site, expected generation, and expected generation id.
- Current release binary: 4.4 MB, three targets, zipped assets.

Current caller surface: registry state lives under `$STUFF_STATE_HOME` (`promoted-generation`, `.adopt-complete`, `publish.lock`); source Makefiles set `STUFF_DIR`; the publish helpers need `cf`, Python 3.11+, Rust, node/npm, uv, and optionally repair tools. The new API removes `STUFF_DIR` and `STUFF_STATE_HOME` from the caller; cargo/npm/uv remain prerequisites for their ecosystems, and Python remains only for maintenance and the oracle.

### Foundation proof

- **Linkable libraries (rung 3-4).** `cargo check -p mynpm -p mypypi -p myoci` passes in 4.8 s; all three are library crates. `cargo tree -i stuff` shows nothing depends on `stuff`, so linking them into it is acyclic. `mynpm::publish_site` is public at `crates/mynpm/src/publish.rs:986` and called by `crates/mynpm/src/cli.rs:716`; `myoci::publish` is public at `crates/myoci/src/publish.rs:38`. `mypypi` has no public publish entry point; `command_publish` is CLI-bound at `crates/mypypi/src/cli.rs:1168`, so N1 is required, not assumed.
- **Live snapshot suffices for cargo (rung 4).** `make test-mycrates` passes 176 tests, including the `adopt-publish` golden. `docs/mycrates-contracts.md` documents that adopt rebuilds native/upstream records from the published site and that pins and upstream provenance are local-only. P4 preserves the local admin state.
- **Conditional promotion (rung 4).** `python3 deploy/test-r2-promote.py` passes in the R2 suite; `docs/PUBLISHING.md` documents `If-Match`/`If-None-Match:*` and 412. The Rust port's parity is R4.
- **Site activation and no-op (rung 3-4).** `crates/stuff-store/src/site.rs:200` publishes through staging and activation; mynpm, mypypi, and myoci already use it, and the 357-test workspace suite passes. The cargo port reuses it.

Unproven until the named slice: the Rust S3 client against the fake (G1/R1), the cargo port (C1-C3), and the raw-asset installer path (L2).

## Sequence

Every item is a slice with one local acceptance command. Sizes: S is hours, M is days.

### G1 — Shared fake-S3 fixture and Rust harness (S)

- **Owned:** `deploy/fake_s3.py` (new, extracted from `deploy/test-r2-put.py:42` and `deploy/test-r2-promote.py:25`), both Python tests, `crates/stuff/tests/support/mod.rs` (new), `Makefile` (`test-stuff-s3`, `test-stuff-parity`).
- **Prerequisites:** none.
- **Acceptance:** `cargo test -p stuff --test fake_s3` passes; `python3 deploy/test-r2-put.py` and `python3 deploy/test-r2-promote.py` pass; `make test-stuff-s3` finishes under 60 s. `test-stuff-parity` is the aggregate Rust publish suite; it starts as `cargo test -p stuff` and grows with each R/C slice, staying under 600 s.
- **Notes:** the fake supports PUT, GET, HEAD, paginated list, and conditional PUT. Extract, do not fork behavior; the Python tests stay green.

### G2 — CLI, config, paths, exit codes (M)

- **Owned:** `crates/stuff/src/cli.rs`, `main.rs`, `error.rs`, `publish/mod.rs`, `publish/config.rs`, `publish/paths.rs`, parser tests, `Makefile` (`test-stuff`).
- **Prerequisites:** G1.
- **Acceptance:** `cargo test -p stuff cli paths` passes; `target/debug/stuff publish --help` shows the four grammars, `--dry-run`, and `--json`; `stuff publish cargo` with no package exits 2; `cargo test --workspace` passes.
- **Notes:** keep the existing `ls`, `search`, `diff`, `doctor`, and `render` commands and flags unchanged.

### G3 — Dependency admission and license record (S)

- **Owned:** `crates/stuff/Cargo.toml`, `Cargo.lock`, `NOTICE`, dependency tests.
- **Prerequisites:** G2.
- **Acceptance:** `cargo build --release -p stuff` passes; the binary size and build time are recorded in **Status**; `cargo test -p stuff` passes; `NOTICE` names every new crate and its license; `cargo tree -p stuff` contains no AWS SDK and no `serde` direct dependency.
- **Notes:** pin the exact versions in the mirror; if a version needs a newer rustc than the pinned toolchain, stop and report rather than updating the toolchain.

### R1 — SigV4 and S3 client (M)

- **Owned:** `publish/r2/mod.rs`, `sigv4.rs`, `client.rs`, `xml.rs`, tests.
- **Prerequisites:** G3.
- **Acceptance:** `cargo test -p stuff publish::r2` passes; SigV4 output matches the Python `deploy/r2-multipart.py:97` oracle and the published AWS vectors; the fake-S3 test exercises PUT, GET, HEAD, paginated list, and error mapping; `make test-stuff-r2-client` finishes under 60 s.

### R2 — Uploads and manifest (M)

- **Owned:** `publish/r2/put.rs`, `multipart.rs`, `manifest.rs`, tests.
- **Prerequisites:** R1.
- **Acceptance:** Rust parity with `python3 deploy/test-r2-put.py` (8 tests) and `python3 deploy/test-r2-multipart.py` (10 tests), including 429/`Retry-After`, content types, skip-if-equal, byte conflict, and multipart ETags; `make test-stuff-r2-put` finishes under 120 s.

### R3 — Pull and live snapshot (M)

- **Owned:** `publish/r2/pull.rs`, `publish/snapshot.rs`, tests.
- **Prerequisites:** R1.
- **Acceptance:** Rust pull of a fixture generation matches `python3 deploy/test-r2-pull.py` (6 tests) and `python3 deploy/test-r2-pull-listing.py` (4 tests) byte for byte, including multipart ETag verification and shared-store paths; `make test-stuff-r2-pull` passes.

### R4 — Transaction: sync, guard, promote, rollback, dry-run, lock (M)

- **Owned:** `publish/r2/sync.rs`, `publish/r2/promote.rs`, `publish/r2/guard.rs`, `publish/transaction.rs`, tests.
- **Prerequisites:** R2, R3.
- **Acceptance:** parity with the scenarios in `deploy/test-r2-sync.sh` (120 objects) and `python3 deploy/test-r2-promote.py`; tests for `If-Match`, `If-None-Match:*`, 412, partial upload, interrupted promotion, stale recorded generation, dry-run, catalog guard, and rollback; `make test-stuff-r2-sync` finishes under 180 s.
- **Notes:** hold the local `stuff-store::FileLock` across a publish; the remote conditional write is the cross-machine fence.

### R5 — Credentials (S)

- **Owned:** `publish/r2/credentials.rs`, tests.
- **Prerequisites:** R1.
- **Acceptance:** parity with `python3 deploy/test-r2-temp-credentials.py`; direct `R2_S3_*`, `cf` minting, missing-login, and expired-token cases; `make test-stuff-r2-credentials` passes.
- **Parallel:** may run alongside R2-R4; it owns `credentials.rs` and its tests only.

### C1 — Cargo core: model, index, archive gate, adopt (M)

- **Owned:** `publish/cargo/mod.rs`, `model.rs`, `index.rs`, `crate_file.rs`, `adopt.rs`, `store.rs`, tests.
- **Prerequisites:** G3. May run parallel with R1-R3.
- **Acceptance:** byte-identical index lines and accept/reject decisions for all 10 `crates/mycrates/tests/golden/publish/` fixtures; the contract rules in `docs/mycrates-contracts.md`; `make test-stuff-cargo-core` finishes under 120 s.
- **Notes:** normalizations are exact: requirement intervals, renamed dependencies, feature-set comparison, key order, escaping, shard paths, and SemVer ordering.

### C2 — Cargo render and generation (M)

- **Owned:** `publish/cargo/render.rs`, `publish/cargo/publish.rs`, tests.
- **Prerequisites:** C1.
- **Acceptance:** rendering `crates/mycrates/tests/golden/adopt-publish/input-site` produces `expected-generation` byte for byte and `expected-generation-id`; `make test-stuff-cargo-render` passes.

### C3 — Cargo loopback server and end-to-end publish (M)

- **Owned:** `publish/cargo/server.rs`, `publish/cargo/wire.rs`, `publish/cargo/run.rs`, `crates/stuff/tests/cargo_publish_e2e.rs`, a fixture crate.
- **Prerequisites:** C2, R4.
- **Acceptance:** a real `cargo publish` against the in-process server completes through `stuff publish cargo`; the resulting generation bytes and id equal the Python path on the same inputs; the second run reports `unchanged` and promotes nothing; `make test-stuff-cargo-e2e` finishes under 180 s.
- **Notes:** bind an ephemeral loopback port, generate the Cargo registry config in the child environment, never mutate the persistent public URL, and stop the listener on every exit path.

### C4 — Cargo live state, cache, pins, provenance (M)

- **Owned:** `publish/cargo/snapshot.rs`, `publish/state.rs`, migration notes, tests.
- **Prerequisites:** R3, C3.
- **Acceptance:** with an empty cache and no old state, a publish against a pulled fixture produces the same generation as the full-state path; an existing mycrates state is backed up and its pins/provenance survive; repeated publish is a no-op; `make test-stuff-cargo-state` passes.

### C5 — Cargo CLI and output (S)

- **Owned:** `crates/stuff/src/cli.rs`, `publish/cargo/`, tests.
- **Prerequisites:** C3.
- **Acceptance:** `stuff publish cargo -p NAME... --manifest-path PATH --registry NAME --dry-run --json` parses and reports `published|unchanged|conflict`; exit codes match P8; `cargo test -p stuff publish::cargo::cli` passes.

### N1 — Extract the mypypi publish library (M)

- **Owned:** `crates/mypypi/src/publish.rs` (new), `crates/mypypi/src/cli.rs`, tests.
- **Prerequisites:** none. May run parallel with R1-R5.
- **Acceptance:** `cargo test -p mypypi` passes; the existing `mypypi publish` and `republish` CLI cases behave identically; the new public function takes the same inputs as the CLI.

### N2 — `stuff publish npm` (M)

- **Owned:** `publish/npm.rs`, `publish/inventory.rs`, tests.
- **Prerequisites:** R4 and linked `mynpm`; N1 is not required.
- **Acceptance:** a fixture pnpm lockfile publishes through `stuff publish npm` and produces the same generation as `mynpm publish` on the same pulled site/state; no `mynpm` subprocess; `make test-stuff-npm` passes.

### N3 — `stuff publish pypi` (M)

- **Owned:** `publish/pypi.rs`, tests.
- **Prerequisites:** R4, N1.
- **Acceptance:** fixture packages and wheels publish through `stuff publish pypi` and produce the same index/generation as the current `mypypi publish`; `make test-stuff-pypi` passes. Native repair and Linux build proof is in **Needs environment**.

### N4 — `stuff publish oci` (M)

- **Owned:** `publish/oci.rs`, tests.
- **Prerequisites:** R4.
- **Acceptance:** `tests/fixtures/oci/layout` publishes through `stuff publish oci` and produces the same generation as `myoci publish`; `make test-oci-publish` stays green; `make test-stuff-oci` passes.

### N5 — Rollback and doctor (S)

- **Owned:** `publish/rollback.rs`, `doctor.rs`, CLI wiring, tests.
- **Prerequisites:** R4, N2-N4.
- **Acceptance:** `stuff rollback <registry> <generation>` verifies the manifest and promotes conditionally for all four registries; `stuff doctor` keeps the current read-only checks and adds the live generation check; `make test-stuff-rollback` passes.

### L1 — Raw release assets (S)

- **Owned:** `.github/workflows/release.yml`.
- **Prerequisites:** C5, N5.
- **Acceptance:** a local macOS `cargo build --release -p stuff` produces a raw binary and `shasum -a 256` matches a generated `SHA256SUMS`; the workflow uploads `stuff-<target>` and `SHA256SUMS` with no zip; the build job runs `cargo test -p stuff` and the three registry crates. Linux targets are proved by CI under **Needs environment**.

### L2 — Installer raw asset (S)

- **Owned:** `~/usr/tools/sync/install.sh`, `~/usr/dotfiles/sync-tools-test.sh`.
- **Prerequisites:** L1.
- **Acceptance:** `sh ~/usr/dotfiles/sync-tools-test.sh` passes; a new case downloads raw `stuff-<target>`, verifies the SHA-256 from `SHA256SUMS`, installs it executable, and rejects a bad checksum. No zip path remains for `stuff`.

### L3 — Consumer Makefiles (M)

- **Owned:** `Makefile` in `h12tiny`, `smolith`, `tansu`, and `pytest-parallel`.
- **Prerequisites:** L1, L2, C5, N2-N4.
- **Acceptance:** from each repository, with no stuff checkout at the sibling path, `make -n publish-mycrates|publish-mynpm|publish-mypypi` resolves `stuff publish ...`; `stuff publish ... --dry-run` against the fake/local fixture succeeds; `rg STUFF_DIR` returns no matches in the four repositories. A real publish is owner-gated. `h12tiny` commits the pilot first.

### L4 — usr skill (S)

- **Owned:** `~/usr/dotfiles/base/.agents/skills/stuff-registries/SKILL.md`.
- **Prerequisites:** L3.
- **Acceptance:** `sh ~/usr/dotfiles/skills-check.sh` passes; the file is 30-50 lines; it points to `stuff publish --help` and `stuff/docs/PUBLISHING.md` instead of restating procedures.

### L5 — Close (M)

- **Owned:** the P13 deletion list, `Makefile`, `docs/PUBLISHING.md`, crate READMEs, tests, **Status**.
- **Prerequisites:** L1-L4 and the owner's real-pilot receipt.
- **Acceptance:** `rg` finds no reference to a deleted wrapper; `make test`, `cargo test --workspace`, `make test-stuff-parity`, `make test-oci-publish`, and `make test-publish-lock` pass; `mdtool check --untracked docs/CAMPAIGN-PUBLISHER.md` is clean apart from documented out-of-repo paths; `ultrareview` P1/P2 findings are fixed; worktrees and scratch are removed; the final report is written.
- **Hard stop:** if the owner has not reported the real publication exercise, delete nothing and park the item with the receipt still open.

### Milestones and parallelism

- **M1 (after R4):** `make test-r2` plus `cargo test --workspace`. This is the R2 parity milestone.
- **M2 (after C3):** `make test-mycrates`, the golden render diff, and `cargo test --workspace`.
- **M3 (after N5):** `make test-oci-publish`, `cargo test -p mynpm -p mypypi -p myoci`, and `make test`.
- **M4 (after L3):** `make test`, the macOS raw build, the installer test, and the four consumer dry-runs.
- **Parallel pairs:** after G3, C1 may run with R1-R3 (files under `publish/cargo/` versus `publish/r2/`), and N1 may run with any of them. R5 may run with R2-R4. N2, N3, and N4 may run in parallel after N1 and R4 because each owns one `publish/<registry>.rs` and its tests; the coordinator owns `publish/mod.rs` and the CLI wiring. All other items are serial.

## Acceptance

| ID | Deliverable | Proof (success) | Failure condition |
| --- | --- | --- | --- |
| A1 | Rust R2 engine | The shared fake-S3 parity runner produces the same object set, bytes, sizes, and ETags, and the same exit codes as the Python oracle for put, multipart, pull, sync, guard, promote, rollback, dry-run, 412, partial upload, and stale state. | Any byte or object-set difference; a plain-put fallback; a promotion after a failed verification. |
| A2 | Cargo publisher | All 10 golden publish fixtures and the adopt-publish generation match byte for byte; a real `cargo publish` through `stuff` matches the Python path; the second publish is a no-op. | Any index-line, generation-id, catalog, or archive-gate difference; a promotion on the no-op. |
| A3 | npm, PyPI, OCI | Each `stuff publish` produces the same generation as the current library/CLI on the same fixture inputs; the existing crate and OCI suites stay green. | A different generation, a subprocess to `mynpm`/`mypypi`/`myoci`, or a broken existing suite. |
| A4 | Live state and concurrency | Fresh cache with no old state publishes from the live fixture; repeated publish is a no-op; interrupted upload resumes; a concurrent 412 is retried additively and never overwritten; stale state is refused; pins/provenance survive the backup. | A local subset replaces the live set; a 412 overwrites; a no-op promotes; provenance is lost. |
| A5 | Release and installer | Raw `stuff-<target>` plus `SHA256SUMS`; the installer verifies the checksum and installs the binary; no zip for stuff. | A zip-only asset, an unverified binary, or a failed installer test. |
| A6 | Consumers and skill | All four Makefiles invoke `stuff publish ...` with no sibling checkout; the skill passes its check at 30-50 lines. | A remaining `STUFF_DIR` path, a wrapper invocation, or a skill longer than 50 lines. |
| A7 | Close | The P13 wrappers are deleted with no references; all suites pass; ultrareview P1/P2 fixed; no worktrees or scratch. | A dead reference, a red suite, or an unresolved P1/P2. |

## Needs environment

- **Real R2 read/dry-run:** `stuff publish cargo -p <already-published-crate> --dry-run` with `R2_S3_*` or a minted `cf` credential; expected `unchanged`, no write. Owner-run because it reaches the live bucket.
- **Real publication exercise:** one owner-authorized additive publish (for example a new patch version of a test crate) through `stuff publish cargo`, then `stuff doctor --remote`; record the generation and the read-back. This is the receipt L5 waits on.
- **Linux release builds:** the `aarch64-unknown-linux-musl` and `x86_64-unknown-linux-musl` jobs in `release.yml`, run in CI. Local macOS proves the Apple target.
- **Live multipart:** `make test-r2-live` with `R2_ACCEPTANCE_BUCKET` and `R2_S3_*`.
- **OCI:** `make test-oci-pull`, `make test-oci-push`, `make test-oci-conformance`, and `make oci-bench` with Docker and `oras`.
- **PyPI native repair:** `deploy/mypypi-container.sh` on a Linux host with the repair tooling.
- **Real ecosystem acquisition:** a network publish/fetch through crates.io, npm, and PyPI; the local fixtures prove generation, not remote acquisition.
- **Release dispatch:** the owner dispatches `release.yml`; the campaign edits the workflow but never runs or tags it.

## Execution boundary and non-goals

**Authorized:** edit the six repositories' source, tests, docs, workflows, and Makefiles; run local tests, builds, and the fake S3; create disposable worktrees under `.work/wt/` and scratch under `.work/tmp/`; commit locally.

**Forbidden:** pushing, pull requests, release tags or dispatch, any real registry write, `make deploy`, credential or Cloudflare-permission changes, deleting user state or canonical checkouts, modifying `catalog.toml`, adding a database or background worker, porting the mycrates acquisition/archival commands in this campaign, and running heavy Linux/Docker builds on the macOS host.

**Contract changes to carry through tests and docs:** the `stuff` CLI surface and exit codes; the state/cache/config paths; raw release assets and installer behavior; the four consumer Makefiles; the deleted wrappers; the usr skill. Each is listed in the commit body that lands it.

**Non-goals:** a universal package model; changing the Worker, registry wire formats, or catalog renderer; porting `mycrates import-lock`/`fetch`/`verify`/`adopt`/`migrate`/GC; migrating the Python maintenance tests; publishing a `stuff` release; version bumps.

## Reference

| Path | Purpose |
| --- | --- |
| `crates/stuff/src/cli.rs:37` | Existing command enum and flag parser; G2 extends it. |
| `crates/stuff/src/main.rs:33` | Existing dispatch. |
| `crates/stuff/Cargo.toml:19` | Current dependencies; G3 adds the crypto/XML/archive crates. |
| `crates/mynpm/src/publish.rs:986` | `publish_site`; library entry for N2. |
| `crates/mynpm/src/cli.rs:716` | CLI publish handler; the shape N2 replaces. |
| `crates/mypypi/src/cli.rs:1168` | CLI-only `command_publish`; N1 extracts it. |
| `crates/myoci/src/publish.rs:38` | `publish`; library entry for N4. |
| `crates/stuff-store/src/site.rs:200` | Generation staging and activation; reuse for every registry. |
| `crates/mycrates/AGENTS.md` | mycrates module map and format rules. |
| `crates/mycrates/mycrates/publish.py:95` | Publisher and generation-id oracle. |
| `crates/mycrates/mycrates/adopt.py:25` | Adopt-from-site oracle for the live snapshot. |
| `crates/mycrates/mycrates/server.py` | Loopback publish protocol oracle. |
| `crates/mycrates/mycrates/crate_file.py` | Archive gate oracle. |
| `crates/mycrates/tests/golden/publish/` | 10 byte-exact publish fixtures. |
| `crates/mycrates/tests/golden/adopt-publish/` | Input site, expected generation, and generation id. |
| `docs/mycrates-contracts.md` | State, index, upload API, site layout, and generation identity contracts. |
| `docs/PUBLISHING.md` | Current operator procedures; rewrite at the close. |
| `deploy/publish:1` | Current wrapper; deleted at the close. |
| `deploy/publish-mycrates.sh:1` | Current Cargo transaction; C3 replaces it. |
| `deploy/r2-sync.sh:1` | Current sync/promotion transaction; R4 replaces it. |
| `deploy/r2-promote.py:1` | Conditional-promotion oracle; R4 replaces it. |
| `deploy/r2-put.py:1`, `deploy/r2-multipart.py:1` | Upload oracle; R1-R2 replace them. |
| `deploy/r2-pull.py:1` | Pull/adopt oracle; R3 replaces the publish path but keeps the maintenance command. |
| `deploy/test-r2-put.py:42`, `deploy/test-r2-promote.py:25` | Fake S3 servers reused by G1. |
| `.github/workflows/release.yml:76` | Release build; L1 changes the asset shape. |
| `~/usr/tools/sync/install.sh:671` | Release installer; L2 adds the raw `stuff` path. |
| `~/usr/dotfiles/base/.agents/skills/stuff-registries/SKILL.md` | Skill rewritten by L4. |
