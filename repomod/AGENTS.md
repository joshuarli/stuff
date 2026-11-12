# Working in repomod

repomod manages canonical local repositories from `catalog.toml`.

| Topic | Guide |
| --- | --- |
| Architecture, sync, repair, and state ownership | [Design](docs/design.md) |
| Catalog tags and visibility | [Tagging](docs/tagging.md) |
| Rules, fixers, and musl publication | [Policy development](docs/policy-development.md) |
| Dependency policy checks | [Audit](docs/audit.md) |
| Tracked dependency model and extraction | [Inventory](docs/dependency-inventory.md) |
| Dashboard, browser checks, and benchmarks | [Dashboard](docs/dependency-dashboard.md) |
| Deferred fixes | [TODO](TODO.md) |
| Checkout removal contract | [Draft design](docs/rfc/RPMD-74-checkout-removal.md) |
| Remaining musl rollout decisions | [Draft rollout](docs/rfc/RPMD-85-musl-rollout.md) |

## Before work

Run `uv run repomod sync` before any work starts, so canonical checkouts reflect their remotes.

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
- Do not run `fix` against the live catalog while changing code.
- Canonical checkouts are user data. Never reset, stash, clean, switch, or discard changes in them.
- Sync may fetch and only fast-forward clean checkouts on their default branch. Fixers use detached worktrees and publish only from an unchanged canonical checkout.
- Remote-write commands are `topics apply`, `push`, `fix`, `merge`, and `release`; `fix` pushes branches and opens pull requests. `pins refresh` writes local approved pins.
- Keep Git subprocesses, GitHub operations, synchronization, and policy checks in their owning modules under `src/repomod/`.
- Keep rules read-only and fixers as explicit, validated file plans. Do not add databases, background workers, or generic workflow frameworks.
- Keep pull-request publication in the fix flow. Do not add rollout tracking, patch storage, or per-fixer clone directories.
