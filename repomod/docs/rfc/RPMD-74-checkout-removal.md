# RPMD-74: Checkout removal contract

Status: implemented

## Decision

Archived sync continues to remove disposable checkouts automatically. GC uses the same
repository-wide inspection. Refusal preserves the checkout and keeps an archived repository
managed in the catalog. Dry-run performs the same inspection without deleting anything.

Ordinary removal requires all of the following:

- The existing path and origin guards pass.
- The checkout has no tracked changes, nonignored untracked files, detached HEAD, unknown
  upstream, commits ahead of its upstream, or stashes.
- Every local branch and tag has a matching ref currently advertised by origin. A local branch
  may also be an ancestor of that advertised branch when the remote object is available locally.
- Every remote-tracking tip is currently advertised by a configured remote or is an ancestor
  of an advertised branch whose object is available locally.
- No recovery or other unrecognized ref exists.
- No other registered worktree depends on the Git directory. Missing and locked worktrees
  still count. A checkout using a shared `.git` file is also retained.

`gc --force` overrides dirty-checkout, current-branch, stash, and publication checks and permits
stray-file removal. It cannot override shared-storage, registered-worktree, recovery-ref,
symlink, path, or catalogued-origin protection. Resolve those blockers explicitly first.

## Inspection and concurrency

`git.py` owns Git inspection and live remote queries. `clean.py` owns the shared removal
decision used by GC and archived sync. Remote errors and unknown publication state refuse
ordinary removal. Stale remote-tracking refs alone are insufficient evidence of publication.
Git status explicitly includes all nonignored untracked files regardless of display settings.

The inspection runs under repomod's repository lock. Immediately before deletion, it rechecks
checkout and Git-directory identities, HEAD, refs, origin, registered worktrees, and status.
A change refuses removal. The lock excludes cooperating repomod commands only. Git writers
and filesystem writers that ignore the lock can still race after the final check. Remote refs
can also change after advertisement. The inspection does not claim atomic exclusion of those
writers or permanent remote retention.

## Verification

Disposable real-Git tests exercise both archived sync and GC, including dry-run. They cover
published checkouts, unpublished branches and tags, recovery refs, stashes, hidden untracked
files, inaccessible remotes, deleted remote refs, and clean, dirty, or missing linked worktrees.
Refusal tests verify that refs, commit objects, local files, and linked-worktree metadata survive.
Force tests verify unconditional recovery and shared-storage protection.
