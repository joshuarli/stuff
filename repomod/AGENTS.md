# AGENTS.md

repomod mass-applies fixes across GitHub repos and opens draft PRs. It pushes to many repos,
so reliability, resumability, and idempotency are non-negotiable.

```
uv sync
make test                       # all tests offline, network mocked
uv run ruff check src tests
uv run ty check
```

## Where to look

- Usage, outputs, re-run behavior, and the fix interface: [README.md](README.md)
- Reliability, API budget, interrupts, idempotency, and sync internals: [docs/design.md](docs/design.md)
- Open decisions: [docs/rfc/](docs/rfc/)
- Adding a fix: [.claude/skills/new-fix/SKILL.md](.claude/skills/new-fix/SKILL.md)

## Where things belong

- `types.py`: every dataclass, NamedTuple, and type alias shared across modules. Never define types in the module that uses them.
- `main.py`: CLI, orchestration, and the main loop. No data types.
- `git.py`: all git subprocess calls. Nothing else shells out to git.
- `status.py`: all `status.yaml` I/O and logging. It owns `atomic_write_yaml`.
- `ui.py`: all terminal output. No business logic.
- `utils.py`: core infra (`interrupt`, `retry`, `RateLimiter`, `ShutdownError`). Not a dumping ground.
- `lint.py`: validation of a fix's output before any diff is shown or pushed.
- `sync.py`: the GitHub repo sync that writes `repos.yaml`.
- `RunContext` groups state shared across worker threads. Don't pass 10 positional args.

## Rules

- Batch GitHub API calls or make them once per run, not once per repo. `tests/test_apply_gh_calls.py` asserts the `--apply` calls.
- Put `except ShutdownError: raise` before any broad `except`. A broad handler that swallows the shutdown signal is the most common interrupt bug.
- Never call `pd.finish()` for an interrupted repo.
- The only runtime dependency is `pyyaml`. Shell out to `gh` and `git`. Discuss before you add a dependency.
- Write comments that explain why, not what. Don't remove useful comments during refactors.
- Prefer pure functions. `filter_work` filters and returns, with no side effects.
- No lazy imports, no `nonlocal`, no section-divider comments, no dead code, and no `\033[B` for cursor movement.
- Tests mirror `src` (`test_main.py` tests `main.py`) and use realistic names (`acme/backend`, not `"a"`).
- Never hand-edit `repos.yaml`, `status.yaml`, `logs-*/`, `patches-*/`, or `repos/`. They are generated, and patches must stay byte-exact, trailing whitespace included. `repos-disabled.yaml` is the hand-maintained denylist. Sync does not write it.
