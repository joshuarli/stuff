# RPMD-74: Checkout removal contract

Status: draft

## Summary

Make ordinary checkout removal depend on the state of the whole Git repository, including
local refs and linked worktrees. Share the decision between archived sync and GC. Keep the
existing behavior of refusing uncertain deletion; do not add a database or a recovery daemon.

## Motivation

The current removal guard receives only the checked-out branch's `GitStatus` and a stash count.
A clean main does not prove that the object database is disposable. Real-Git reproductions
removed an unpublished feature commit and invalidated another registered worktree while the
current branch was clean and had no commits ahead of its upstream.

Canonical checkouts are user data. A repomod lock serializes cooperating commands but does not
exclude a user, editor, or another Git process. The design must state that boundary instead of
claiming that the lock makes an ordinary user's checkout immutable.

## Design

Keep Git inspection in `git.py`, the shared removal decision in `clean.py`, and its two callers
in `clean.py` and `sync.py`. Build one inspection result under the existing repository lock.
It must establish these facts before ordinary removal:

- The path and origin are verified under the existing rules.
- Tracked changes and all nonignored untracked files are absent, regardless of Git display settings.
- No stash or repomod recovery ref exists.
- Every retained local branch/ref has a verified published counterpart; unknown publication is a blocker.
- No additional registered worktree depends on the common Git directory, including a missing or locked worktree whose recovery status is unknown.

Use Git plumbing for ref reachability and `git worktree list --porcelain` for the worktree
registry. Do not infer publication from a branch name or the checked-out branch's ahead count.
If only stale remote-tracking information is available, refuse removal rather than inventing
remote durability. Report the specific blocking ref or worktree so the owner can resolve it.

Recheck relevant identities immediately before removal and fail if they changed. Document the
remaining limitation of concurrent non-cooperating filesystem/Git writers. The smallest repair
must at least close the demonstrated deterministic loss paths; it must not claim complete
concurrency exclusion that ordinary Git does not provide.

Preserve files, refs, stashes, and worktree indexes exactly on refusal. Archived sync must leave
the catalog entry managed when removal is refused, matching its current recovery behavior.
Dry-run uses the same inspection and reports the same blocker without deletion.

Acceptance uses disposable real Git repositories: a clean published branch, a separate
unpublished branch, a recovery ref, a stash, hidden untracked files, and clean/dirty registered
linked worktrees. Exercise both archived sync and unforced GC, and verify preserved commit
objects and usable linked-worktree metadata after every refusal.

## Alternatives considered

Current-branch status is cheap but cannot answer whether shared Git storage is disposable.
Automatically deleting other branches or worktrees destroys the state the guard must protect.
Moving everything into a new persistent quarantine adds storage ownership and lifecycle work;
it is not required to fix the proved cases. Leaving archived checkouts for explicit GC avoids
implicit deletion during synchronization and remains an owner policy option.

## Open questions

- Should archived sync continue deleting checkouts automatically? Keep the current behavior only after the stronger guard is implemented, or retain archived checkouts and leave removal to explicit GC. Recommendation: retain them if automatic deletion is not a required workflow.
- May `gc --force` invalidate other registered worktrees? Recommendation: keep shared-worktree and recovery-state blockers unconditional; require the owner to resolve those states first. This would tighten current force behavior and needs a deliberate decision.
- What counts as a verified published counterpart when remote state is unavailable? Recommendation: refuse automatic removal; a last-fetched tracking ref alone is not proof of the remote's present retention.

## Decision

Pending. No runtime behavior changes are part of this review. The existing safety contract and
all conservative refusal paths remain in force until a scoped implementation is accepted.
