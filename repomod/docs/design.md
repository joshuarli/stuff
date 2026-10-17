# Design

repomod pushes to hundreds of repos, so reliability, resumability, and idempotency are
non-negotiable. This page explains how the code keeps them. For how re-runs behave from the
user's side, see [Re-running a fix](../README.md#re-running-a-fix).

## Reliability

- Every subprocess call has a 120-second timeout (`SUBPROCESS_TIMEOUT` in `utils.py`).
- Network calls in `git.py` and `sync.py` use `@retry`: 3 attempts with exponential backoff.
- `github_rate_limiter` spaces GitHub calls made through `gh_run` and `clone` by 0.25 seconds.
- `pr_create_limiter` spaces PR creation and marking ready by 2 seconds. GitHub's secondary
  rate limits on creating content are much tighter than on reads.
- One repo failing doesn't stop the run.
- `atomic_write_yaml` writes status files to a temp file and renames it into place.

## API budget

Keep GitHub API calls minimal. Batch them or look them up once per run, not once per repo.

`--apply` does one search for every existing PR on the fix branch. Then it makes at most one
`gh pr create` per new repo, which also sets reviewers and returns the URL. Repos that only
need a push, or whose PR is already merged or closed, cost no API calls.
`tests/test_apply_gh_calls.py` asserts the exact calls. Keep it passing when you touch the PR
path.

## Node ids

A PR's GraphQL node id never changes, so `status.yaml` caches it as `pr_node_id`. It comes
free from the search for existing PRs. New PRs get one batched lookup after `--apply`. Draft
and merged state can change, so repomod never caches them.

## Resumability

`^C` raises `ShutdownError` through `interrupt.wait()`, which uses `Event.wait()` so sleeping
threads wake instantly. In-flight workers finish their current network call. Incomplete repos
are marked `interrupted`, and the next run picks them up.

Interrupted repos must not increment done counters, so never call `pd.finish()` for them.
`execute_fix_on_repo` writes the status before `pd.finish()`, so a `^C` between the two can't
overwrite a recorded result with `interrupted`.

## Idempotency

`compute_fix_hash` hashes `main.py` plus the parsed config without `skip`.
`legacy_fix_hash`, the raw bytes of both files, is still accepted so older `status.yaml`
entries stay current.

`commit_and_push` is safe to re-run after a partial failure. It reuses an existing local
branch, commits only when something is staged, and force-pushes the branch of an open PR.
It never pushes to a branch whose PR is merged or closed, because that would resurrect a PR
someone finished or rejected.

`--apply` pushes the stored dry-run patch because re-running a fix can resolve differently the
second time. If the patch is stale, unreadable, or doesn't apply, repomod re-runs the fix.

## Repo sync

`sync.py` fetches the REST search pages in parallel. It then requests commit history in
batched GraphQL queries, only for repos the activity filter can't decide from `pushed_at` or
from the `last_human_commit` memo kept in `repos.yaml`. Repos with no non-bot default-branch
commit within `MAX_IDLE` (1 year) are dropped to cut PR noise. They are listed in
`repos-excluded.yaml`, which is checked in so you can audit the filter.

## Workflow validation

`lint.py` validates a fix's output before repomod shows or pushes a diff. It runs actionlint
on changed workflow files. If it finds errors, it lints the same files with the changes
stashed, and only errors the fix introduced fail the repo. A fix can add its own checks with `validate`. `preflight`
requires `actionlint` for every command that runs a fix.
