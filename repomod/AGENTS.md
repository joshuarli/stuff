# Working in repomod

repomod manages canonical local repositories from `catalog.toml`. Read the relevant guide before
changing [design](docs/design.md), [tagging](docs/tagging.md), or [policy development](docs/policy-development.md).

## Build and verify

```sh
uv sync
make test
uv run ruff check src tests
uv run ty check
```

Tests use local Git repositories and fake GitHub responses. Use copied catalogs for mutation
tests. Set `TMPDIR` to `.work/tmp` for commands that create scratch data.

## Constraints

- `catalog.toml` is curated user data. Read it before catalog work and preserve its comments and repository selection.
- Do not run `sync`, `fix`, `push`, `topics apply`, or `pins refresh` against the live catalog while changing code.
- Canonical checkouts are user data. Never reset, stash, clean, switch, or discard changes in them.
- Sync may fetch and only fast-forward clean checkouts on their default branch. Fixers use detached worktrees and publish only from an unchanged canonical checkout.
- Remote writes require `topics apply`, `push`, or `fix --push`. `pins refresh` writes the local approved-pin file.
- Keep Git subprocesses, GitHub operations, synchronization, and policy checks in their owning modules under `src/repomod/`.
- Keep rules read-only and fixers as explicit, validated file plans. Do not add databases, background workers, or generic workflow frameworks.
- Do not add pull-request creation, rollout tracking, patch storage, or per-fixer clone directories.
