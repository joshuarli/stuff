# Design

repomod has one catalog, local Git checkouts, read-only GitHub status, declarative policy, and
explicit repair and topic writes. Git records every source change. No database or run ledger
tracks completed repairs.

## State and ownership

`catalog.toml` owns repository identity, curated role/language/publishing tags, checkout
root, and narrow autofix overrides. Tags are defined in `docs/tagging.md`. It does not
record branch state or previous lint results. `REPOMOD_ROOT` overrides
the catalog's configured root for that process.

Canonical checkout paths flatten the personal owner and retain an owner directory for other
owners. The loader validates identifiers, checkout collisions, and resolved paths. Checkout
path components that are symlinks are rejected. An existing checkout with a different
GitHub origin is never repointed.

`discover` is read-only and does not define ownership. `add` changes the catalog. `sync` acts
on catalogued repositories after checking current GitHub metadata. An archived or missing
repository is never cloned. A clone starts in a sibling staging directory and moves into its
canonical path only after its origin is validated.

## Synchronization and status

`git.py` owns every Git subprocess. Sync fetches one remote default branch without tags. It
fast-forwards only a clean checkout already on that branch and only when Git proves ancestry.
Shallow histories deepen in bounded steps when ancestry is uncertain. A checkout that is
dirty, detached, on another branch, ahead, or divergent keeps its checked-out files. Fetch can
update the matching remote-tracking ref.

Local status reads porcelain v2 with NUL delimiters and does not fetch. Ahead and behind
counts compare the local branch with its last-fetched upstream. They are not current remote
GitHub state.

GitHub metadata queries return the actual default branch and current HEAD SHA in batches.
Status queries `Commit.statusCheckRollup` for that exact SHA and follows every context page.
Incomplete results are unknown. An empty rollup is no checks. A cache entry is valid only for
its repository and SHA, and expires after five minutes. Offline status reads the cache without
calling `gh`.

## Policy

Rules are ordinary functions registered in `rules/__init__.py`. Each rule has a stable ID,
description, required tags, pure check, optional file-plan fixer, commit subject, and allowed
path patterns. Global rules have no required tags. Multiple `--tag` filters select a union of
rules and repositories.

Finding order is stable. A finding names the repository, rule, file, message, severity, and
whether the deterministic part can be repaired. Rules do not run project builds or install
dependencies. `status` skips external checks and topic reads. Full `lint` reports unavailable
required tools as operational failures.

GitHub Actions use approved full commit SHAs from `actions.toml`. Version channels stay on
their current major unless `pins refresh --allow-major` is explicit. Lint compares local files
to this policy without resolving remote tags. Workflow fixes fail unless `actionlint` validates
the modified files.

`release-policy.toml` records the ecosystem contracts used by release rules. A deterministic
metadata omission can be repaired. A dynamic version source, conflicting package-manager
metadata, or ambiguous release path needs a human decision and is reported without rewriting
release configuration.

## Repair transaction

Fix runs only on a clean canonical checkout at the current remote default branch. A per-repo
lock serializes repomod sync, fix, and push operations. Each run creates one detached worktree
at the canonical checkout's exact starting HEAD.

Fixers run sequentially. A fixer returns the complete file contents it intends to change.
repomod rejects paths outside its declared scope and rejects symlinks along a changed path.
Validation and a post-check must pass before the fixer gets a commit. Each fixer gets one
commit with its own subject. A no-op gets no commit. A failed fixer resets only its disposable
worktree to the previous successful commit, then later rules continue.

After all rules run, repomod fast-forwards the canonical checkout only if it remains clean,
attached to the expected branch, and at its starting HEAD. If the checkout changed, repomod
leaves it alone and retains the successful commits under a recovery ref. Worktrees are removed
on normal completion. If removal fails, repomod reports the path.

Push checks the last-fetched upstream and then lets Git enforce a normal fast-forward update.
A race with a remote update or branch protection leaves local commits untouched and reports
the push failure.

## GitHub writes

Only `topics apply`, `pins refresh`, and explicit push commands write outside the local
checkout. `topics apply` replaces the complete topic set with catalog tags. `pins refresh`
only reads upstream refs and writes the local approved-pin file. `fix --push` runs the same
non-force push used by `repomod push` after local commits are published safely.
