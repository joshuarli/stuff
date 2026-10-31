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
- [ ] R2 S3 credentials (`cf` is 403 on token management; needs an R2 API token with object read+write on the three buckets) and promotion with `--expected` 127688d2a3bd447af4b9f830 (cargo), c7c458b0e78a9841c1c7de8808d6af5d61957df5ffedbb77760674e93e9bbdb8 (npm), 345ff46c78fe3e1ef597d206d797f0c8389eb1e7a147d8973da43156db11ee2c (pypi)
- [ ] known gap: `mypypi add` appends unrestricted duplicate rows for hand-restricted `pyobjc-*` entries; do not re-run `add` against the catalog until fixed

## Phase C: migrate repositories

- [ ] per repo: `.cargo/config.toml` replace-with, `.npmrc`, uv index, regenerated locks, PRs

## Phase D: enforcement

- [x] repomod `registry-sources` rule and `registries.toml` merged, disabled (`enforce = false`); enable after Phase C (41 findings in 23 repos today)
- [ ] audit-upgrades and `fix` against a stuff-only world (upgrades need ingestion first)
