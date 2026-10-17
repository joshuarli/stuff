Prefix: RPMD
Next: 30

### RPMD-8 · P2 · The dependency set is not pinned in git

- **Area:** Dependencies
- **Where:** `pyproject.toml:5`, `pyproject.toml:24`, `uv.lock` (untracked)
- **Problem:** `git ls-files` does not contain `uv.lock`, and `git status` shows it untracked. `uv run` on a fresh checkout resolves ranges again. The on-disk lock points every registry URL at a Sentry index that `pyproject.toml`, `.envrc`, and CI do not declare, so this file must not be committed as it stands. `joshuarli-pytest-parallel` is a git source with no `rev`. The lock records commit `d10c19e8dc9c2cbcc92429133312222a5678df25`, but that pin exists only in the untracked file. There is no publish workflow. `make test` passed here (165 tests), so the build is not broken on this machine. An operator cannot roll back to the dependency set that last ran.
- **Recommendation:** Pin `rev` on the git source. Regenerate `uv.lock` against the public index and commit it. Do not commit the Sentry-index lock.
- **Fix with:** none
- **Effort:** S

### RPMD-16 · P3 · The fix hash does not cover modules a fix imports

- **Area:** Architecture
- **Where:** `src/repomod/main.py:113`, `src/repomod/fixes/pin-gha/main.py:9`, `src/repomod/fixes/ubuntu-24/main.py:1`
- **Problem:** `compute_fix_hash` hashes that fix's `main.py` bytes plus the parsed config without `skip`. `pin-gha` imports `repomod.utils`. A change to retry or the rate limiter changes that fix's behavior and does not change the hash, so repos already marked `skipped` or `pr_created` are not run again. `macos-15` and `ubuntu-24` are copied apart for this reason. Nothing checks the imports. The self-contained sentence in the ubuntu module docstring was deleted. It was not enforced.
- **Recommendation:** When loading a fix, reject an import of `repomod`, or add each imported module's bytes to the hash.
- **Fix with:** comment-cleanup
- **Effort:** M

### RPMD-24 · P3 · Force-push replaces commits added on an open fix branch

- **Area:** Data
- **Where:** `src/repomod/git.py:61`, `src/repomod/git.py:195`, `README.md:75`
- **Problem:** `clone` fetches only the default branch and deletes every other local branch. The fix branch is recreated from that tip and pushed with `--force`. Nothing fetches the remote fix branch or uses `--force-with-lease`. A later `--apply` that is not skipped replaces reviewer commits on the open PR. The README now states that. Keeping the force-push is what makes a re-run idempotent. Switching to a lease changes that contract.
- **Recommendation:** Keep the force-push unless you want reviewer commits to block the next apply. The choice is in the RFC.
- **Fix with:** durable-state
- **Effort:** M
- **Design:** [docs/rfc/RPMD-24-force-push.md](docs/rfc/RPMD-24-force-push.md)

### RPMD-28 · P4 · A scalar branch, title, or body is coerced with str

- **Area:** Types
- **Where:** `src/repomod/types.py:139`
- **Problem:** `skip`, `reviewers`, and `sparse_paths` reject a scalar. `branch`, `pr_title`, and `pr_body` still go through `str()`, so a number or a boolean loads as text instead of failing. Walked from the source.
- **Recommendation:** Require those three fields to be strings in `from_yaml`, the same way the lists are required to be lists of strings.
- **Fix with:** make-invalid-states-unrepresentable
- **Effort:** S

### RPMD-29 · P4 · The YAML temp write does not fsync the directory

- **Area:** Data
- **Where:** `src/repomod/status.py:63`
- **Problem:** `atomic_write_yaml` fsyncs the file and then `os.replace`s it. It does not fsync the destination directory. Generation publishes fsync the tree. `repos.yaml` and `repos-excluded.yaml` use this helper, so a crash after the replace can lose the directory entry. Walked from the source.
- **Recommendation:** Fsync the parent directory after `os.replace` in `atomic_write_yaml`.
- **Fix with:** durable-state
- **Effort:** S

