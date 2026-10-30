---
name: upgrade-dependencies
description: "Use for /upgrade-dependencies: survey audit-upgrades, audit-vuln, audit-banned, audit-registry and audit-lock across the catalogued repositories, upgrade what the fixer can, hand the rest to one subagent per repository, and report what merged and what needs an owner decision."
disable-model-invocation: true
---

# Upgrade dependencies across the fleet

Use this skill when the catalogued repositories should be brought up to date. It extends
`docs/audit.md`: read that first for sources, rules, and fixer behaviour. Do not start it while
changing repomod code, because `fix` runs the working tree's rules against the live catalog.

## What writes where

- `uv run repomod fix --rule audit-upgrades` runs native upgraders (`uv lock --upgrade`,
  `pnpm update --latest`, `cargo update`, `go get -u` plus `go mod tidy`) in a disposable
  worktree of a clean default-branch checkout. It makes one local commit on a
  `repomod/fix/<id>` branch, then pushes that branch and opens or reuses a pull request. It is a
  remote write: never run it without the confirmation step below.
- The rule only reports outdated direct dependencies as fixable. `audit-vuln`, `audit-banned`,
  `audit-registry`, and `audit-lock` findings are always manual.
- `status --offline` does not run `audit-upgrades`. Use `lint` for the survey.
- Registry and OSV data is cached for 24 hours. A survey older than that is stale.
- The rule compares against what the repository can install: npm releases newer than pnpm's
  `minimumReleaseAge` do not count, `uv`/`ruff`/`ty` follow `python-tools.toml` (bumped with
  `pins refresh`), and a finding is manual when a specifier, exact `=` pin, or workspace MSRV
  blocks the latest version. A deliberate pin is suppressed with
  `audit-upgrades:ECOSYSTEM:NAME` in a catalog `ignore` list plus `ignore_reason`.
- `fix` leaves the canonical checkout on its `repomod/fix/*` branch after it runs. Those
  checkouts are not eligible for another `fix` until the PR is merged and the checkout is back
  on the default branch.

## Survey

1. Confirm checkout state without mutating anything: `uv run repomod status --dirty --offline`.
   A dirty, detached, ahead, or fix-branch checkout is not eligible for `fix`; list it and
   leave it alone. Never reset, stash, clean, switch, or discard changes in canonical
   checkouts.
2. Run `uv run repomod lint --rule audit-upgrades --rule audit-vuln --rule audit-banned
   --rule audit-registry --rule audit-lock --json` and group findings per repository and
   rule. Repositories with `ignore` entries for an audit rule are out of scope for that rule;
   say so rather than overriding the catalog.
3. Classify every outdated dependency:
   - **Minor or patch, lockfile only:** the fixer handles it.
   - **Major, or constrained by a manifest** (Rust requirements in `Cargo.toml`, npm ranges,
     Go majors, `requires-python` caps): manual, one subagent per repository.
   - **Fork** (`fork` tag): upgrading diverges the fork from its upstream. Do not include forks
     unless the owner asks; report `audit-upstream` drift separately.
   - **Pinned toolchains and vendored sources:** a pinned Rust nightly, Node, pnpm, or uv pin is
     not a dependency. Do not change a pinned Rust nightly as part of an upgrade.
4. Vulnerabilities come first. For each `audit-vuln` finding record the advisory, whether the
   package is direct or transitive, and the fixed version. A transitive-only advisory needs an
   owner decision (override, direct bump, or accept); do not add overrides on your own.

## Confirm before writing

Do not run `fix` or spawn subagents yet. Tell the user, per repository: the fixer-eligible
bumps, the manual majors with their likely breaking changes, vulnerabilities, bans, skipped
checkouts, and forks excluded. Then wait for explicit confirmation of the full fleet or a
subset. Only approved repositories proceed; everything else stays parked and is reported.

## Fixer pass

1. Start with one or two small repositories: `uv run repomod fix --rule audit-upgrades
   --repo OWNER/NAME`. Read the output. A `failed` or `skipped` line is a result to report,
   not to retry blindly.
2. If the first repositories are healthy, run the approved set. Keep `--workers` at the default.
3. Verify each fixed branch before trusting it. Create a worktree of the `repomod/fix/*` branch
   under `.work/wt/`, run the repository's own check commands from its `AGENTS.md` (the same
   commands CI runs), and confirm the diff is lockfile and manifest changes only. A red check
   is not merged; report it with the upgrade that caused it, and leave the PR for the owner.
4. Auto-merge is enabled only where the repository allows it. Private free-tier repositories
   cannot auto-merge: merge green PRs directly with `gh pr merge --rebase` and say so. Never
   merge a red PR.

## Manual upgrades

Spawn one `worker` subagent per approved repository. Do not batch repositories and do not let
children touch each other's checkouts. Every brief is self-contained and states:

- The repository path, default branch, the exact packages and target versions, and the
  `audit-*` findings it must clear.
- Read the repository's `AGENTS.md` first. Work on a new `upgrade/<short-name>` branch cut from
  the default tip in a worktree under the repository's `.work/wt/`. Never use a
  `repomod/fix/*` branch. Require a clean checkout; otherwise stop and report.
- Read the release notes of every major version crossed and fix the breakage at its cause. Do
  not weaken or delete a failing test unless the test is wrong, and say so.
- Dependencies are not added or swapped: no replacement of a banned crate with a new one, no
  new override entries, no changed toolchain pin. Those are owner decisions to report.
- Verify with the repository's own commands, then push the branch and open a PR with `gh`. Stop
  after three rounds without progress and report the evidence.

A child's report stays with the parent: what changed, the decisive check and its result,
remaining red checks with causes, and the PR URL.

## Report

Re-run the survey and report per repository: merged, green awaiting merge, red-blocked with the
cause, skipped checkouts, forks excluded, and findings that need an owner decision
(vulnerabilities that cannot be fixed by a direct bump, `audit-banned` swaps, `audit-registry`
sources, missing lockfiles). List what is still outdated and why. Leave the Actions catalog
(`actions.toml`) and the Node, pnpm, and uv pins to `pins refresh` and the `gha-*` rules; they
are a separate change.
