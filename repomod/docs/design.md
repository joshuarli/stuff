# Design

repomod uses one curated catalog, canonical local Git checkouts, read-only GitHub status, and
explicit repair and topic writes. Git records source changes; there is no status database or
repair ledger.

## State and ownership

`catalog.toml` owns repository identity, policy tags, checkout root, and autofix overrides.
`REPOMOD_ROOT` overrides the configured root for one process. Tags are defined in
[tagging semantics](tagging.md). The catalog does not record branch state or lint results.

Personal-owner checkouts live directly under the root; other owners use `OWNER/REPO`. The
catalog rejects symlink path components, and repomod never repoints a checkout with a different
GitHub origin. New clones are staged beside their destination and moved into place only after
their origin is checked.

`git.py` owns Git subprocesses and per-repository locking. `github.py` owns GitHub reads, CI
cache, topic operations, and approved Action pin resolution. `sync.py` coordinates clones,
fetches, and safe fast-forwards. Policy rules live under `rules/`; their authoring contract is
in [policy development](policy-development.md).

## Synchronization and status

Sync reads current GitHub metadata for the catalog. It skips fetch only when the checkout is
clean, attached to the default branch, and its HEAD matches GitHub's reported branch tip. Other
valid checkouts fetch that branch; repomod fast-forwards only a clean checkout on it when Git
confirms the move is safe. Dirty, detached, non-default, ahead, and divergent checkouts keep
their files and local commits. A fetch can update the matching remote-tracking ref. Dry-run
checks local checkout state without GitHub calls.

Local status does not fetch. Ahead and behind counts compare the local branch with its
last-fetched upstream, so they are not current remote state. Online status checks CI for the
current GitHub default-branch commit. The CI cache is keyed by repository and commit SHA and
expires after five minutes. Offline status reads cached data without network calls.

## Repair and writes

Fix requires a clean checkout on the current default branch. Each fixer runs in a detached
worktree at the starting commit and can change only its declared paths. Validation and policy
checks must pass before each changed fixer gets one local commit. repomod updates the canonical
checkout only if it remains clean at its starting commit; otherwise it retains successful work
under a recovery ref or worktree and reports the recovery path.

Push uses a normal fast-forward Git push and leaves local commits intact on failure. `topics
apply` replaces GitHub topics with the catalog's complete tag set. `pins refresh` reads upstream
refs and updates the local approved-pin file.
