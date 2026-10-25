# Policy development

Rules live in `src/repomod/rules/` and are registered in `rules/__init__.py`. Each rule has a
stable ID, a read-only check, and an explicit tag predicate. See [tagging semantics](tagging.md)
for tag classification and repository filters.

Return structured `Finding` values with concrete messages and deterministic ordering. Report
unavailable tools, unreadable files, and failed API calls as operational findings, not policy
violations.

Lint IDs are durable catalog policy codes. Catalog version 2 supports per-repository
`[overrides."OWNER/REPO"]` entries with `ignore = ["rule-id"]`. Validate each code against the
registered rule IDs. Document policy changes alongside the rule.

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

Action checks and fixes live in `rules/actions.py`. Package checks live in `rules/release.py`.
Rules inspect repository files but do not install dependencies, build projects, or execute release
scripts. Use offline fixtures under `tests/` for policy behavior. Tests use local Git repositories
and fake GitHub responses. Follow the commands in [AGENTS.md](../AGENTS.md).

`gha-settings` requires the shared concurrency group (G001), cancellation
(G002), and `bash -euo pipefail {0}` defaults (G003) in every GitHub workflow.
Its fixer inserts or corrects those root blocks and validates the result with jactionlint.
Findings carry the file, line, column, and code; `status` and `lint` share the
`file line:col CODE` format and print each code's description as a legend. `gha-syntax` runs jactionlint's safe
fixes and pinact's action pinning in a disposable worktree. `no-formatters` rejects formatter
commands, formatter dependencies, and formatter configuration files. Ruff lint commands such as
`ruff check` remain allowed. Ruff's format command and format configuration are banned. `mdtool`
runs the read-only documentation audit at `~/.local/bin/mdtool` for each repository.

`gha-version` updates action references and version comments from `actions.toml`. `pins refresh`
fetches stable releases and commit SHAs. A clear version reference can move to a newer major; an
unlabeled SHA still needs review.

`rust-toolchain` requires Rust repositories to pin `nightly-2026-09-15` in the root
`rust-toolchain.toml` and include `rust-src` and `clippy`. Its fixer preserves other toolchain
settings and extra components. It leaves malformed TOML, symlinks, non-table `toolchain` values,
invalid component arrays, and toolchain settings declared outside `[toolchain]` for manual repair.

`python-standard` requires Python 3.15 and exact uv, Ruff, and ty pins. `python-tools.toml` records
the approved releases. `pins refresh` updates them from PyPI, and the fixer regenerates `uv.lock` in
a disposable copy before returning its file plan. It updates `.python-version` pins and explicit
GitHub Actions Python versions below 3.15, and removes matrix rows made identical by that update.
Block-style workflow version lists remain manual findings.

`typescript-stack` applies to TypeScript repositories. `typescript-tools.toml` records the latest
stable Node.js and pnpm releases, and `pins refresh` updates those approvals. The rule checks
runtime pins offline, requires TypeScript 7 or newer, strict type checking, Oxlint, and Vitest, and
removes Deno configuration and npm, Bun, or Vite+ script commands. Apps also use Effect 4. Frontend
apps use Vite 8 and Solid 2.
