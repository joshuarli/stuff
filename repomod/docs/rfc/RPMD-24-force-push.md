# RPMD-24: Force-push of an open fix branch

Status: draft

## Summary

`--apply` recreates the fix branch from the default branch and pushes it with `--force`. Commits added on that branch after the PR opened are replaced. This RFC asks whether that stays the contract.

## Motivation

`commit_and_push` force-pushes so a re-run lands the reviewed tree even when the previous push stopped halfway. `clone` only fetches the default branch, and it deletes other local branches, so the client never sees commits that exist only on the remote fix branch.

A reviewer who pushes to the PR branch loses those commits on the next `--apply` that is not skipped. A matching `pr_created` row is skipped, so this happens when the fix hash changes or the row is `failed`, `pending`, or `interrupted`.

## Design

Recommended: leave the force-push in place.

Record the SHA that was pushed, in `status.yaml`, next to `pr_url`. The README already tells you that a later push replaces commits on the open branch. No code change.

## Alternatives considered

Fetch the remote fix branch before the push. If its tip is not the SHA this tool pushed, stop that repo and leave the branch alone. Push the tool's own update with `--force-with-lease`. A reviewer commit then blocks the apply until you drop it or change `branch` in `config.yaml`. Re-runs are no longer idempotent when someone has pushed to the PR.

## Open questions

1. Should a commit on the open fix branch block the next `--apply`, or be replaced? Recommendation: replace it. The rollout is generated, and the README now says so.
2. If you want the lease, should the blocked repo be `failed` (retried forever) or a new status that `--status` shows until you clear it? Recommendation: `failed` with a reason that names the remote SHA, so you notice it.

## Decision

Undecided.
