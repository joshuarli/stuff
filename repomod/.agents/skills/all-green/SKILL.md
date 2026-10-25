---
name: all-green
description: "Resynchronize all catalogued repositories, drive every failing CI to green through one subagent per failing repo, and report what merged and what still needs an owner decision."
---

# Bring the fleet to green CI

Use this skill when every catalogued repository should have passing CI. The parent surveys
once, fans out to one subagent per failing repository, then integrates and reports. Do not
start it while changing repomod code.

## Survey

1. From the repomod checkout, run `uv run repomod sync`. Record which checkouts synchronized,
   which are dirty, detached, or ahead, and which were skipped.
2. Run `uv run repomod status --failed` and list open fix PRs with
   `gh pr list --state open` per repository. For each open PR record its merge state and
   failing checks with `gh pr view --json mergeStateStatus,statusCheckRollup`.
3. Never reset, stash, clean, switch, or discard changes in canonical checkouts. A dirty,
   detached, or ahead checkout is not a lane; report it and leave it alone.
4. Triage each failure before dispatching: read the failed log with
   `gh run view <id> --log-failed`, compare against the default branch CI history with
   `gh run list --branch <default>`, and close superseded duplicate PRs with a comment
   naming the newer PR. Flakes get one `gh run rerun --failed`; a second identical failure
   is a product bug, not a flake. Failures your own automation caused take priority over
   pre-existing red CI.

## Confirm before fanning out

Do not spawn subagents yet. First tell the user exactly what is broken in each repository:
failing checks with the root cause where known, whether the failure reproduces on the
default branch, open PRs covering it, and the proposed fix direction per lane. Then wait
for explicit confirmation, offering the full fleet or a subset of repositories. Only lanes
the user approved may spawn; anything else stays parked and is reported as such.

## Fan out

Spawn one `general` subagent per approved failing repository. Do not batch repos into one
child and do not let children touch each other's checkouts. Every brief is self-contained
and states:

- The repository path, its default branch, and the exact failing checks with log excerpts.
- Read the repository's `AGENTS.md` first and reproduce with the same commands CI runs.
- Work on a new `all-green/<short-name>` branch cut from the default tip. Never use a
  `repomod/fix/*` branch; those belong to the repomod fix flow. Require a clean checkout
  first; otherwise stop and report.
- Keep the fix minimal and product-scoped. Workflow and policy defects belong in repomod,
  not in the repository. Do not weaken or delete a failing test unless the test is wrong,
  and say so in the report.
- Verify locally with the narrowest hard check (focused tests, `jactionlint` for workflow
  edits), then push the branch and open or update a PR with `gh`. CI in most repositories
  runs on `pull_request`, so a PR is required to get a signal; do not push fixes straight
  to the default branch to "test" them.
- Poll `gh pr view --json mergeStateStatus,statusCheckRollup` after each push. Stop after
  three fix rounds without progress and report the evidence.

A child's final report stays with the parent: what changed, the decisive check and its
result, remaining red checks with causes, and the PR URL.

## Integrate and report

1. For each green PR, enable auto-merge where the repository allows it. Private free-tier
   repositories cannot auto-merge; merge those green PRs directly with
   `gh pr merge --rebase` and say so.
2. Never merge a red PR and never force-push a branch another lane owns. Merging needs all
   required checks green; a red product test is an owner decision, not an automation task.
3. Resync once at the end and re-run the survey to confirm the fleet state.

Report per repository: merged, green-awaiting-merge, red-blocked with the root cause and
the exact owner decision needed, and skipped checkouts. List protected-branch mismatches
(stale required contexts, approval requirements) separately; fix server-side settings with
explicit `gh api` calls, never as drive-by edits inside a product lane.
