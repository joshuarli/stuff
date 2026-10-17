Prefix: RPMD
Next: 25

### RPMD-8 · P2 · The dependency set is not pinned in git

- **Area:** Dependencies
- **Where:** `pyproject.toml:5`, `pyproject.toml:24`, `uv.lock` (untracked)
- **Problem:** `git ls-files` does not contain `uv.lock`, and `git status` shows it untracked. `uv run` on a fresh checkout resolves ranges again. The on-disk lock points every registry URL at a Sentry index that `pyproject.toml`, `.envrc`, and CI do not declare, so this file must not be committed as it stands. `joshuarli-pytest-parallel` is a git source with no `rev`. The lock records commit `d10c19e8dc9c2cbcc92429133312222a5678df25`, but that pin exists only in the untracked file. There is no publish workflow. `make test` passed here (165 tests), so the build is not broken on this machine. An operator cannot roll back to the dependency set that last ran.
- **Recommendation:** Pin `rev` on the git source. Regenerate `uv.lock` against the public index and commit it. Do not commit the Sentry-index lock.
- **Fix with:** none
- **Effort:** S

### RPMD-15 · P3 · The legacy fix hash is not the hash the test uses

- **Area:** Tests
- **Where:** `tests/test_main.py:967`, `src/repomod/main.py:99`, `src/repomod/main.py:816`
- **Problem:** `test_results_recorded_under_the_old_style_hash_still_count_as_current` passes `accepted_hashes` that contain the literal `"legacy-hash"`. It never calls `legacy_fix_hash`, and it never goes through `main`, which is where that hash is unioned with the current one. A wrong byte range in `legacy_fix_hash`, or dropping it at the `main` call site, leaves existing `status.yaml` rows stale and this test stays green. Skip-list invariance of the new hash is tested.
- **Recommendation:** Write a status row whose `fix_hash` is `legacy_fix_hash(fix_dir)` of the real files and run `main`. That repo must not be redone. A byte change to `main.py` must.
- **Fix with:** test-audit
- **Effort:** S

### RPMD-16 · P3 · The fix hash does not cover modules a fix imports

- **Area:** Architecture
- **Where:** `src/repomod/main.py:113`, `src/repomod/fixes/pin-gha/main.py:9`, `src/repomod/fixes/ubuntu-24/main.py:1`
- **Problem:** `compute_fix_hash` hashes that fix's `main.py` bytes plus the parsed config without `skip`. `pin-gha` imports `repomod.utils`. A change to retry or the rate limiter changes that fix's behavior and does not change the hash, so repos already marked `skipped` or `pr_created` are not run again. `macos-15` and `ubuntu-24` are copied apart for this reason. Nothing checks the imports. The self-contained sentence in the ubuntu module docstring was deleted. It was not enforced.
- **Recommendation:** When loading a fix, reject an import of `repomod`, or add each imported module's bytes to the hash.
- **Fix with:** comment-cleanup
- **Effort:** M

### RPMD-17 · P3 · The GitHub rate limiter also serializes git clones

- **Area:** Performance
- **Where:** `src/repomod/utils.py:134`, `src/repomod/git.py:39`, `src/repomod/git.py:74`
- **Problem:** `RateLimiter.__enter__` holds `_lock` until the `with` body returns. `github_rate_limiter` wraps `git clone`, `git fetch`, `git push`, every `gh_run`, and `pin-gha`'s `git ls-remote`. `--workers` can be 8, and those commands still run one at a time. The 0.25s gap is only the pause between them. Magnitude unproven. Not measured. `docs/design.md` now says the lock is held for the whole command.
- **Recommendation:** Keep the spacing. Do not drop the lock around `gh`. Use a separate limiter for git transport so clones can overlap, and leave `pr_create_limiter` at 2 seconds.
- **Fix with:** make-it-fast
- **Effort:** M

### RPMD-24 · P3 · Force-push replaces commits added on an open fix branch

- **Area:** Data
- **Where:** `src/repomod/git.py:61`, `src/repomod/git.py:195`, `README.md:75`
- **Problem:** `clone` fetches only the default branch and deletes every other local branch. The fix branch is recreated from that tip and pushed with `--force`. Nothing fetches the remote fix branch or uses `--force-with-lease`. A later `--apply` that is not skipped replaces reviewer commits on the open PR. The README now states that. Keeping the force-push is what makes a re-run idempotent. Switching to a lease changes that contract.
- **Recommendation:** Keep the force-push unless you want reviewer commits to block the next apply. The choice is in the RFC.
- **Fix with:** durable-state
- **Effort:** M
- **Design:** [docs/rfc/RPMD-24-force-push.md](docs/rfc/RPMD-24-force-push.md)

### RPMD-18 · P4 · PR labels are accepted and never set

- **Area:** Dead code
- **Where:** `src/repomod/types.py:77`, `src/repomod/git.py:206`
- **Problem:** `FixConfig` has no labels field. `pr_spec` leaves `PRSpec.labels` empty. `push_and_open_pr` still builds `--label` arguments. No config, status row, or caller supplies them.
- **Recommendation:** Delete `labels` and the `--label` arguments.
- **Fix with:** dead-code-archaeology
- **Effort:** S
