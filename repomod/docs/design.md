# Design

repomod uses one curated catalog, canonical local Git checkouts, read-only GitHub status, and
explicit repair and topic writes. Git records source changes. repomod has no status database or
repair ledger.

## State and ownership

`catalog.toml` owns repository identity, policy tags, checkout root, and autofix overrides.
`REPOMOD_ROOT` overrides the configured root for one process. The catalog does not record branch
state or lint results.

Personal-owner checkouts live directly under the root. Other owners use `OWNER/REPO`. The catalog
rejects symlink path components. repomod checks that a checkout's origin matches its GitHub
repository. New clones are staged beside their destination and moved into place after validation.

`git.py` owns Git subprocesses and repository locks. `github.py` owns GitHub reads, CI cache,
topics, and approved Action tag resolution. `sync.py` coordinates clones, fetches, and safe
fast-forwards. Policy rules live under `rules/`. See [policy development](policy-development.md).

## Synchronization and status

Sync reads current GitHub metadata for catalog entries. It skips fetch only when a clean checkout
is attached to the default branch and its HEAD matches GitHub's reported branch tip. Otherwise it
fetches that branch. It fast-forwards only a clean checkout on that branch when Git confirms the
move is safe. Dirty, detached, non-default, ahead, and divergent checkouts keep their files and
local commits. A fetch may update the matching remote-tracking ref. Dry-run checks local state
without GitHub calls.

Local status does not fetch. Ahead and behind counts compare the local branch with its last-fetched
upstream, so they are not current remote state. Online status checks CI for GitHub's current
default-branch commit. The CI cache is keyed by repository and commit SHA and expires after five
minutes. Offline status reads cached CI without network calls.

## Repair and writes

Fix requires a clean checkout attached to the default branch. Each fixer runs in a detached
worktree at the starting commit and can change only its declared paths. Before a local commit,
validation must pass, the fixer must leave no fixable findings for its rule, and it must introduce
no policy errors. repomod updates the canonical checkout only if it remains clean at its starting
commit. Otherwise it preserves successful work under a recovery ref or worktree and reports how
to recover it.

Push uses a normal fast-forward Git push and keeps local commits on failure. `topics apply`
replaces GitHub topics with the catalog's complete tag set. `pins refresh` reads upstream refs
and updates the local approved-pin file.
