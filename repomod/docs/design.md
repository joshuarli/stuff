# Design

repomod uses one curated catalog, canonical local Git checkouts, read-only GitHub status, and
explicit repair and topic writes. Git records source changes. repomod has no status database or
repair ledger.

## State and ownership

`catalog.toml` owns repository identity, policy tags, checkout root, autofix overrides, and
per-repository ignored rule codes. Version 2 adds the `ignore` override for stable lint IDs.
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
without GitHub calls. For repositories that GitHub reports as forks of a parent, sync also
ensures a conventional `upstream` remote pointing at the parent and fetches the parent's default
branch, so fork-behind comparisons stay verifiable locally; a failing upstream fetch is reported
in the result without failing the sync.

Local status does not fetch. Ahead and behind counts compare the local branch with its last-fetched
upstream, so they are not current remote state. Online status checks CI for GitHub's current
default-branch commit. The CI cache is keyed by repository and commit SHA and expires after five
minutes. Offline status reads cached CI without network calls. For repositories that GitHub reports
as forks, online status also compares the fork default branch against its upstream parent and
reports behind and ahead counts. The upstream cache is keyed by repository and fork commit SHA and
expires after one hour. Offline status reads cached upstream comparisons. Online status also lists open
pull requests with their mergeable and merge-state status; `--failed` includes repositories
with conflicting PRs and forks behind their upstream. Offline status omits pull requests.

## Repair and writes

Fix requires a clean checkout attached to the default branch. Fixers run serially in a dedicated
branch per repository, with repositories processed in parallel. Each fixer uses a disposable
worktree at the current branch tip and can change only its declared paths. Before a local commit,
validation must pass, the fixer must leave no fixable findings for its rule, and it must introduce
no policy errors. Every successful fixer gets its own commit on a new local
`repomod/fix/<id>` branch; the canonical default branch remains untouched. `fix --push` pushes
that branch and opens or reuses a pull request against the default branch. It enables GitHub
auto-merge only when the repository allows it and the default branch requires status checks. The
next fix run resumes its unmerged branch from the current default branch. Failed or interrupted
work remains on its branch or in its worktree.

Push uses a normal fast-forward Git push and keeps local commits on failure. `topics apply`
replaces GitHub topics with the catalog's complete tag set. `pins refresh` updates the local
approved GitHub Action, Python tool, and TypeScript tool versions from their upstream sources.
