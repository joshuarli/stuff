# Design

repomod pushes to hundreds of repos, so reliability, resumability, and idempotency are
non-negotiable. This page explains how the code keeps them. For how re-runs behave from the
user's side, see [Re-running a fix](../README.md#re-running-a-fix).

## Reliability

- Every subprocess call has a 120-second timeout (`SUBPROCESS_TIMEOUT` in `utils.py`).
- Network calls in `git.py` and `sync.py` use `@retry`: 3 attempts with exponential backoff.
- `github_rate_limiter` spaces `gh_run` by 0.25 seconds and holds its lock for the whole `gh` call, so those processes do not overlap. `git_transport_limiter` spaces starts of `clone`, `fetch`, `push`, and `git ls-remote` by the same 0.25 seconds, then releases its lock before the git command, so those commands can overlap.
- `pr_create_limiter` spaces PR creation and marking ready by 2 seconds. GitHub's secondary
  rate limits on creating content are much tighter than on reads.
- One repo failing doesn't stop the run.
- `atomic_write_yaml` writes status files to a temp file and renames it into place.

## API budget

Keep GitHub API calls minimal. Batch them or look them up once per run, not once per repo.

`--apply` searches for existing PRs on the fix branch once before the fixes, then once more
after every fix and before any push. The second snapshot overrides the first per repo. A
repo only the first search saw keeps that PR, and a repo in neither is still pushed. It
then makes at most one `gh pr create` per new repo, which also sets reviewers and returns
the URL. Repos that only need a push, or whose PR is already merged or closed, cost no
API calls.
`tests/test_apply_gh_calls.py` asserts the exact calls. Keep it passing when you touch the PR
path.

## Node ids

A PR's GraphQL node id never changes, so `status.yaml` caches it as `pr_node_id`. It comes
free from the search for existing PRs. New PRs get one batched lookup after `--apply`. Draft
and merged state can change, so repomod never caches them.

## Resumability

`^C` raises `ShutdownError` through `interrupt.wait()`, which uses `Event.wait()` so sleeping
threads wake instantly. The handler only sets that event. The worker loop prints
`Gracefully exiting, please wait...` once when it notices. A second `^C` restores the
default SIGINT action and re-raises it, which terminates the process. In-flight workers
finish their current network call. Incomplete repos are marked `interrupted`, and the next
run picks them up.

Interrupted repos must not increment done counters, so never call `pd.finish()` for them.
`execute_fix_on_repo` writes the status before `pd.finish()`, so a `^C` between the two can't
overwrite a recorded result with `interrupted`.

## Idempotency

`compute_fix_hash` hashes `main.py` plus the parsed config without `skip`.
`legacy_fix_hash`, the raw bytes of both files, is still accepted so older `status.yaml`
entries stay current.

`commit_and_push` is safe to re-run after a partial failure. It reuses an existing local
branch, commits only when something is staged, and force-pushes the branch of an open PR
([RPMD-24](rfc/RPMD-24-force-push.md)).
It never pushes to a branch whose PR is merged or closed, because that would resurrect a PR
someone finished or rejected. It also refuses, before checkout or commit, when the fix
branch is exactly the repo's default branch (`Main` is not `main`): force-pushing that
name would rewrite it. That repo is recorded `failed` and the rest of the run continues.

`--apply` pushes the stored patch because a second fix run can resolve differently.
The merged-or-closed check is the opening search overlaid with that second search, which
returns at most 1000 hits. A hit in the second search wins. A PR that changes state after
the refresh returns and before `git push` can still be force-pushed.

## Repo sync

`sync.py` fetches the REST search pages in parallel. It then requests commit history in
batched GraphQL queries, only for repos the activity filter can't decide from `pushed_at` or
from the `last_human_commit` memo kept in `repos.yaml`. Repos with no non-bot default-branch
commit within `MAX_IDLE` (1 year) are dropped to cut PR noise. They are listed in
`repos-excluded.yaml`, which is checked in so you can audit the filter.

## Workflow validation

`lint.py` runs actionlint on changed workflow files before a diff is shown or pushed.
It stashes the edits to measure the baseline, and only findings the fix introduced fail
the repo. `preflight` requires `actionlint` for every command that runs a fix.
