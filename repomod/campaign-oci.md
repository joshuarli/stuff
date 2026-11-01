# Musl toolchain campaign

## Status

**Goal:** make every musl Linux build input controlled by one approved lock, one refresh path, and one enforcing rule, then consolidate the conventional Rust builders onto a shared OCI image.

**Preflight:** ready. Owner decisions D1-D4 were answered on 2026-11-01. Execution started and closed on 2026-11-01.

- **Accepted slices:** M1 — `musl-toolchain.toml` and `repomod pins refresh [--check] [--only]` (b53864d). Seeded from live sources on 2026-11-01: upstream musl 1.2.6 (2026-03-20), stable Alpine 3.24.2 with musl 1.2.6-r3, edge with musl 1.2.6-r6. `pins refresh --check --only musl` exits 0 and reports `alpine:3.24` at 3.24.3 as an unapproved observation. M2 — `linux-musl-toolchain` replaces `rust-musl-image` and enforces the lock with a deterministic fixer (8e5e209). M3 — canonical `docker/musl/Dockerfile`, its `rust-toolchain.toml`, and `repomod musl build` (cd1f16f). A local `linux/amd64` build completed within the time box; its OCI layout digest was `sha256:f3d9149eb4f54d7e1b49fddaf8d81ddede0545ee9cb41bae34209bf6fb1d1bc6`, and the smoke test printed clang 23.1.0-rc2 and mold 3.0.0 and found `/usr/local/bin/musl-cargo`. The local image and OCI output were removed after the proof. M4 — shared `static_elf.py`, `docker/musl/verify-static-elf`, and workflow enforcement with the image-mode trace (c04ade5). On `linux/amd64`, the static artifact passed, the dynamic artifact failed on `PT_INTERP`, the good trace passed, and the rustup self-contained trace failed. The local image was removed after the proof.
- **Close checks:** `make test` is 595 passed and 1 skipped; `ruff` and `ty` pass. The fleet dry-run is 118 error findings across 15 repositories, 0 operational, 105 fixable, in 0.303 s. `mdtool check --untracked campaign-oci.md` exits 0 with 6 R001 warnings for paths that belong to other repositories. `ultrareview` is not installed, so the close used the repository checks and a manual `ca099ce..HEAD` diff review. The closeout lifecycle ran `codex-campaign-closeout.py begin`, pruned stale worktree registrations, and cleared the marker; the campaign left no worktrees, containers, images, or scratch behind.
- **Rollout (post-close):** `repomod fix --rule linux-musl-toolchain` opened 11 PRs; 9 merged automatically or by admin after their real checks passed (`ish`, `diff-pretty`, `e`, `fastermod`, `fdu`, `git-build`, `rustybench`, `srv`, `vault`). Three follow-up PRs were opened for the remaining fixable repos; `node-build` and `zzz` merged, and `tansu` is open. `fz` and `tansu` have pending reruns, and `tm` is blocked by the known `missing tmux 3.8 reference` failure. `clang23` was added to the lock and `node-build` now uses it. The local fleet after the rollout is 49 findings in 6 repositories, 0 operational, 42 fixable; the merged remote state is lower. `zzz` is merged remotely but its canonical checkout is still on `all-green/zzz`, so the local fleet lint still reports its 3 findings until it returns to `master`.
- **Environment note:** `pins refresh --check --only musl` exits 0 again after the Docker Hub rate limit cleared. Manual findings remain on `chromium-build` (4 adapter-lock items), `fdu` (rustup 1.29.0), and `tansu` (the unverified rustup installer); these are not deterministic fixer edits.
- **Fleet dry-run (M2, unchanged at close):** 118 error findings across 15 repositories, 0 operational, 105 fixable. Counts: `ish` 10, `tansu` 25, `chromium-build` 4, `diff-pretty` 9, `e` 9, `fastermod` 6, `fdu` 2, `fz` 10, `git-build` 1, `node-build` 6, `rustybench` 9, `srv` 6, `tm` 6, `vault` 12, `zzz` 3. `fdu` reports the stale `python3` and rustup 1.29.0; `tansu` reports both musl Dockerfiles. `node-build`'s `clang` token has no version in the live Alpine index, so its package findings stay manual instead of inventing one. `python-build` is clean, and skipped or builder-free repositories return no findings.
- **Red:** the campaign's own checks are green. The local fleet still reports 49 findings; the merged remote state is lower. The remainder is the open fz/tansu/tm PRs and the manual adapter and rustup findings, not the original 118-item migration backlog.
- **Next:** closed. Remaining work is the fz/tansu reruns, the tm CI fix, the manual adapter and rustup findings, image publication, and consumer migration.
- **Resume:** "Campaign closed. Execute the **Needs environment** items with the owner's authority."

## Charter

- Decide and keep going. Record a decision only when a later slice depends on it.
- The owner's grant is local: edit this repository, run its tests and linters, run local Docker builds under `.work/`, run `uv run repomod sync` as the repository instructions require, and read canonical checkouts. Sync may fetch and fast-forward a clean checkout; that is the only write the campaign makes there. The grant does not include remote writes. Pushing, opening pull requests, publishing images, `topics apply`, `repomod push`, and `repomod fix` are hard stops; park the item on **Needs environment**. At this revision `fix` pushes every branch it creates and has no `--push` flag, so the campaign never runs it.
- Hard stops are few and each parks one item: a remote write, a destructive change to a canonical checkout, a build that requires a Linux host when the current host is macOS, a rule or test that cannot check anything, or a dependency/authority decision the owner has not granted.
- Fix an egregious problem met on the way only when it is caused by the current slice: a test broken by the change, a stale reference to a rule or command this campaign changes, or a malformed lock/Dockerfile that the slice writes. Put it in its own commit. Record anything else and leave it. No general cleanup, no formatter runs.
- Parity before optimization. M1-M2 must make the lock, refresh, and rule catch the baseline findings before M3-M4 build or consolidate anything.
- One writer. The slices share `src/repomod/cli.py`, `src/repomod/rules/musl_toolchain.py`, `musl-toolchain.toml`, and the policy docs, so do not run parallel lanes. Serial delivery is expected to finish faster than integration. Work on `master`; commit each accepted slice there. A parked item goes on `campaign/musl-<id>` with its cause and retry condition in **Status**. Do not create a long-lived campaign branch unless the owner asks.
- Every commit is a Conventional Commit with `musl` as the scope; put the item ID in the body. Code comments never cite this document.
- Time-box each item at three attempts. On the third failure, park the item on its branch with the cause and a retry condition; do not retry an unchanged failure. A shared environment blocker gets one owner: the owner.
- Each slice ends with its acceptance command and the narrowest affected test file. Run the full suite at M3 and at M5. The baseline timings are in **Baseline**; report before running anything expected to exceed ten minutes.
- Performance floor: a fleet-wide `uv run repomod lint --rule linux-musl-toolchain --json` over the 34 catalogued repositories must finish under 5 seconds on this host, and a slice's focused test file must finish under 60 seconds. If either exceeds the bound, profile and fix it before accepting the slice. The old rule's fleet run is 0.13 s and the full suite is 12.7 s; the Docker build is separately time-boxed at 10 minutes.
- Use `.work/` for scratch, OCI output, and worktrees; never `/tmp` or `$TMPDIR`. Set `TMPDIR` to `.work/tmp` for commands that spill files. Remove containers, images, and worktrees the campaign creates before closing the slice.
- After each accepted slice, run the parent steering checkpoint in the `orchestrate` campaign reference. Rewrite **Status** only when the ruling changes; git history is the log.

## Decisions

### Owner-decided (2026-11-01)

- **D1 — Architecture: local-first, then image.** Land the lock, refresh, rule, and exact per-repo pinning first. Add the canonical OCI builder source and a local build next. Registry publication and consumer migration are owner-run environment steps. Reverses if the owner later requires the shared image before any enforcement.
- **D2 — Rollout: the owner applies fixes.** The campaign lands the policy, fixer, tests, and a dry-run fleet report. The owner runs `repomod fix` and merges the pull requests. The campaign never pushes. Reverses only with an explicit remote-write grant.
- **D3 — Scope: all musl builders, heavy builds off macOS.** Cover the conventional Rust builders, tansu, and the bespoke Python, Chromium, Node, Git, and Zig builders. Do not run their heavy builds on this macOS host; put those proofs on **Needs environment** and keep the local work to source, lock, workflow, and static checks. Reverses if a smaller first campaign is preferred.
- **D4 — Verification: verifier plus enforcement.** Add the shared static-ELF verifier and require release workflows to call it. Actual artifact execution on both architectures is environment-gated. Reverses if the owner prefers workflow-only checks.

### Prescribed

- **D5 — Rule ID: `linux-musl-toolchain` replaces `rust-musl-image`.** The new rule owns the whole builder contract, including non-Rust musl builders. No catalog override references `rust-musl-image`, so removing it does not break a configured ignore. Update the docs and tests in the same change. Reverses if an external consumer is found using the old ID.
- **D6 — Lock: `musl-toolchain.toml` at the repository root.** It records the upstream musl release, the `stable` and `edge` Alpine channels with index and per-architecture manifest digests, exact direct package versions per channel and architecture, the LLVM/mold/rustup archive pins, the adapter list, and an optional `[image]` reference/digest for the published canonical builder. The exact schema is in M1.
- **D7 — Refresh: `repomod pins refresh [--check] [--only DOMAIN]`.** The command refreshes `actions.toml`, `python-tools.toml`, `typescript-tools.toml`, and `musl-toolchain.toml`. `--only actions|python|typescript|musl` restricts the run so a musl check is not blocked by unrelated pin drift. `--check` fetches and reports drift without writing; it exits 1 when a file would change. There is no scheduler, background worker, or database; a daily check is an owner cron command. A malformed source or failed fetch writes nothing and fails loudly.
- **D8 — Scope detection: content and workflow references, not a new catalog tag.** `linux-musl-toolchain` runs for every repository and returns no findings when no musl builder exists. It skips `fork`, `private-tool`, and `macos` repositories. It scans regular `Dockerfile`, `Dockerfile.*`, `*.Dockerfile`, and `docker/*.Dockerfile` files outside ignored build/vendor trees, plus Dockerfiles named by release or CI workflows. Adapters cover the bespoke builders listed in D3. Reverses if false positives require an explicit catalog opt-in.
- **D9 — Pinning depth: digest, direct packages, and archive checksums.** Require an immutable image digest, exact direct `apk` versions, and checksummed toolchain archives. The full APK closure and offline `apk` installation are not required for the first pass; the bespoke `python-build` and `chromium-build` locks keep their existing closure machinery, and the canonical image may adopt closure later. Reverses if a release drifts because an unpinned transitive package changed.
- **D10 — Digest-pinned `edge` and `latest` tags are allowed.** The digest is the control; the tag is documentation. A floating `alpine:edge` or `alpine:latest` without a digest is an error. A digest that does not match the approved channel is an error.
- **D11 — Fixer scope: deterministic Dockerfile edits only.** The fixer may rewrite the Alpine base digest and exact package versions in conventional Rust builders and tansu. It does not rewrite generated locks, build systems, or workflows for bespoke builders; those are manual findings with the adapter command. A finding is fixable only when the lock has an approved version for every package in the file. The fixer must leave no fixable finding and introduce no new policy error; the existing fix runner enforces both.
- **D12 — Canonical image: source and local build, no publish.** Add `docker/musl/Dockerfile` and `repomod musl build`, which reads the lock, passes build arguments, and writes a local OCI layout under `.work/`. The command has no push path in this campaign. Only a published digest is recorded in `[image]`; a local digest is not consumable and must not be recorded as approved.
- **D13 — Verifier: one source, two consumers.** The ELF checks are defined once in `src/repomod/rules/static_elf.py`. The rule's fixer embeds the workflow step; the canonical image installs the same script. Image-mode builders also pass a linker trace that names the builder's `libc.a` and CRT paths; pinned-mode builders keep the ELF checks until they migrate. Actual runs on release artifacts are environment-gated.
- **D14 — Fleet severity: errors, not warnings.** The rule lands at error severity. The fleet will be red until the owner runs the fixer and merges the pull requests; that red is the migration backlog, not a reason to add catalog ignores. The campaign's own tests stay green. Reverses if the owner wants a warning phase before enforcement.
- **D15 — Do not change `fix` semantics.** The campaign records the AGENTS/docs mismatch (`fix --push` versus the current unconditional push) and leaves the command alone. The owner runs it with remote-write authority. Reverses only if the owner asks for a local-only fix mode.
- **D16 — No automatic release, scheduler, or rollout tracker.** Changing the builder does not dispatch a release. Daily refresh is an external schedule. There is no new database, worker, queue, or per-fixer clone directory.
- **D17 — Heavy proof stays off macOS.** The canonical image's local proof is a single-architecture build under a ten-minute time box. Building or qualifying `python-build`, `chromium-build`, `node-build`, `git-build`, or `zzz`, and building the multi-architecture image for publication, belong on a Linux host. If a local build exceeds the time box, stop and park the proof on **Needs environment**.
- **D18 — Version ownership.** The musl and Alpine versions refresh automatically. LLVM, mold, and rustup version bumps are manual, checksum-verified changes, because their release selection is not part of the audit's refresh scope. The Rust nightly remains owned by `rust-toolchain`; this campaign does not duplicate it.
- **D19 — Seed from live sources, not the audit snapshot.** The audit's 2026-11-01 values are a cross-check, not a fixture. Verified drift: `python-build` now locks `musl`/`musl-dev` 1.2.6-r6 on Alpine edge, not the audit's `-r2`. `chromium-build` still has `-r2` in its default package lock and `-r3` in its Helium lock.
- **D20 — Use the smallest reversible assumption on an unlisted detail.** Record it only if a later slice depends on it. Examples: the exact diagnostic wording, the verifier's trace grammar, whether an adapter check is lint-only or has a light fixer. The bound is D11 and D17.

### Delegated

- The exact package table in the lock: cover every package used by the conventional builders and `git-build`; add a package when a real builder uses it. Do not invent versions.
- The exact test names and fixture layout. Use offline fixtures for policy behavior, as the repository already does.
- The choice between `docker buildx imagetools inspect` and a registry API for digest resolution. Prefer the Docker CLI so the refresh and the build agree.
- The canonical image's base tag spelling, as long as the digest matches the lock and the tag is not floating.
- The verifier's trace grammar, as long as it is deterministic, tested, and distinguishes the builder's `/usr/lib` paths from a rustup self-contained or glibc path.

## Baseline

Verified on 2026-11-01 at `0fc287f`, after `uv run repomod sync`. The catalog has 34 active entries, 26 non-forks, and 25 non-forks excluding `mactop`.

### The current policy is blind to the audit's findings

`rust-musl-image` lives in `src/repomod/rules/binary_release.py`. It applies only to repositories tagged `tool`, skips `private-tool`, requires a Rust executable resolvable from the root manifest, and reads only the root `Dockerfile`. It checks the Alpine major/minor tag (not a digest), glibc markers, the rustup archive URL and checksum, the `llvm-prebuilt-musl` archive and checksum, both musl targets, `rust-src`, `musl-cargo`, `crtbegin`, and `libutil.a`. It does not check `apk` package versions or image digests.

`uv run repomod lint --rule rust-musl-image --json` returns `[]`. That is a coverage gap: tansu has no root `Dockerfile`, and most conventional builders use unversioned `musl-dev`; `fdu` is the exception.

### Commands documented but absent

`AGENTS.md`, `docs/design.md`, `docs/policy-development.md`, and `docs/audit.md` describe `pins refresh`. `src/repomod/cli.py` has no `pins` subcommand, and `refresh_pins` in `src/repomod/rules/actions.py` has no production caller. M1 restores the documented command and adds the musl refresher.

`cmd/fix.py` pushes every branch it creates and opens pull requests; the CLI has no `--push` flag. `AGENTS.md` says remote writes require `fix --push`. The campaign does not run `fix`; the owner does. Do not change this behavior in the campaign.

### Fleet facts

| Repository | Builder fact |
| --- | --- |
| `joshuarli/ish` | Root `Dockerfile`, `FROM alpine:3.24.1`, unversioned `musl-dev`; the reference builder. |
| `laputa-systems/diff-pretty`, `e`, `fastermod`, `fz`, `srv`, `tm` | Same Alpine tag and unversioned packages as `ish`. |
| `laputa-systems/fdu` | Root `Dockerfile`, digest-pinned Alpine 3.24.2, `musl-dev=1.2.6-r3`; the closest existing model, but `python3=3.14.8-r0` is stale against the live stable index (`3.14.10-r1`) and its rustup is 1.29.0 against the approved 1.29.1, so it is still a finding. |
| `laputa-systems/vault` | Root `Dockerfile`, `FROM alpine:3.24.1`, adds `gcc` and `mold`, unversioned packages. |
| `laputa-systems/rustybench` | Root `Dockerfile`, Rust binary, LLVM 22.1.8; a library with no release workflow, but a real musl build input. |
| `joshuarli/tansu` | `docker/linux-musl.Dockerfile` and `docker/rust-check.Dockerfile` both start from floating `alpine:edge` with unversioned packages; no root `Dockerfile`, so the old rule cannot see them. |
| `laputa-systems/python-build` | Per-architecture Alpine edge manifest digests and a complete `bootstrap.lock.json`; `musl`/`musl-dev` are 1.2.6-r6, release `3.25.0_alpha20260805`. The audit's `-r2` claim is stale. |
| `laputa-systems/chromium-build` | Base image digest is Alpine 3.24.1; `config/packages.amd64.lock` has `musl-dev=1.2.6-r2`, the Helium lock has `-r3`; the Dockerfile directly pins `-r2`. |
| `laputa-systems/git-build` | `FROM alpine:latest@sha256:...` is Alpine 3.24.1 with pinned packages; musl is inherited from the base image, not installed directly. |
| `laputa-systems/node-build` | No Dockerfile; `.github/workflows/build.yml` uses `docker run` with an Alpine edge 3.25 alpha digest in `ALPINE_AMD64` and unversioned `apk add python3 build-base clang linux-headers perl pkgconf`. |
| `laputa-systems/zzz` | No Dockerfile; Ubuntu host plus Zig 0.16.0 downloaded without a checksum; both musl targets. |
| `joshuarli/mactop` | macOS-only; out of scope. |
| Fork, private-tool, and library repositories without a musl builder | Out of scope; the rule must return no findings for them. |

### Loop timings

- `make test`: 526 passed, 1 skipped, 12.0 s (real 12.7 s).
- `uv run ruff check src tests`: 0.30 s.
- `uv run ty check`: 0.36 s.
- `uv run repomod lint --rule rust-musl-image --json`: 0.13 s.

### Environment facts

- Docker 29.8.2 with buildx 0.22.0; both `linux/amd64` and `linux/arm64` builders are available. A trivial Alpine OCI build completed locally in 1.9 s.
- No registry login is configured and the active `gh` token has no `write:packages` scope. Publishing the canonical image is a **Needs environment** step.
- The `zzz` checkout is on `all-green/zzz`, not its default branch; `sync` fetched but did not switch it. An owner-run fixer will skip it until it is back on the default branch.
- `ultrareview` is not installed on this host; `mdtool` is. M5 uses the repository checks and a manual diff review and records the missing review tool.

## Sequence

All items are serial. M2 depends on M1; M3 depends on M1 and M2; M4 depends on M2 (M3 is needed for the image-mode proof); M5 depends on all. Do not start a later item while an earlier acceptance is red.

### M1 — Approved musl lock and refresh path

**Size:** M (days). **Prerequisite:** none.

**Owned files:** `musl-toolchain.toml` (new), `src/repomod/cmd/pins.py` (new), `src/repomod/cli.py`, `src/repomod/rules/musl_toolchain.py` (new; lock loading, validation, rendering, refresh), `tests/test_musl_toolchain.py` (new), `tests/test_pins_cli.py` (new), `docs/design.md`, `docs/policy-development.md`, `README.md`.

**Outcome:**

1. Add `musl-toolchain.toml` with this contract:

```toml
version = 1

[upstream]
version = "1.2.6"
released = "2026-03-20"

[channels.stable]
branch = "3.24"
image = "alpine:3.24.2"
index_digest = "sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6"
release = "3.24.2"
[channels.stable.manifests]
amd64 = "sha256:d56c381f961d307a21b3ca004cf1e3910f106644aefb1f43e654c8a56c4fd395"
arm64 = "sha256:260479a1cfaf304c4c20da7f8405d3ce313513dcd534bb743257bdd2fe0f3e2d"
[channels.stable.packages.x86_64]
musl = "1.2.6-r3"
"musl-dev" = "1.2.6-r3"
[channels.stable.packages.aarch64]
# same keys, per-architecture versions
[channels.edge]
branch = "edge"
image = "alpine:edge"
index_digest = "sha256:020dfcbaaf4cc1078bf2d9c7ba31a8466e334061dcd2f248001d68f79e52c000"
release = "3.25.0_alpha20260805"
[channels.edge.manifests]
amd64 = "sha256:266f29255458134745f2bf588cb23ed1ed1768b96ff2580a05d70a8aba59e145"
arm64 = "sha256:42d86a3173522de4786cfba0b5d631dbadb3d03a86d218dafb070dafd9809c7e"
# edge packages follow the same per-architecture shape

[toolchain]
llvm_version = "23.1.0-rc2"
llvm_release = "6eb5fb9"
mold_version = "3.0.0"
mold_release = "cfb4aab"
rustup_version = "1.29.1"

[toolchain.archives.llvm.x86_64]
url = "https://github.com/laputa-systems/llvm-prebuilt-musl/releases/download/..."
sha256 = "..."
[toolchain.archives.llvm.aarch64]
# ...
[toolchain.archives.mold.x86_64]
# ...
[toolchain.archives.mold.aarch64]
# ...
[toolchain.archives.rustup.x86_64]
# ...
[toolchain.archives.rustup.aarch64]
# ...

[adapters."laputa-systems/python-build"]
kind = "python-build"
channel = "edge"
lock = "bootstrap.lock.json"
[adapters."laputa-systems/chromium-build"]
kind = "chromium-build"
channel = "stable"
lock = "config/packages.amd64.lock"
[adapters."laputa-systems/node-build"]
kind = "node-build"
channel = "edge"
workflow = ".github/workflows/build.yml"
[adapters."laputa-systems/git-build"]
kind = "git-build"
channel = "stable"
dockerfile = "Dockerfile"
[adapters."laputa-systems/zzz"]
kind = "zzz"
zig_version = "0.16.0"
zig_archive_sha256 = "..."

[image]
# absent until the canonical image is published; then:
# reference = "ghcr.io/<owner>/musl-toolchain"
# digest = "sha256:..."
```

The central package table covers the conventional Rust builders, tansu's two Dockerfiles, and the `node-build` adapter's `apk add` line. The exact keys are the union of those real `apk add` lines, seeded from the live indexes; a starting set is `bash`, `binutils`, `build-base`, `ca-certificates`, `clang`, `clang22`, `coreutils`, `curl`, `file`, `gcc`, `git`, `libgcc`, `linux-headers`, `lld22`, `llvm22`, `make`, `mold`, `musl`, `musl-dev`, `openssl`, `pax-utils`, `perl`, `pkgconf`, `python3`, `strace`, `tar`, `tzdata`. Adapter-listed Chromium, Python, Git, and Zig builders use their own locks or exact pins. Package versions are per architecture because Alpine can publish different revisions per architecture.

2. Implement `repomod pins refresh` with `--check` and `--only`. It runs the existing `refresh_pins`, `refresh_python_tool_versions`, and `refresh_typescript_tool_pins`, plus the new musl refresh; `--only` selects one of `actions`, `python`, `typescript`, or `musl`. `--check` fetches, compares, prints the files that would change, and writes nothing; it exits 1 on drift and 0 when current. A fetch or parse failure is an operational error that writes no file and exits 2. Keep each existing refresher's write-if-changed behavior; the musl lock is rendered and validated in full before a single atomic write.

3. The musl refresh reads only these sources:

- `https://musl.libc.org/releases.html` for the latest upstream `musl-<version>.tar.gz`.
- `https://dl-cdn.alpinelinux.org/alpine/v3.24/main/x86_64/APKINDEX.tar.gz` and the matching `aarch64` index for the stable channel.
- `https://dl-cdn.alpinelinux.org/alpine/v3.24/community/x86_64/APKINDEX.tar.gz` and the matching `aarch64` index for stable packages such as `mold`.
- `https://dl-cdn.alpinelinux.org/alpine/edge/main/x86_64/APKINDEX.tar.gz` and the matching `aarch64` index for the edge channel.
- `https://dl-cdn.alpinelinux.org/alpine/edge/community/x86_64/APKINDEX.tar.gz` and the matching `aarch64` index for edge community packages.
- `docker buildx imagetools inspect alpine:3.24.2` and `alpine:edge` for the approved index and per-architecture manifest digests, and `alpine:3.24` for the observed branch patch. Read the observed release with a minimal `docker run --rm --platform linux/amd64 alpine:3.24 cat /etc/alpine-release`; do not write it to the lock.

Parse the APKINDEX sections by `P:`/`V:`/`S:` records; do not shell out to `apk`. Verify that the upstream musl version is the prefix of the Alpine `musl` package version. Reject a lock whose `index_digest` or manifests are not 64-hex `sha256:` values, whose `musl` and `musl-dev` versions differ within a channel/architecture, or whose channel is unknown. The refresh does not choose new LLVM, mold, or rustup versions; it preserves those values and their checksums.

The stable channel approves an immutable patch, seeded here as `alpine:3.24.2`; the moving `alpine:3.24` branch is printed as an observation and is not written to the lock. A newer branch patch does not fail `--check`; a newer `musl` or `musl-dev` package revision, a digest mismatch, or a stale adapter lock does. The refresh never auto-approves a new patch; the owner reviews the observation, edits `image`/`index_digest`, builds the image, and runs the smoke test before the new patch becomes approved.

4. Seed the initial lock from the live sources at execution time. Cross-check the audit's values: stable musl is 1.2.6-r3, edge is 1.2.6-r6, upstream is 1.2.6. Do not copy the audit's `python-build` `-r2` value.

**Acceptance:**

```sh
uv run pytest tests/test_musl_toolchain.py tests/test_pins_cli.py -q
uv run repomod pins refresh --check --only musl
uv run ruff check src tests
uv run ty check
```

`--check --only musl` must exit 0 against the seeded lock and 1 after a fixture changes one version. The tests must cover a malformed APKINDEX, a failed fetch, a digest mismatch, a stale lock, and an unchanged lock that is not rewritten.

### M2 — Enforce `linux-musl-toolchain` across all musl builders

**Size:** L (multi-day). **Prerequisite:** M1.

**Owned files:** `src/repomod/rules/musl_toolchain.py`, `src/repomod/rules/binary_release.py` (remove `rust-musl-image`; keep `binary-release-*`), `src/repomod/rules/__init__.py` if the rule moves modules, `tests/test_linux_musl_toolchain.py` (new), `tests/test_binary_release.py` (remove the old rule tests), `tests/fixtures/musl_toolchain/` (new), `musl-toolchain.toml` (adapter entries), `docs/policy-development.md`, `docs/tagging.md`, `docs/design.md`, `README.md`.

**Outcome:**

1. Replace `rust-musl-image` with `linux-musl-toolchain` at error severity. Keep the existing Rust builder checks and add the contract below. The rule returns `[]` when a repository has no musl builder; skip `fork`, `private-tool`, and `macos` before scanning.

2. Scope detection. Collect regular files named `Dockerfile`, `Dockerfile.*`, `*.Dockerfile`, and `docker/*.Dockerfile` outside `.git`, `.work`, `target`, `node_modules`, `vendor`, `testdata`, `tests/fixtures`, `.venv`, `build`, and `dist`. A candidate is in scope when its text mentions `musl`, `unknown-linux-musl`, `musl-cargo`, or `musl-dev`, or when a release/CI workflow that builds a `*-linux-musl` target references it through `docker build -f`, `docker/build-push-action` `file:`, or `--file`. Do not follow symlinks; scan the raw text including comments so a builder documented only by a `musl` comment is still found.

3. Builder checks for every in-scope Dockerfile:

- An Alpine `FROM` must carry `@sha256:<64 hex>`. The digest must equal the lock's index digest for a multi-architecture stage, or the per-architecture manifest for a stage with `--platform`. Select the channel from the Alpine tag when present; otherwise match the digest to a channel. A digest that matches neither stable nor edge is a finding. Adapter-listed builders use their configured channel. A `FROM` that uses the published canonical image must equal `[image].digest`; its stage is exempt from package and toolchain checks because the image owns them.
- Resolve `ARG` defaults for image references and package versions. A build argument that controls a pinned image or package version must have an approved default; buildx-provided arguments such as `TARGETARCH` and repository build selectors such as `TARGET` are exempt. A release workflow may pass the same approved image digest explicitly.
- Every `apk add` package must be exact `name=version`; flags and `--virtual` names are skipped. `musl` and `musl-dev` must equal the selected channel's version. Other packages must equal the lock's version when the lock records them; otherwise require an exact version and report a manual finding to add it to the lock. A repository with one multi-architecture `apk add` line must use versions that are equal across architectures or an architecture-conditional `ARG`; a conflict is a finding. A non-approved `--repository` or a direct `.apk` URL is a finding unless the adapter's own lock owns it.
- A musl builder must not use `apt-get`, a Debian/Ubuntu final stage, or an `unknown-linux-gnu` target. A digest-pinned Debian host-compiler stage is allowed only in the `chromium-build` adapter, because that builder's compiler host is intentional and documented.
- An unverified remote installer such as `curl https://sh.rustup.rs | sh` or a toolchain download without a checksum is a finding in every musl builder, not only in binary release builders. The approved form is the versioned archive URL with a SHA256. This catches tansu's current rustup install script.
- A binary prerelease workflow (one that publishes a GitHub prerelease with native artifacts) that builds a `*-linux-musl` target on `ubuntu-*` without a container or `docker build` is a finding, unless the repository is the `zzz` adapter and its Zig archive is checksum-pinned. CI-only musl test builds are out of scope.
- The full Rust release-builder contract applies only when the repository has a binary prerelease contract, using `has_binary_release_contract` from `binary_release.py`. It keeps the existing `rust-musl-image` checks: versioned rustup archive and checksum, `llvm-prebuilt-musl` and checksum, both musl targets, `rust-src`, `musl-cargo`, CRT stubs, and `libutil.a`. Read the root Makefile's `release`/`dist` recipe and require `-Z build-std=std` and `-Z build-std-features=`; a release builder that links precompiled std is a finding. Resolve the LLVM, mold, and rustup versions from both `ARG` defaults and literal archive URLs; either form must equal the lock when the builder uses those archives. fdu uses a literal rustup 1.29.0 URL without an ARG, so the URL check is required. A distro `mold` package must be exact-version pinned and covered by the package table. The Rust nightly stays owned by `rust-toolchain`.
- A Rust builder without a binary release contract, such as `rustybench` or tansu's CI check image, is checked for the base digest and package pins only. Do not force `musl-cargo`, the CRT stubs, `libutil.a`, or build-std onto a CI-only or benchmark-only image.

4. Adapters. The rule reads `[adapters]` from the lock and applies one of these checks. An adapter with its own lock (`python-build`, `chromium-build`) owns its `FROM` and package validation; skip the generic Dockerfile checks for it so it is reported once. `node-build` and `git-build` use the generic `FROM` check plus their adapter rules; `node-build` uses the central table and `git-build` requires exact pins without it. None of the adapters rewrites a generated lock:

- `python-build`: the `bootstrap.lock.json` closure for both architectures must contain `musl` and `musl-dev` equal to the selected channel; its per-architecture image digests must match the channel manifests; its `release` must match the channel's recorded Alpine release. A mismatch is a manual finding naming the repository's own bootstrap refresh.
- `chromium-build`: the selected `packages.*.lock` and the Dockerfile's direct `musl-dev` pin must agree with the selected channel; the Alpine `BASE_IMAGE` digest must match the channel; the Debian compiler-host stage must be digest-pinned. Allow the documented `x86_64-unknown-linux-gnu` compiler-layout symlink; it names Chromium's host compiler directory, not a glibc target build. A mismatch is a manual finding naming the repository's `update-lock` command.
- `node-build`: the workflow's `ALPINE_AMD64` digest must match the channel; the `apk add` line must use exact versions from the lock. This adapter may fix the workflow line.
- `git-build`: every `apk add` package must be exact; musl is inherited from the base and needs no direct pin. The generic `FROM` check owns the base digest. This adapter may fix the Dockerfile.
- `zzz`: the Zig version and archive SHA256 in the lock must match the workflow's download, and the download must verify the checksum. This adapter may add the checksum; it does not build Zig.

5. Fixer. For conventional Rust builders and tansu, rewrite the base to the approved channel digest and every package token to `name=version`. Preserve the existing formatting, comments, and multi-stage structure. The node-build and zzz adapters may rewrite their workflow's `apk add` or checksum line. The fixer's allowed paths are `Dockerfile`, `Dockerfile.*`, `*.Dockerfile`, `docker/*.Dockerfile`, `.github/workflows/*.yml`, and `.github/workflows/*.yaml`. A finding is fixable only when every package or checksum in the file has an approved value; otherwise it stays manual. The existing fix runner must see no remaining fixable finding and no new policy error.

**Acceptance:**

```sh
uv run pytest tests/test_linux_musl_toolchain.py tests/test_binary_release.py -q
uv run repomod lint --rule linux-musl-toolchain --repo laputa-systems/fdu
uv run repomod lint --rule linux-musl-toolchain --repo joshuarli/tansu
uv run repomod lint --rule linux-musl-toolchain --json
uv run ruff check src tests
uv run ty check
```

`fdu` must exit 1 with at least the package-version finding for `python3` (its `3.14.8-r0` is stale against the live stable `3.14.10-r1`) and the rustup mismatch (1.29.0 against the approved 1.29.1); a fixture with a binary prerelease workflow, the lock's exact package set, and the approved rustup version must exit 0. `tansu` must exit 1 and report both `docker/linux-musl.Dockerfile` and `docker/rust-check.Dockerfile`. The fleet JSON must exit 1, or 2 if an operational finding is present; save it under `.work/` (do not commit it) and record the per-repository counts and any operational finding in **Status**. The fixture tests must fail if a floating base, a missing digest, an unversioned package, a stale version, a mismatched canonical image, a glibc final stage, a host musl build, or a stale adapter check is accepted; must fail if an out-of-scope Ubuntu Dockerfile or a skipped repository is flagged; and must show that a CI-only Rust builder passes with only the base and package checks.

### M3 — Canonical OCI builder and local build

**Size:** M (days). **Prerequisite:** M1, M2.

**Owned files:** `docker/musl/Dockerfile` (new), `src/repomod/cmd/musl.py` (new), `src/repomod/cli.py`, `tests/test_musl_build.py` (new), `musl-toolchain.toml` (`[toolchain]`/`[image]` validation), `docs/policy-development.md`, `docs/design.md`, `README.md`.

**Outcome:**

1. Add `docker/musl/Dockerfile`: an Alpine base selected by an `ARG` from the lock, exact `apk` versions, the LLVM/mold/rustup archives selected per `TARGETARCH` and checksum-verified, `rust-src`, both musl targets, the `musl-cargo` wrapper, the CRT stubs, and `libutil.a`. The Dockerfile's `ARG` defaults must equal the lock so a direct build without arguments still uses approved values. M4 adds the verifier to this image.

2. Add `repomod musl build --platform <platform> --output <path>`. It reads the lock, rejects a Dockerfile/lock mismatch, and invokes `docker buildx build` with the lock values. It writes an OCI layout (default `.work/oci/musl`) and prints the resulting local digest. It has no push path. A missing Docker or a failed build exits nonzero without writing a partial layout.

3. Add the image-mode rule path: when `[image].digest` is present, a conventional builder may use the canonical image by digest; the rule accepts that stage and exempts it from package checks. Until the digest is present, pinned mode is the only accepted form. The fixer does not switch a builder to the image before publication.

**Acceptance:**

```sh
uv run pytest tests/test_musl_build.py -q
uv run repomod musl build --platform linux/amd64 --output .work/oci/musl
docker run --rm --platform linux/amd64 <local-image> sh -ec 'clang --version && mold --version && test -x /usr/local/bin/musl-cargo'
```

Time-box the build at ten minutes. If it exceeds the box or requires Linux, stop, park the proof on **Needs environment**, and keep the Dockerfile/lock consistency tests as the local acceptance. Do not build the multi-architecture manifest or push. Remove the local image and OCI output at the end of the slice.

### M4 — Static ELF verifier and workflow enforcement

**Size:** M (days). **Prerequisite:** M2; M3 for the image-mode proof.

**Owned files:** `src/repomod/rules/static_elf.py` (new; the single source of the check text), `docker/musl/verify-static-elf` (new; the image copy), `docker/musl/Dockerfile` (install the verifier), `src/repomod/rules/binary_release.py` (`binary-release-static`), `tests/test_binary_release.py`, `tests/fixtures/static_elf/` (new), `docs/policy-development.md`, `docs/tagging.md`.

**Outcome:**

1. Define the ELF checks once in `static_elf.py`: ELF64, machine `Advanced Micro Devices X86-64` or `AArch64`, no `PT_INTERP`, no `DT_NEEDED`, at least one artifact per expected Linux architecture, and an optional linker trace. The workflow step and the image script are generated from the same text; do not maintain two copies by hand.

2. The verifier accepts one or more artifact paths and an optional `--trace <log>`. Without a trace it performs the ELF checks. With a trace it also requires the builder's `/usr/lib/libc.a` and CRT paths and rejects a rustup self-contained path or a glibc path. A missing trace in image mode is a finding; a pinned-mode builder may omit it until migration. The verifier does not execute arbitrary release binaries; the canary release runs each repository's own smoke command inside the canonical image, or on the target host, on both architectures.

3. Extend `binary-release-static` so its smoke step uses the shared check text. Keep the existing requirement that the smoke runs after artifact download and before publication. The fixer remains able to insert the step. A repository is in image mode when its builder Dockerfile uses `[image].digest`; require the trace only for those, and do not make an existing static check fail solely because the builder is not yet image-mode. A test asserts the checked-in image script equals the rendered text.

**Acceptance:**

```sh
uv run pytest tests/test_binary_release.py -q
docker run --rm --platform linux/amd64 <local-image> verify-static-elf <static-artifact>
docker run --rm --platform linux/amd64 <local-image> verify-static-elf <dynamic-artifact>
docker run --rm --platform linux/amd64 <local-image> verify-static-elf --trace <good-trace> <static-artifact>
docker run --rm --platform linux/amd64 <local-image> verify-static-elf --trace <bad-trace> <static-artifact>
```

For the image proof, build a static hello-world with the image's clang and use the image's `/bin/busybox` as the dynamic fixture; write a good trace containing the builder's `/usr/lib/libc.a` and a bad trace containing a rustup self-contained `libc.a`. The static artifact must pass; the dynamic artifact must fail on `PT_INTERP` or `DT_NEEDED`; the good trace must pass; the bad trace must fail. If M3 was parked, the fixture-based tests are the local acceptance and the image runs move to **Needs environment**.

### M5 — Close

**Size:** S (hours). **Prerequisite:** M1-M4.

**Owned files:** `campaign-oci.md` (Status), the policy docs. No product code unless a review finding requires it.

**Outcome:**

1. Run the full suite, ruff, ty, and the fleet dry-run. Record the final counts in **Status**.
2. Confirm M2 removed `rust-musl-image` completely: implementation, tests, docs, and any dead helper. Do not leave a deprecated alias or tombstone.
3. Remove the campaign's worktrees, local images, containers, and `.work` scratch. Follow the orchestrate worktree lifecycle; if `codex-campaign-closeout.py` is available, use it.
4. Run `mdtool check --untracked campaign-oci.md` and fix broken links. `ultrareview` is not installed here; record that and do the manual diff review.
5. Report what landed, the fleet finding counts, the environment items, and the contract changes (D5, D7, D12, D13).

**Acceptance:**

```sh
make test
uv run ruff check src tests
uv run ty check
uv run repomod lint --rule linux-musl-toolchain --json
mdtool check --untracked campaign-oci.md
```

The fleet lint is expected to exit 1 until the owner-run rollout; its JSON is the migration backlog, not a close failure. The campaign closes when the tests, ruff, ty, and `mdtool` checks pass. **Needs environment** carries the rollout and does not hold the campaign open.

## Acceptance

| Deliverable | Proof | Pass | Fail |
| --- | --- | --- | --- |
| Approved lock and refresh | `uv run pytest tests/test_musl_toolchain.py tests/test_pins_cli.py -q`; `uv run repomod pins refresh --check --only musl` | Fixture tests pass; `--check --only musl` exits 0 on the seeded lock and 1 on drift | A malformed index, failed fetch, digest mismatch, or stale lock is accepted or partially written |
| Enforcing rule and fixer | `uv run pytest tests/test_linux_musl_toolchain.py tests/test_binary_release.py -q`; `repomod lint --rule linux-musl-toolchain --repo laputa-systems/fdu`; `--repo joshuarli/tansu`; a lock-exact fixture | `fdu` exits 1 with at least the stale `python3` and rustup findings and the lock-exact fixture exits 0; `tansu` exits 1 with both Dockerfiles; fixtures cover every check and the fixer postcondition | A floating builder, unversioned package, stale version, glibc final stage, or host musl build passes; a pinned builder or out-of-scope repo is flagged |
| Canonical builder and local build | `uv run pytest tests/test_musl_build.py -q`; `repomod musl build --platform linux/amd64 --output .work/oci/musl` | Lock/Dockerfile consistency test passes; the local build and smoke pass within the time box | A build-argument mismatch, missing checksum, or partial layout is accepted; the build exceeds the box without being parked |
| Verifier and workflow enforcement | `uv run pytest tests/test_binary_release.py -q`; the image verifier commands | Static passes, dynamic fails, good trace passes, bad trace fails; the workflow fixer inserts the shared check | A dynamic binary, wrong architecture, or glibc/rustup trace path passes; image mode allows a missing trace |
| Close | `make test`; `uv run ruff check src tests`; `uv run ty check`; `mdtool check --untracked campaign-oci.md` | All pass; no dead `rust-musl-image` path; Status carries the fleet counts | A stale rule, stale doc reference, worktree, image, or background process remains |

## Needs environment

These items do not hold the campaign open. Run them on the owner's authority and record the result in **Status**.

1. **Publish the canonical image.** No registry login and no `write:packages` scope are available here.
   ```sh
   docker buildx build --platform linux/amd64,linux/arm64 \
     --push -t ghcr.io/<owner>/musl-toolchain:<version> \
     -f docker/musl/Dockerfile docker/musl
   docker buildx imagetools inspect ghcr.io/<owner>/musl-toolchain:<version>
   ```
   Record the index digest as `[image].reference` and `[image].digest` in `musl-toolchain.toml`, then run `repomod pins refresh --check` and the image-mode fixtures.

2. **Heavy builds on Linux.** These are the bespoke builders' own documented commands; do not run them on this macOS host.
   - `python-build`: `python3 build.py doctor`, then `fetch`, `build`, and `qualify` for `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`.
   - `chromium-build`: run its documented `release-linux` make target.
   - `node-build`: `python3 build.py doctor`, then `fetch`, `build`, `qualify`, and `check-budgets` for `x86_64-unknown-linux-musl`.
   - `git-build`: run its documented `git` make target with `DOCKER_PLATFORM=linux/amd64` and `linux/arm64`.
   - `zzz`: `make TARGET=x86_64-linux-musl CPU=x86_64_v3` and `make TARGET=aarch64-linux-musl CPU=cortex_a53`, after adding the Zig archive checksum.
   Record each builder's musl version, artifact, and verifier result.

3. **Canary releases.** Run the release workflows for `srv`, `tm`, and `fdu` on both Linux architectures, then run `verify-static-elf` on the downloaded artifacts and execute each repository's own smoke command inside the canonical image, or on the target host. Only after all three canaries pass should the remaining conventional builders be migrated.

4. **Fleet rollout.** Start with `joshuarli/tansu`, then the conventional builders. Run `uv run repomod fix --rule linux-musl-toolchain` with remote-write authority, then `uv run repomod merge` after CI is green. The fixer handles conventional Rust builders and tansu; the bespoke findings are manual and name their adapter commands. Tansu's unverified rustup installer stays manual until it is replaced with the locked archive. `zzz` must be back on its default branch before it can be fixed.

5. **Consumer migration to the shared image.** After the image is published and the lock is updated, switch the canary Dockerfiles to the canonical image by digest, remove the duplicated toolchain setup, run the canaries again, then roll forward. This is the consolidation phase, not a local campaign task.

6. **Daily refresh.** Schedule `uv run repomod pins refresh --check --only musl` externally (cron or systemd timer). A change is reviewed, the image is rebuilt, the canaries run, and only then are consumer digests updated. Nothing publishes a release automatically.

7. **`fix --push` documentation mismatch.** Decide separately whether `fix` should default to local and require `--push`, matching `AGENTS.md`, or whether the documentation should describe the current unconditional push. The campaign does not change it.

8. **Zig adapter proof.** Verify the Zig archive checksum and the resulting musl version on Linux. The local campaign can add the checksum and the lock entry, but it cannot prove the Zig toolchain's libc inputs.

## Execution boundary and non-goals

**Authorized:** edit this repository's source, tests, docs, lock, and `docker/musl/`; run `uv`, pytest, ruff, ty, `repomod lint`, `repomod pins refresh --check`, `repomod musl build`, and `repomod sync`; create `.work/` scratch, OCI output, and disposable worktrees; read canonical checkouts.

**Forbidden:** `git push`; pull requests; registry publish; `topics apply`; `repomod push`; `repomod fix`; writing to canonical checkouts beyond the clean fast-forward sync; deleting or resetting user data; editing `catalog.toml` repository selection; adding a database, scheduler, background worker, rollout tracker, or per-fixer clone directory; running heavy builds on macOS; leaving containers, images, worktrees, or background processes; adding CI workflows. The publish path is owner-run and listed above.

**Contract changes to surface:** `rust-musl-image` becomes `linux-musl-toolchain` (D5); `pins refresh` becomes a real command with `--check` (D7); `musl-toolchain.toml` becomes a policy file (D6); `repomod musl build` is added without a push path (D12); `binary-release-static` gains the shared verifier and trace requirement for image-mode builders (D13). The `fix` command is unchanged (D15).

**Non-goals:** automatic dependency or toolchain upgrades; automatic releases; a scheduler; full APK closure/offline installation in the first pass; a new catalog tag; converting the bespoke Python, Chromium, Node, Git, or Zig builders to the Rust image; changing the pinned Rust nightly; changing `catalog.toml` tags or selection; fixing the `fix --push` documentation mismatch; publishing a Cosign signature or provenance attestation.

## Reference

Verified builder examples and code:

- `~/repos/ish/Dockerfile` — the reference Rust musl builder.
- `~/repos/laputa-systems/fdu/Dockerfile` — the current digest-and-package-pinned model.
- `~/repos/tansu/docker/linux-musl.Dockerfile` and `rust-check.Dockerfile` — the floating edge case.
- `~/repos/laputa-systems/python-build/bootstrap.lock.json` — the complete-closure example.
- `~/repos/laputa-systems/chromium-build/config/packages.amd64.lock` — the existing package-lock adapter.
- `src/repomod/rules/binary_release.py` — the current `rust-musl-image` and `binary-release-static` rules.
- `src/repomod/rules/actions.py` — `refresh_pins`; `src/repomod/rules/python_standard.py` and `typescript_standard.py` — the other refreshers.
- `src/repomod/cli.py`, `src/repomod/cmd/fix.py` — dispatch and the current fix/push behavior.
- `docs/policy-development.md`, `docs/tagging.md`, `docs/design.md` — the policy and release documentation updated by M1-M4.

External sources verified on 2026-11-01:

- `https://musl.libc.org/releases.html` — upstream releases (latest 1.2.6).
- `https://dl-cdn.alpinelinux.org/alpine/v3.24/main/x86_64/APKINDEX.tar.gz` and the `aarch64` sibling — stable packages.
- `https://dl-cdn.alpinelinux.org/alpine/edge/main/x86_64/APKINDEX.tar.gz` and the `aarch64` sibling — edge packages.
- `docker buildx imagetools inspect alpine:3.24.2`, `alpine:3.24`, and `alpine:edge` — index and manifest digests.
