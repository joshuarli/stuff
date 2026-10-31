# Backlog: fleet dependencies on stuff

Goal: every non-fork catalogued repository resolves Cargo, npm/pnpm, and Python packages only
from `stuff` (~/d/stuff), and repomod enforces it.

## Decisions

- Scope is package registries only (crates.io, registry.npmjs.org, pypi.org). Toolchain
  downloads, apk, base images, and GitHub Actions are out of scope for now.
- ~/d/stuff may be changed to remove the blockers below.
- Git sources are allowed through catalog exceptions with a reason; path and `link:` workspace
  members inside a repository stay allowed.
- The repomod rule lands last, after the fleet is migrated, so `status` stays at 0.
- Nothing is published to R2 or deployed without an explicit go from the owner.
- shadowdriver stays private and is never published to stuff. toolbox, the only tracked repo that
  depends on it, keeps its git dependency through a catalog exception; its other dependencies
  still move to stuff.

## Phase A: stuff blockers (worktrees in ~/d/stuff, local only)

- [x] mycrates: 926 crates ingested and published locally; 20 of 26 lockfiles pass `cargo fetch --locked` unchanged (merged to stuff main, unpushed)
- [x] mynpm: 370 hosted versions plus 107 metadata-only stubs; standard tarball path served; six pnpm projects install with default settings and an unchanged lock
- [x] mypypi: 52 versions, 152 wheels for macOS arm64 and Linux arm64/x86_64; nine uv projects lock and sync (`[tool.uv] environments` snippet required)
- [x] deploy: R2 sync past 1000 objects, fleet-ingest.sh, conditional promotion, dry-run mode, docs (merged to stuff main, unpushed)
- [x] adopt-live: `mycrates adopt`, `mynpm adopt`, `deploy/r2-pull.py` rebuilt state from the live buckets
- [x] rehearsal: fleet ingested on top of the adopted state; every live package byte-identical; real cargo, pnpm, and uv clients pass (20 of 26 Cargo locks, the rest are repo defects)

## Phase B: publish (owner-run or owner-approved)

- [x] canonical state in `~/.local/state/stuff` (rehearsal result, intermediate generations pruned)
- [x] read-only plan against the real buckets: 0 conflicts, 0 multipart; put 1607 / 667 / 194 objects (cargo / npm / pypi)
- [x] published to R2 through `cf` with `R2_PROMOTE_UNCONDITIONAL=1` (an API token could not be minted: the `cf` login has no token-management scope), so the `current` marker moves with a plain put; cargo f5a1048969baf6ddb0c38a77, npm 76ebe653859c751fe9070c03fd784ac2710de8db2b89b36e4e50b29d91397daa, pypi 469766803c6329cbbf473425a2938a0e0aa7549e116a866eb4f97504f380c64d; `deploy/deployed-smoke.sh` passes
- [x] delta ingest after repo locks moved (cargo +32 crates, npm +81 versions, laputa-ptytest 0.3.0 for fz)
- [ ] `make test-r2-live` multipart acceptance (needs R2 S3 credentials and a scratch bucket; no file is over 100 MB so nothing published needs multipart)
- [ ] known gap: `mypypi add` appends unrestricted duplicate rows for hand-restricted `pyobjc-*` entries; do not re-run `add` against the catalog until fixed
- [ ] known gap: `mycrates add` pins an empty checksum that `fetch` rejects; ingest through a lock instead
- [ ] new repo locks must be ingested before they merge (a Cargo.lock or pnpm-lock.yaml version missing from stuff fails CI)

## Phase C: migrate repositories

- [x] `registry-sources` fixer (`repomod fix --rule registry-sources`, run with `enforce` flipped locally and uncommitted) writes the cargo source replacement, `.npmrc`, uv index plus darwin/linux `environments`, and relocks `uv.lock`
- [x] PRs opened for 23 repositories (diff-pretty, pytest-parallel, e, fastermod, fdu, hi-lite, rustybench, srv merged)
- [ ] remaining PRs: wait for CI after the delta publish; discover, shadowdriver and toolbox are private and CI cannot start (account billing), so verify them locally before merging
- [ ] default branches already red before this change: ish, h12tiny (also stale `repomod/fix` branch), discover, shadowdriver, toolbox
- [ ] tm has a dirty checkout; its PR came from a worktree
- [ ] tansu keeps two git forks (catalog exception); publish h2-futures-lite and hyper-futures-lite to the native registry to remove it
- [ ] fz's Cargo.lock has a sourceless `laputa-ptytest` entry; regenerate it

## Phase D: enforcement

- [x] repomod `registry-sources` rule and `registries.toml` merged, disabled (`enforce = false`); enable after Phase C (41 findings in 23 repos today)
- [ ] audit-upgrades and `fix` against a stuff-only world (upgrades need ingestion first)
