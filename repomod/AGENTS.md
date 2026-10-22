# Working in repomod

repomod manages canonical local repositories from `catalog.toml`. Git records source changes;
rules lint and repair without a status database. Read the relevant topic guide before changing
sync, tagging, or policy behavior:

- [Design](docs/design.md): state, synchronization, status, and repair behavior.
- [Tagging](docs/tagging.md): catalog tag meanings and selection rules.
- [Policy development](docs/policy-development.md): rule and fixer contracts.

## Verify changes

```sh
uv sync
make test
uv run ruff check src tests
uv run ty check
```

Tests use local Git repositories and fake GitHub responses. Use copied catalogs, local bare
repositories, and fake API responses for mutation tests. Set `TMPDIR` to `.work/tmp` for
commands that create scratch data. Do not run `sync`, `fix`, `push`, `topics apply`, or
`pins refresh` against the real catalog while changing code.

## Protect local data

`catalog.toml` is curated user configuration. Read it before catalog work; preserve its comments
and repository selection, and run catalog-editing tests against a copy. Never reset, stash,
clean, switch, or discard changes in a canonical checkout. Sync may fetch and update a
remote-tracking ref. Fixers use a disposable detached worktree and update the canonical checkout
only when it still matches its starting state.

Remote writes require explicit commands: `topics apply`, `push`, or `fix --push`. `pins refresh`
updates the local approved-pin file. Do not remove existing checkouts, patches, logs, or
generated files while changing repomod; they are user data. Do not add PR creation, rollout
tracking, patch storage, or per-fixer clone directories.

## Code conventions

Keep Git subprocesses in `git.py`, GitHub operations in `github.py`, synchronization in `sync.py`,
and policy checks in `rules/`. Use Python 3.14 standard-library APIs where they fit. Keep rules
as ordinary Python definitions. Do not add databases, background workers, generic workflow
frameworks, or code comments.
