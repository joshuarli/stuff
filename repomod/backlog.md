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

## Phase A: stuff blockers (worktrees in ~/d/stuff, local only)

- [ ] mycrates: ingest every fleet Cargo.lock (889 versions) into the crates-io archive; fetch
  concurrency; verified with `cargo fetch --locked` per repo against a local stuff-server
- [ ] mynpm: ingest every pnpm-lock.yaml (477 versions); verified with
  `pnpm install --frozen-lockfile` per repo; decide whether pnpm locks stay valid
- [ ] mypypi: ingest from uv.lock; fleet wheels (48 versions); sdist-only and musllinux gaps;
  uv.lock behaviour under `[[tool.uv.index]]`
- [ ] deploy: R2 sync past 1000 objects; fleet ingest orchestration; docs

## Phase B: publish (owner-run or owner-approved)

- [ ] R2 credentials and first publish of the ingested generations

## Phase C: migrate repositories

- [ ] per repo: `.cargo/config.toml` replace-with, `.npmrc`, uv index, regenerated locks, PRs

## Phase D: enforcement

- [ ] repomod `registry-sources` rule and `registries.toml`; enabled after Phase C
- [ ] audit-upgrades and `fix` against a stuff-only world (upgrades need ingestion first)
