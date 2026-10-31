# Policy development

Rules live in `src/repomod/rules/` and are registered in `rules/__init__.py`. Each rule has a
stable ID, a read-only check, and an explicit tag predicate. See [tagging semantics](tagging.md)
for tag classification and repository filters.

Return structured `Finding` values with concrete messages and deterministic ordering. Report
unavailable tools, unreadable files, and failed API calls as operational findings, not policy
violations.

Lint IDs are durable catalog policy codes. Catalog version 3 supports per-repository
`[overrides."OWNER/REPO"]` entries with `ignore = ["rule-id"]` and a required `ignore_reason`
string. Validate each code against the
registered rule IDs. An entry of the form `mdtool:R005` ignores one `mdtool` finding code in that
repository and keeps the rest of the documentation audit. An entry of the form
`audit-upgrades:crates:NAME` (ecosystem `pypi`, `npm`, `crates`, or `go`) exempts one dependency
from `audit-upgrades`. An entry of the form `registry-sources:git:NAME` allows one git-sourced package
named `NAME` under `registry-sources`. Document policy changes alongside the rule.

## Fixers

A fixer returns complete UTF-8 file contents keyed by repository-relative path. It does not edit,
stage, or commit files. Declare the smallest safe `allowed_paths` and a rule-specific
Conventional Commit subject. Add a validator when the plan needs a rule-specific check.

The runner rejects out-of-scope paths, invalid plans, new policy errors, and remaining fixable
findings for that rule. A changed fixer gets one local commit. Use fixers only for deterministic
repairs. Keep human release choices out of automatic fixes. Do not generate placeholder README,
license, or agent files.

## Release policy

`actions.toml` owns approved GitHub Action channels, versions, and commit SHAs.
`release-policy.toml` owns package release conventions and the `[binary]` native
prerelease target contract (portable and single-OS target sets plus forbidden glibc
markers). Publishing checks require the matching catalog opt-in tag. Update these
policy files when changing approved release rules.

`binary-release-static` checks public tool prereleases that publish Linux ELF files, including fork
workflows. It skips `private-tool` repositories and tools tagged `macos`. Its release-job smoke runs
after artifact download and before packaging. It rejects ELF `PT_INTERP` and `DT_NEEDED` entries and
requires exactly one `x86_64` and one `aarch64` ELF artifact. The fixer inserts this smoke when the
workflow downloads artifacts in its prerelease job before publication.

Action checks and fixes live in `rules/actions.py`. Package checks live in `rules/release.py`.
Rules inspect repository files but do not install dependencies, build projects, or execute release
scripts. Use offline fixtures under `tests/` for policy behavior. Tests use local Git repositories
and fake GitHub responses. Follow the commands in [AGENTS.md](../AGENTS.md).

`gha-settings` requires the shared concurrency group (G001), cancellation
(G002), and `bash -euo pipefail {0}` defaults (G003) in every GitHub workflow.
Its fixer inserts or corrects those root blocks and validates the result with Pinact.
Findings carry the file, line, column, and code; `status` and `lint` share the
`file line:col CODE` format and print each code's description as a legend. `gha-triggers`
requires `push` to scope branches to the local default branch whenever `pull_request`
is present, so a commit pushed to a PR branch runs CI once. Its fixer resolves the
default branch from local Git and leaves an unactionable manual finding when it cannot.
`gha-syntax` validates GitHub Actions files offline with Pinact's default profile and the
explicit `actions.toml` catalog. Its fixer returns a catalog-backed Pinact plan from a disposable
worktree; repomod keeps validation and repository writes.
`no-formatters` rejects formatter
commands, formatter dependencies, and formatter configuration files. Ruff lint commands such as
`ruff check` remain allowed. Ruff's format command and format configuration are banned. `mdtool`
runs the read-only documentation audit at `~/.local/bin/mdtool` for each repository,
except repositories tagged `fork`, which are exempt. Both rules skip paths under directories
excluded from the Cargo workspace, since vendored fork crates are not maintained documentation.
mdtool's consumer advisories (C001, C002, C003) say that code reads or embeds a document, so they
are not repository defects and `mdtool` does not report them. A README title that is the repository's own name is not a developer-topic
heading (T003), so `node-build` is not flagged for containing "build".
`workflow-redundancy` flags a workflow other than `lint.yml` or `lint.yaml` that runs `make lint` as a
`run` command when the lint workflow also runs it and the two workflows share a trigger event. Lookalikes such as
`make lint-fix`, `echo make lint`, or step names do not count, and a workflow whose triggers cannot be read is
skipped. The finding is not fixable because choosing which workflow keeps the step is a human decision.
`mirrored-docs` compares `.agents/skills` with `.claude/skills` and `.agents/agents` with `.claude/agents`.
For each path that is a regular file in both trees with identical bytes it reports the `.claude` copy; the
owner makes one tree a symlink to the other. Symlinks, files present in only one tree, and files that differ
are not reported, and repositories tagged `fork` are skipped. The finding is not fixable.
`generated-artifacts` reports tracked build outputs and requires ignore coverage for
language artifact directories. Operating-system metadata files such as `.DS_Store` and
`Thumbs.db` are environment noise covered by the global ignore file, so the rule does not
require in-repository patterns for them. Its fixer appends missing language patterns to the end of
a regular or absent root `.gitignore`, preserving existing content; a symlinked `.gitignore` and
tracked outputs stay manual findings.

## GitHub Actions with Pinact

Keep `pinact` on `PATH` when running repomod checks and fixes. `gha-syntax` runs
`pinact lint --profile=default --format=json --catalog actions.toml` on workflow and action
metadata files. The other GitHub Actions fixers use the same Pinact validation command on their
planned changes. The catalog path is explicit; repomod's `actions.toml` owns approval policy, and
catalog-backed linting and fixing make no network requests.
The former `gha-pin` and `gha-version` rule IDs are consolidated into `gha-syntax`.

The `gha-syntax` fixer selects only workflow and action metadata files. It runs Pinact's
catalog-backed `fix` in a detached worktree under the repository's `.work/tmp` directory by
default, honoring an explicitly set `TMPDIR`, and returns changed file contents as a normal
repomod file plan. Repomod checks the plan's allowed paths and policy postconditions, then owns
per-rule commits, push decisions, pull requests, and cleanup. The canonical checkout is not the
Pinact write target.
`gha-lint-workflow` requires every repository to have `.github/workflows/lint.yml` (or
`lint.yaml`) that runs `make lint`. Its fixer creates the workflow from a template pinned to the
current `actions.toml` checkout when it is missing, provisioning the lint toolchain from the
approved pins (`astral-sh/setup-uv` with the approved uv and Python 3.15 for Python sources,
`pnpm/setup` with the approved pnpm and Node for TypeScript sources; Rust resolves its pinned
toolchain file via rustup with no setup step). A present workflow that does not run
`make lint` is a manual finding. `make-lint-target` requires a root `Makefile` that defines a
`lint` target, and pins the linter by detected language: `cargo clippy` for Rust, `ruff`
for Python (never `flake8`), and `oxlint` (or a `pnpm`/`npm` `lint`/`check` script, which
`typescript-standard` already pins to Oxlint) for TypeScript. The rest of the lint command
stays a per-repository human choice, so a missing Makefile or a missing target is always
manual: add the target running the repository's linters as a non-mutating check.
`make-lint-target` rejects `--fix` in the lint recipe. Linter versions ride the existing
pins: the `rust-toolchain` nightly for clippy, `python-tools.toml` for ruff, and
`typescript-tools.toml` for oxlint.

Catalog-backed fixes preserve action subpaths, refuse major changes and downgrades, and do not
treat an unlabeled SHA as approval. The bounded checker is not a complete implementation of
GitHub's workflow validator; dynamic and unsupported syntax remain outside its coverage claims.
See the [Pinact guide](https://github.com/joshuarli/usr/blob/main/tools/pinact/README.md) and its
[compatibility report](https://github.com/joshuarli/usr/blob/main/tools/pinact/docs/compatibility.md).

`pins refresh` fetches stable releases and commit SHAs for `actions.toml`. Pinact's catalog-backed
fixer updates references and version comments only within an identifiable major; major changes and
unlabeled SHAs still need review.

`github-protection` requires zero approving reviews on protected branches so owner-only
automation never waits on reviews. Repositories without branch protection are skipped.
It has no fixer; change protection with explicit API calls.

`rust-release-profiles` rule for binary-shipping Rust repositories. `[profile.release]`
is for performant testing and CI: `opt-level = 3`, `lto = "thin"`,
`panic = "abort"`, `strip = false`, and no `codegen-units` cap, so debug
and test builds link fast while staying optimized. Final prerelease binaries
are built with `--profile dist`, never `--release`:

```toml
[profile.dist]
inherits = "release"
lto = "fat"
codegen-units = 1
strip = true
```

CI workflows build and test the `release` profile; prerelease workflows package
`target/<triple>/dist/`. Libraries without shipped binaries need no `dist` profile.

`rust-toolchain` requires Rust repositories to pin `nightly-2026-09-15` in the root
`rust-toolchain.toml` and include `rust-src` and `clippy`. Its fixer preserves other toolchain
settings and extra components. It leaves malformed TOML, symlinks, non-table `toolchain` values,
invalid component arrays, and toolchain settings declared outside `[toolchain]` for manual repair.

`python-standard` requires Python 3.15 and exact uv, Ruff, and ty pins. `python-tools.toml` records
the approved releases. `pins refresh` updates them from PyPI, and the fixer regenerates `uv.lock` in
a disposable copy before returning its file plan. It updates `.python-version` pins and explicit
GitHub Actions Python versions below 3.15, and removes matrix rows made identical by that update.
Block-style workflow version lists remain manual findings. When a repository has no
`pyproject.toml`, the fixer creates an unpackaged project (`[tool.uv] package = false`, version
`0.0.0`, no runtime dependencies, repository name as project name, approved dev pins) and its
`uv.lock`; it never replaces or merges an existing file.

`typescript-stack` applies to TypeScript repositories. `typescript-tools.toml` records the latest
stable Node.js and pnpm releases, and `pins refresh` updates those approvals. The rule checks
runtime pins offline, requires TypeScript 7 or newer, strict type checking, Oxlint, and Vitest, and
removes Deno configuration and npm, Bun, or Vite+ script commands. Apps also use Effect 4. Frontend
apps use Vite 8 and Solid 2. An app counts as a frontend when it depends on a UI framework
(React or Solid) or contains JSX sources; a plain Vite bundle of vanilla TypeScript does not.

`rust-deps` notes the fleet's minimal Rust stack. Libraries emit via `log`;
binaries initialize with `env_logger`. Use `tracing` only when spans are needed,
and then as `tracing = { default-features = false, features = ["std"] }`
(no `attributes`/`instrument` unless a span is proven). Prefer `miniserde`
for simple JSON; reach for `serde` + `serde_json` only for `flatten`,
`untagged`, borrowed data, or precision `miniserde` cannot express.
Applications use `anyhow`; libraries use `thiserror`. Parametrize with
`rstest`; keep `proptest` for property-based tests and migrate `test-case`
to `rstest` when touched.

`conventional-commits` requires Conventional Commit subjects (`type[(scope)][!]: description`
with one of `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `build`, `ci`, `chore`, `revert`)
on open pull requests; drafts are skipped and overlong subjects warn. It is lint-only with no
fixer: rename the PR explicitly. It needs the network, so it is excluded from offline checks.

`audit-upgrades` reports locked direct dependencies that lag their registry's latest stable
release as fixable findings. Its check refreshes the shared audit cache over the network, so it
is excluded from `status --offline`. Its fixer runs the native upgraders (`uv lock --upgrade`,
per-project `pnpm update --latest`, `cargo update`, `go get -u ./...` plus `go mod tidy`) in a
disposable copy and returns the changed lockfiles and manifests as its file plan. Upgrades that
need manifest edits, such as Rust majors constrained by `Cargo.toml`, fail the fixer postcondition
and stay manual.

`registry-sources` requires every non-fork repository to resolve Cargo, npm/pnpm, and Python
dependencies only from the private `stuff` registry. `registries.toml` owns the three approved URLs
and the `enforce` switch; it ships `enforce = false`, so the rule is registered but reports nothing
until the fleet is migrated. With `enforce = true` it checks, per project directory holding a
lockfile (and the root `package.json` or `pyproject.toml`), that the configuration is present in that
directory or an ancestor: `.cargo/config.toml` replacing `crates-io` with the approved sparse
source, `.npmrc` with `registry=` set to the approved URL and no other registry key (and no
`registry` or `registries` in `pnpm-workspace.yaml`), and a `default = true` uv index at the approved
URL in `pyproject.toml` or `uv.toml`. It also scans lockfiles. `Cargo.lock` keeps crates.io source
ids under replacement, so those are accepted; other registries are findings. pnpm and npm lock
tarball URLs must start with the approved registry, and `uv.lock` registry sources must equal the
approved index. Git sources are findings unless the repository's catalog override ignores
`registry-sources:git:NAME` with an `ignore_reason`. Path, `link:`, `file:`, and workspace sources
are allowed while they stay inside the repository. Unapproved registry sources are reported once
per lockfile and source, listing the first packages.

The fixer adds the missing configuration and leaves every other finding manual. It writes
`.cargo/config.toml` at the repository root (Cargo merges parent configuration), `registry=` into
each npm project's `.npmrc` (npm and pnpm read only the project's own file), and, per uv project, the
default `[[tool.uv.index]]` plus `[tool.uv] environments` for darwin and linux, then runs `uv lock` in
a disposable copy so `uv.lock` records the stuff index. Windows-only packages leave the lock because
`environments` excludes Windows, and stuff publishes macOS and Linux wheels only. A file that already
sets a different cargo source, npm registry, or uv index fails the fixer and needs a manual edit.
