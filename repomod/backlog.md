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
- [~] mynpm: 370 of 477 hosted (107 are other-platform packages mynpm skips); existing pnpm locks need
  the standard tarball path served (lane npm-tarball-path) and a decision on pnpm lockfile verification
  (`trustLockfile` / `minimumReleaseAge: 0`) or hosting stubs for the unhosted platform packages
- [~] mypypi: 47 of 48 published for macOS arm64 only; Linux wheels, build backends, and the
  ansicon/Windows problem in lane pypi-linux (`[tool.uv] environments` instead of a check exception)
- [x] deploy: R2 sync past 1000 objects, fleet-ingest.sh, docs (merged to stuff main, unpushed)

## Phase B: publish (owner-run or owner-approved)

- [ ] R2 credentials and first publish of the ingested generations

## Phase C: migrate repositories

- [ ] per repo: `.cargo/config.toml` replace-with, `.npmrc`, uv index, regenerated locks, PRs

## Phase D: enforcement

- [x] repomod `registry-sources` rule and `registries.toml` merged, disabled (`enforce = false`); enable after Phase C (41 findings in 23 repos today)
- [ ] audit-upgrades and `fix` against a stuff-only world (upgrades need ingestion first)
