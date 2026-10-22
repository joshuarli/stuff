# Working in repomod

repomod manages canonical local repositories from `catalog.toml`. Git is the record of source
changes. Rules lint and repair without a status database. A successful fixer gets one local
commit. Remote writes require an explicit topic or push command.

## Verify changes

```sh
uv sync
make test
uv run ruff check src tests
uv run ty check
```

Tests use local Git repositories and fake GitHub responses. Do not run `sync`, `fix`, `push`,
`topics apply`, or `pins refresh` against a real catalog while changing code. Use temporary
catalogs, local bare repositories, and fake API responses. Point `TMPDIR` at `.work/tmp` for
commands that create scratch data.

## Protect local data

`catalog.toml` is curated user configuration. Read it before catalog work, preserve its
comments and repository selection, and do not add inferred repositories or tags. Run tests
that edit a copied catalog. The default checkout root is `~/repos`; `REPOMOD_ROOT` overrides
it. Do not remove existing checkout, patch, log, or generated files during this replacement.

Never reset, stash, clean, switch, or discard changes in a canonical checkout. Sync may fetch
and update a remote-tracking ref. Fixers use a disposable detached worktree and publish
successful commits only when the canonical checkout still matches its starting state.

## Code boundaries

- `catalog.py` parses and validates catalog configuration, tags, and checkout paths.
- `git.py` owns every Git subprocess and the per-repository lock.
- `github.py` owns GitHub reads, CI cache, topics, and approved Action tag resolution.
- `policy.py` selects rules and orders structured findings.
- `rules/` contains read-only checks and deterministic file-plan fixers.
- `fix.py` runs each fixer in isolation, validates its scope, and creates one commit per fixer.
- `sync.py` clones missing catalog entries and safely fast-forwards existing checkouts.
- `cli.py` parses arguments, filters repositories, and prints results.
- `types.py` holds shared domain types.

Use Python 3.14 standard-library APIs where they fit. Keep rules as ordinary Python
definitions. Do not add databases, background workers, compatibility readers, or generic
workflow frameworks.

## Policy rules

Give each rule a stable ID and a read-only check. Findings include the repository, rule, file,
message, severity, and fixability. A fixer returns explicit repository-relative file contents,
declares its allowed paths, validates the result, and uses a rule-specific Conventional Commit
subject. Do not write placeholder README, license, or agent files.

Global rules have no required tags. Semantic catalog tags activate reusable rules. Language
and package manager detection comes from repository files. Publishing policy requires an
explicit `crates-io`, `pypi`, or `npm` tag.

## GitHub operations

Batch metadata and CI requests. CI must match the current remote default-branch SHA. Unknown,
stale, failed, and no-check states remain distinct. `status --offline` makes zero network
calls. `topics diff` reads metadata. Only `topics apply` replaces the topic set.

Never add pull-request creation, draft state, rollout tracking, patch storage, or per-fix clone
directories. Existing files from the retired implementation remain user data and are not read
or deleted.
