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
- [x] all 23 migration PRs merged; the private repos (discover, shadowdriver, toolbox) were verified locally because CI cannot start there (account billing)
- [x] default-branch CI repaired where it blocked the PRs: h12tiny (smolith pin, bare-LF test race), ish (musl pipe test, stale macOS `l` test), vault (prompt expectations after 1efe588), fz (required check names)
- [x] stuff gaps found by CI and filled: tool crates (wasm-bindgen-cli, rustybench), laputa-ptytest 0.3.0, the h12tiny fork locks (env_logger 0.9, pretty_env_logger)
- [ ] tm #11 (approved Action pins and a docs link) is open; tm CI is red on main (`missing tmux 3.8 reference`), which is the owner's in-flight work
- [ ] tansu keeps two git forks (catalog exception); publish h2-futures-lite and hyper-futures-lite to the native registry to remove it
- [x] fz's Cargo.lock has a sourceless `laputa-ptytest` entry; regenerate it
- [ ] unlocked nested manifests (gitignored or absent lockfiles) resolve at CI time against whatever stuff holds; add their locks to the ingest when CI reports a missing crate

## Phase D: enforcement

- [x] repomod `registry-sources` rule enforced (`enforce = true`); 0 findings fleet-wide
- [ ] audit-upgrades and `fix` against a stuff-only world (upgrades need ingestion first)
