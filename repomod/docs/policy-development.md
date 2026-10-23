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
`release-policy.toml` owns package release conventions. Publishing checks require the matching
catalog opt-in tag. Update these policy files when changing approved release rules.

Action checks and fixes live in `rules/actions.py`. Package checks live in `rules/release.py`.
Rules inspect repository files but do not install dependencies, build projects, or execute release
scripts. Use offline fixtures under `tests/` for policy behavior. Tests use local Git repositories
and fake GitHub responses. Follow the commands in [AGENTS.md](../AGENTS.md).

`gha-settings` requires the shared concurrency group, cancellation, and
`bash -euo pipefail {0}` defaults in every GitHub workflow. `gha-syntax` runs jactionlint's safe
fixes and pinact's action pinning in a disposable worktree. `no-formatters` rejects formatter
commands, formatter dependencies, and formatter configuration files. Ruff lint commands such as
`ruff check` remain allowed. Ruff's format command and format configuration are banned. `mdtool`
runs the read-only documentation audit at `~/.local/bin/mdtool` for each repository.
