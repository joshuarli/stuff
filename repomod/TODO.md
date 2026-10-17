Prefix: RPMD
Next: 31

### RPMD-8 · P2 · The dependency set is not pinned in git

- **Area:** Dependencies
- **Where:** `pyproject.toml:5`, `pyproject.toml:24`, `uv.lock` (untracked)
- **Problem:** `git ls-files` does not contain `uv.lock`, and `git status` shows it untracked. `uv run` on a fresh checkout resolves ranges again. The on-disk lock points every registry URL at a Sentry index that `pyproject.toml`, `.envrc`, and CI do not declare, so this file must not be committed as it stands. `joshuarli-pytest-parallel` is a git source with no `rev`. The lock records commit `d10c19e8dc9c2cbcc92429133312222a5678df25`, but that pin exists only in the untracked file. There is no publish workflow. `make test` passed here (165 tests), so the build is not broken on this machine. An operator cannot roll back to the dependency set that last ran.
- **Recommendation:** Pin `rev` on the git source. Regenerate `uv.lock` against the public index and commit it. Do not commit the Sentry-index lock.
- **Fix with:** none
- **Effort:** S

### RPMD-30 · P3 · Checkout, stage, and a clone OSError still abort the run

- **Area:** Errors
- **Where:** `src/repomod/main.py:357`, `src/repomod/main.py:358`, `src/repomod/main.py:469`
- **Problem:** `has_changes`, `unstage`, a reused patch's checkout and stage, and an `OSError` from `commit_and_push` are caught and the run continues. `checkout_branch` when `--apply` is set, and `stage_worktree` before `has_changes`, are still outside that handler. `clone` catches `SUBPROCESS_ERRORS` and not `OSError`. Those errors escape `process_repo`. With one worker the rest of the run is neither finished nor marked interrupted. Walked from the source after RPMD-26.
- **Recommendation:** Catch `DIFF_SAVE_ERRORS` on those calls the same way as `has_changes`. Keep an existing `pending` row. Let `ShutdownError` propagate, and do not `pd.finish` for it.
- **Fix with:** error-model
- **Effort:** S

