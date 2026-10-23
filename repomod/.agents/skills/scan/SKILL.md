---
name: scan
description: "Resynchronize the catalogued repositories, inspect them for concrete inconsistencies, and file supported findings in each repository's TODO.md."
---

# Scan catalogued repositories

Use this skill when you want a fresh, repository-wide consistency review. It records actionable findings as TODO tickets and recommends whether each issue needs a repository-specific change or a shared repomod rule.

## Synchronize the checkouts

1. From the repomod checkout, run `uv run repomod sync`.
2. Read the sync result and record which checkouts were synchronized or skipped.
3. Do not reset, stash, clean, or switch canonical checkouts. Inspect skipped checkouts read-only and mark their findings as based on an unsynchronized worktree.

`repomod sync` may fetch and fast-forward only clean checkouts on their default branch. Do not run this skill while changing repomod code.

## Inspect the repositories

1. Run `uv run repomod lint --json` and group findings by repository and rule.
2. Inspect every synchronized repository. Read its README, contributor guidance, build and test entrypoints, language and package manifests, lockfiles, GitHub workflows, and existing `TODO.md`.
3. Compare repeated conventions across repositories. Look for conflicting instructions, duplicated policy that has drifted, missing required project files, stale configuration, inconsistent dependency or workflow behavior, and checks that give different results for the same project contract.
4. Follow suspicious findings to their callers or build commands before filing them. Use command output or source locations as evidence. Do not file style preferences, guesses, or differences that follow from distinct project requirements.
5. Decide whether each repair belongs in its repository or a shared repomod rule would prevent the drift elsewhere. Check existing TODOs and open PRs before filing. Do not duplicate a ticket or file work already covered by an open PR. File separate tickets for independent problems.

## File findings

Use the `$todo` skill for ticket priorities, IDs, deduplication, and the exact `TODO.md` format. Put each ticket in the affected repository's root `TODO.md`. Add a ticket only when the evidence supports a concrete problem and the recommendation can be started without asking what it means.

Include paths and line numbers in `Where`. In `Problem`, explain the observed behavior and cite the command or files that establish it. In `Recommendation`, name a repository repair or a candidate repomod rule and fixer. Set `Fix with` to the relevant skill or `none`.

Preserve existing TODO tickets, comments, and IDs. Do not reuse IDs. Do not modify source files, workflows, catalog policy, or other project files while scanning. Do not commit or push the TODO changes.

## Report the scan

Report which repositories you scanned or skipped. List tickets by repository and priority, then list open PRs that cover findings. Report repeated patterns that merit a shared repomod rule. State when the evidence does not support a rule or when a repository needs an owner decision.
