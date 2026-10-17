Prefix: RPMD
Next: 32

### RPMD-8 · P2 · The dependency set is not pinned in git

- **Area:** Dependencies
- **Where:** `pyproject.toml:5`, `pyproject.toml:24`, `uv.lock` (untracked)
- **Problem:** `git ls-files` does not contain `uv.lock`, and `git status` shows it untracked. `uv run` on a fresh checkout resolves ranges again. The on-disk lock points every registry URL at a Sentry index that `pyproject.toml`, `.envrc`, and CI do not declare, so this file must not be committed as it stands. `joshuarli-pytest-parallel` is a git source with no `rev`. The lock records commit `d10c19e8dc9c2cbcc92429133312222a5678df25`, but that pin exists only in the untracked file. There is no publish workflow. `make test` passed here (165 tests), so the build is not broken on this machine. An operator cannot roll back to the dependency set that last ran.
- **Recommendation:** Pin `rev` on the git source. Regenerate `uv.lock` against the public index and commit it. Do not commit the Sentry-index lock.
- **Fix with:** none
- **Effort:** S

### RPMD-31 · P4 · An importlib import is outside the fix hash

- **Area:** Architecture
- **Where:** `src/repomod/main.py` `_imports_in`
- **Problem:** The fix hash follows `import` and `from` statements into the fix directory and the `repomod` package. `importlib.import_module("repomod.utils")` is a call, so that module is not hashed. No current fix loads a module that way. A later fix that does would keep `skipped` and `pr_created` rows current when the loaded module changes.
- **Recommendation:** When that call's argument is a string literal naming a module in the fix directory or the `repomod` package, hash it the same way as an import.
- **Fix with:** none
- **Effort:** S
