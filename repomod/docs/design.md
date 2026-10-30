# Design

repomod uses one curated catalog, canonical local Git checkouts, read-only GitHub status, and
explicit repair and topic writes. Git records source changes. repomod has no status database or
repair ledger.

## State and ownership

`catalog.toml` owns repository identity, policy tags, checkout root, autofix overrides, and
per-repository ignored rule codes. Version 2 adds the `ignore` override for stable lint IDs and `mdtool:CODE` documentation findings.
`REPOMOD_ROOT` overrides the configured root for one process. The catalog does not record branch
state or lint results.

Personal-owner checkouts live directly under the root. Other owners use `OWNER/REPO`. The catalog
rejects symlink path components. repomod checks that a checkout's origin matches its GitHub
repository. New clones are staged beside their destination and moved into place after validation.

`git.py` owns Git subprocesses and repository locks. `github.py` owns GitHub reads, CI cache,
topics, and approved Action tag resolution. `sync.py` coordinates clones, fetches, safe
fast-forwards, and archived-repository untracking. `clean.py` owns checkout removal guards and
unreferenced-checkout collection. `catalog.py` owns catalog reads and edits. Command handlers live
under `cmd/`, one module per subcommand; `cli.py` only parses arguments and dispatches.
Policy rules live under `rules/`. See [policy development](policy-development.md).

## Synchronization and status

Sync reads current GitHub metadata for catalog entries. New clones are full-history, blobless
partial clones: complete commit history so rebases and ancestry queries work, with file contents
fetched on demand instead of up front. A checkout that is still shallow from an older clone is
unshallowed during sync; if that fails the sync for that repository fails loudly rather than
continuing on a shallow boundary. Sync skips fetch only when a clean checkout
is attached to the default branch and its HEAD matches GitHub's reported branch tip. Otherwise it
fetches that branch. It fast-forwards only a clean checkout on that branch when Git confirms the
move is safe. Dirty, detached, non-default, ahead, and divergent checkouts keep their files and
local commits. A fetch may update the matching remote-tracking ref. GitHub is the source of truth
for archival: when GitHub reports a catalogued repository as archived, sync removes its local
checkout instead of cloning or fetching it and comments its entry out of `catalog.toml`, so later
runs stop tracking it. Removal and untracking require a verified checkout whose origin
matches the catalogued repository and refuse dirty, detached, ahead, stashed, or otherwise
unverifiable checkouts without deleting anything; a checkout that cannot be safely removed stays
tracked. Dry-run fetches GitHub metadata but does not
mutate checkouts; it reports what would be cloned, updated, removed, or untracked. For repositories that GitHub reports as forks of a parent, sync also
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
`repomod/fix/<id>` branch; the canonical default branch remains untouched. `fix` pushes
that branch and opens or reuses a pull request against the default branch. It enables GitHub
auto-merge only when the repository allows it and the default branch requires status checks. The
next fix run resumes its unmerged branch from the current default branch. Failed or interrupted
work remains on its branch or in its worktree.

Push uses a normal fast-forward Git push and keeps local commits on failure. `push` pushes
each selected checkout's current branch when it is ahead of its origin upstream and skips
anything else with a reason. `topics apply`
replaces GitHub topics with the catalog's complete tag set. `pins refresh` updates the local
approved GitHub Action, Python tool, and TypeScript tool versions from their upstream sources.
