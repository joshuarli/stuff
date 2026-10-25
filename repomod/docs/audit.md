# Dependency audit

`audit-upgrades` is the fleet's own dependabot rule. It reports stale dependencies,
known vulnerabilities, and registry policy bans. It trusts lockfiles as the
source of truth for installed versions. Swift is out of scope and is not
audited.

```sh
uv run repomod fix --rule audit-upgrades
```

## Sources

- Python: direct dependencies from `pyproject.toml` resolved through `uv.lock`.
- TypeScript: direct dependencies from `package.json` (root and immediate
  subdirectories) resolved through their `pnpm-lock.yaml` files. Transitive
  JavaScript dependencies from all `pnpm-lock.yaml` packages are included for
  vulnerability checks.
- Rust: direct dependencies from `Cargo.toml` resolved through `Cargo.lock`.
- Go: requirements from `go.mod`.
- Swift is out of scope and is not audited.

Latest stable versions come from PyPI, the npm registry, crates.io, and the Go
proxy. Vulnerabilities come from the free OSV API with no authentication.
Results are cached under `XDG_CACHE_HOME/repomod/audit.json` for 24 hours.
Upstream fork comparisons are cached under `XDG_CACHE_HOME/repomod/upstream.json`
for one hour. `--offline` uses only cached data. `--refresh` ignores the cache.

## Rules

- `audit-lock`: a required lockfile is missing or unreadable. Python requires
  `uv.lock`. TypeScript requires `pnpm-lock.yaml`.
- `audit-banned`: a bloated or legacy dependency is present. Python must not
  use `requirements*.txt`. JavaScript must not use yarn: `yarn.lock`,
  `.yarnrc`, `.yarnrc.yml`, `.yarnrc.yaml`, `.yarn/`, and
  `packageManager: yarn@*` are banned because pnpm is the only package
  manager. Rust must not depend on `clap` or `tokio`,
  including transitive `clap-*` and `tokio-*` crates via `Cargo.lock`.
  Rust must not use HTTP clients `reqwest`, `tiny_http`, `hyper`, `h2`,
  `ureq`, `attohttpc`, `surf`, `isahc`, `curl`, or `awc` (including suffixed
  crates such as `hyper-*` and `reqwest-*`); use `h12tiny-*`,
  `laputa-h2-futures-lite`, or `laputa-hyper-futures-lite` instead.
- `audit-registry`: a filesystem or git dependency is present. All
  dependencies must come from a registry. Rust `path` dependencies without a
  `version` are banned; `version` plus `path` workspace members are allowed.
  Python `tool.uv.sources` with `path` or `git` is banned. JavaScript
  `file:`, `link:`, `portal:`, `workspace:`, relative, and URL specs are
  banned. Go `replace` directives pointing at the filesystem are banned.
  A `path` dependency that resolves to the repository's own root package (for
  example a `bench/` harness depending on its parent library via
  `path = ".."`) is exempt: it exercises the working copy and has no registry
  alternative.
- `audit-outdated`: a locked direct dependency is older than the latest stable
  release in its registry.
- `audit-vuln`: a locked dependency has a known OSV vulnerability.
- `audit-upstream`: a fork is behind its upstream parent. The finding names the
  parent, its branch, and how many commits behind (and ahead, when diverged) the
  fork is. Unresolvable comparisons are operational findings. `status` shows the
  same comparison per fork and `--failed` includes forks that are behind.

Catalog `[overrides."OWNER/REPO"]` entries with `ignore = ["audit-*"]` skip
selected audit rules per repository.

## Fixes

The fixer runs native upgraders in clean checkouts when the tool is
available: `uv lock --upgrade`, per-project `pnpm update --latest`,
`cargo update`, and `go get -u ./...` followed by `go mod tidy`. It reports
what it ran and what failed. Dirty checkouts are skipped. Version bumps that
need manifest edits, such as Rust major upgrades constrained by `Cargo.toml`,
remain manual.

The `audit-upgrades` rule rolls the same upgrades into the standard autofix
flow: one commit per repository, with pull requests through `fix`.

```sh
uv run repomod fix --rule audit-upgrades
```

The rule reports only outdated direct dependencies as fixable findings, so
`lint` and `status` surface upgrade debt. Vulnerability, ban, and lockfile
remaining findings stay manual because the upgraders cannot fix them
deterministically. The rule is excluded from `status --offline` because its
check refreshes the registry cache over the network. A repository can opt out
with `ignore = ["audit-upgrades"]` in its catalog override.
