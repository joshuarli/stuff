Prefix: RPMD
Next: 25

### RPMD-19 · P1 · ^C during commit records the repo failed and erases the reviewed result

- **Area:** Errors
- **Where:** `src/repomod/utils.py:52`, `src/repomod/git.py:163`, `src/repomod/main.py:467`, `src/repomod/status.py:42`
- **Problem:** The SIGINT handler only sets an event. It does not start a new session, so the signal is also delivered to the `git` child. A foreground `subprocess.run(..., check=True)` of `sleep` under that handler raised `CalledProcessError` with return code `-2` while the handler ran. `git add`, `git commit`, and `git checkout` in `commit_and_push` are outside `@retry`, so the same error hits the push handler's `SUBPROCESS_ERRORS` branch. That branch calls `set_status(..., status="failed")` and `pd.finish`. `set_status` replaces the repo record, so a dry-run `pending` row (hash, `base_sha`) is discarded. `finish` puts the repo in `_completed`, and the sweep will not mark it `interrupted`. The next `--apply` re-runs the fix instead of pushing the reviewed patch. A `git push` is inside `@retry`: the first child death is retried, and `interrupt.wait` then raises `ShutdownError`. The last retry attempt re-raises the child error instead (`utils.py:105`).
- **Recommendation:** If `interrupt.is_set`, re-raise `ShutdownError` from every `SUBPROCESS_ERRORS` handler and do not call `pd.finish`. Run `git` and `gh` in a new session so an in-flight command is not killed by SIGINT.
- **Fix with:** error-model
- **Effort:** S

### RPMD-20 · P1 · The reviewed patch is replaced before status.yaml is updated

- **Area:** Data
- **Where:** `src/repomod/main.py:417`, `src/repomod/main.py:254`, `src/repomod/main.py:295`, `src/repomod/git.py:128`
- **Problem:** On `--apply`, `write_patch` runs before push and before `set_status`. If the stored patch is missing or does not apply, the fix runs again and that write replaces `patches-{hash}/<repo>.patch` while `status.yaml` still says `pending` with the old `base_sha`. A `ShutdownError` in the push (see RPMD-19) keeps `pending`, because that status is in `RESULT_STATUSES`. The next `--apply` sees a matching hash and `base_sha` and pushes the new bytes. `get_diff` ignores git's exit code, so a failed `git diff` can be stored as the reviewed patch on the dry-run path, which has no interrupt check before `set_status`. The kill window was walked from these lines. It was not executed end to end.
- **Recommendation:** Write the patch and the status row in one step, and on `--apply` only after the push succeeds. Fail `get_diff` when git exits non-zero.
- **Fix with:** durable-state
- **Effort:** M

### RPMD-1 · P2 · A workflow symlink is rewritten outside the clone and the diff stays empty

- **Area:** Security
- **Where:** `src/repomod/fixes/pin-gha/main.py:89`, `src/repomod/fixes/ubuntu-24/main.py:200`, `src/repomod/fixes/macos-15/main.py:112`, `src/repomod/git.py:81`
- **Problem:** Each fix `rglob`s `*.yml` and `*.yaml` under `.github` and `write_text`s matches. On Python 3.14, `rglob` does not follow a symlinked `.github` directory (that listing was empty). It does return a symlinked file. `Path.write_text` follows it: an absolute symlink to a file outside the repo was overwritten, `git diff` was empty, and `has_changes` was false, so the repo is recorded as no work. `git clone` checks out committed symlinks. Whether any of the 109 repos in `repos.yaml` contains one was not checked.
- **Recommendation:** Skip a path whose resolved file is outside the clone, or whose path has a symlink component. Write only through a file opened with `O_NOFOLLOW`.
- **Fix with:** os-boundaries
- **Effort:** S

### RPMD-2 · P2 · A failed sparse checkout is retried as a successful empty tree

- **Area:** Data
- **Where:** `src/repomod/git.py:44`, `src/repomod/git.py:51`, `src/repomod/fixes/pin-gha/config.yaml:9`, `src/repomod/main.py:332`
- **Problem:** `clone` is `@retry`. A new dest is `git clone --sparse` and then `sparse-checkout set`. If that set fails, the dest already has `.git`, and the retry takes the existing-clone branch, which fetches and returns without setting the cone. Calling `clone` on a sparse dest that had never had the cone applied returned success while the workflow file was in `git ls-files` and absent from the worktree. Every current fix sets `sparse_paths: [.github]`. The fix then sees no workflows, `has_changes` is false, and the repo is stored as `skipped` under the current hash, so later runs do not retry it. The same path ignores a later change to `sparse_paths`.
- **Recommendation:** Apply `sparse-checkout set` on the existing-clone path too, and clear the cone when `sparse_paths` is `None`. If clone, fetch, or checkout fails, delete the dest before retrying. Treat `.git` as reusable only when `HEAD` resolves and the cone matches.
- **Fix with:** lifecycle-concurrency
- **Effort:** S

### RPMD-3 · P2 · A later status write deletes the PR identity, so --undraft reports nothing

- **Area:** Data
- **Where:** `src/repomod/status.py:42`, `src/repomod/main.py:423`, `src/repomod/main.py:630`
- **Problem:** `set_status` replaces the whole repo record. A follow-up `pending` write after `pr_created` dropped `pr_url` and `pr_node_id` (reproduced with `set_status`). That write happens whenever the fix hash changes and the repo is processed again. `--undraft` and the node-id cache select only `status == "pr_created"`, so those draft PRs stay open and the command exits 0.
- **Recommendation:** Merge fields into the existing record so `pr_url` and `pr_node_id` survive a later attempt. Select undraft candidates by those fields, then keep the live open-and-draft check.
- **Fix with:** make-invalid-states-unrepresentable
- **Effort:** M

### RPMD-4 · P2 · A scalar skip list fails open and the repo runs

- **Area:** Types
- **Where:** `src/repomod/types.py:123`, `src/repomod/main.py:500`, `src/repomod/git.py:71`, `src/repomod/git.py:205`
- **Problem:** `FixConfig.from_yaml` checks only that `branch`, `pr_title`, and `pr_body` are present. `skip: acme/backend` loaded as a string. `set(config.skip)` was a set of characters, and `"acme/backend" in skip` was false, so the repo is not skipped. The same input turned `reviewers` into one flag per character and `sparse_paths` into `git sparse-checkout set` of each character. The dataclass says `list[str]`. Current fix configs happen to use lists.
- **Recommendation:** In `from_yaml`, require `skip` and `reviewers` to be lists of strings, and `sparse_paths` to be `None` or a list of strings. Reject anything else.
- **Fix with:** make-invalid-states-unrepresentable
- **Effort:** S

### RPMD-5 · P2 · The reviewed diff ignores files that the push commits

- **Area:** Data
- **Where:** `src/repomod/git.py:81`, `src/repomod/git.py:128`, `src/repomod/git.py:163`, `src/repomod/main.py:332`, `src/repomod/main.py:417`
- **Problem:** `has_changes` is `git diff --quiet` and the stored patch is `git diff`, both tracked-only. `commit_and_push` runs `git add --all`. A repo whose only edit was a new file reported `has_changes` false and an empty diff, then `commit_and_push` committed that file and called the push. A fix that only adds a file is recorded as `skipped` and never shown. A tracked edit plus an untracked file (a new workflow, or a leftover such as `.DS_Store` when the repo does not ignore it) is reviewed without the extra file and pushed with it. `changed_files` uses the same tracked diff, so actionlint does not see a new workflow. The three current fixes edit tracked files.
- **Recommendation:** Stage the tree once, review `git diff --cached`, and commit that index. Use the same file list for actionlint.
- **Fix with:** none
- **Effort:** S

### RPMD-6 · P2 · The clone directory drops the owner, and reuse fetches the old origin

- **Area:** Data
- **Where:** `src/repomod/types.py:39`, `src/repomod/main.py:382`, `src/repomod/git.py:51`
- **Problem:** Logs and patches use `Repo.slug` (`owner-name`). The checkout is `repos/<short name>`. `clone` on an existing dest fetches `origin` and never sets the remote URL. Running that command sequence against a dest whose origin was repo A, while the intended repo was B, left origin at A and the worktree on A's tree. `gh pr create --repo` still receives the second full name. `repos.yaml` has 109 repos, 87 under `joshuarli` and 22 under `laputa-systems`, and 0 shared short names today. The next shared name force-pushes the first repo, including with `--workers 1`.
- **Recommendation:** Key the checkout by `Repo.slug`. Before fetch, set `origin` to `https://github.com/{name}.git`. Refuse a dest whose real path is not under that fix's `repos/` directory.
- **Fix with:** os-boundaries
- **Effort:** S

### RPMD-7 · P2 · A git failure while saving the diff aborts the run

- **Area:** Errors
- **Where:** `src/repomod/main.py:332`, `src/repomod/main.py:417`, `src/repomod/main.py:272`, `src/repomod/main.py:558`, `src/repomod/git.py:119`
- **Problem:** Clone, the fix, validation, and push each record a per-repo failure. `has_changes`, `get_diff`, `write_patch`, and `head_sha` sit outside those handlers. `apply_patch` catches `CalledProcessError` and not `TimeoutExpired`. `process_repo` only catches `ShutdownError`. With one worker that exception leaves the loop before the interrupt sweep, so later repos are neither run nor marked `interrupted`. With a pool, `future.result()` raises the same way: the repo is not stored as `failed`. `docs/design.md` says one repo failing does not stop the run. Walked from the source. Not executed.
- **Recommendation:** Catch `SUBPROCESS_ERRORS` and `OSError` around the diff, patch, and `base_sha` block the same way as push, and record that repo as `failed`. Make `get_diff` use `git_run`.
- **Fix with:** error-model
- **Effort:** S

### RPMD-8 · P2 · The dependency set is not pinned in git

- **Area:** Dependencies
- **Where:** `pyproject.toml:5`, `pyproject.toml:24`, `uv.lock` (untracked)
- **Problem:** `git ls-files` does not contain `uv.lock`, and `git status` shows it untracked. `uv run` on a fresh checkout resolves ranges again. The on-disk lock points every registry URL at a Sentry index that `pyproject.toml`, `.envrc`, and CI do not declare, so this file must not be committed as it stands. `joshuarli-pytest-parallel` is a git source with no `rev`. The lock records commit `d10c19e8dc9c2cbcc92429133312222a5678df25`, but that pin exists only in the untracked file. There is no publish workflow. `make test` passed here (165 tests), so the build is not broken on this machine. An operator cannot roll back to the dependency set that last ran.
- **Recommendation:** Pin `rev` on the git source. Regenerate `uv.lock` against the public index and commit it. Do not commit the Sentry-index lock.
- **Fix with:** none
- **Effort:** S

### RPMD-9 · P2 · The interrupt test never locks an interrupted repo

- **Area:** Tests
- **Where:** `tests/test_main.py:868`, `src/repomod/main.py:254`, `src/repomod/main.py:266`
- **Problem:** `test_ctrl_c_during_apply_keeps_earlier_results_instead_of_marking_interrupted` is a dry run. The fix sets `interrupt.event` and returns. Nothing raises `ShutdownError`, and `--apply` is not passed. Alpha still finishes as `pending` and calls `pd.finish`. The test does lock the keep-list: beta (`pending`) and gamma (`pr_created`) are preserved, and deleting the `RESULT_STATUSES` guard in `mark_interrupted` would fail it. It stays green if an unfinished repo is never written as `interrupted`, if that repo calls `pd.finish`, if an in-flight `--apply` pushes, or if the next run does not retry `interrupted`. Those are the behaviors `docs/design.md` states.
- **Recommendation:** One `--apply` run where the in-flight repo raises `ShutdownError` before push. Assert that repo is `interrupted`, `pd.finish` was not used for it, no branch was pushed, the process exits 130, and a second run takes that repo again.
- **Fix with:** test-audit
- **Effort:** M

### RPMD-21 · P2 · A PR merged during the run is force-pushed and recorded as open

- **Area:** Concurrency
- **Where:** `src/repomod/main.py:841`, `src/repomod/git.py:152`, `src/repomod/git.py:200`
- **Problem:** `--apply` takes one search snapshot of PRs on the fix branch. `commit_and_push` skips the push only when that snapshot is not `OPEN`. If the PR is merged after the search, the worker still `git push --force`, and `push_and_open_pr` returns the old URL with state `OPEN` when `known_pr` is set. Status becomes `pr_created`, so later runs skip the repo. The search returns at most 1000 hits (`git.py:379`). A repo missing from that map never hits the guard. The GitHub result of force-pushing a branch whose PR just merged was not executed.
- **Recommendation:** Read that repo's PR state immediately before the push. Do not push when the live state is merged or closed.
- **Fix with:** lifecycle-concurrency
- **Effort:** S

### RPMD-22 · P2 · A fix branch named like the default branch is force-pushed

- **Area:** Data
- **Where:** `src/repomod/git.py:155`, `src/repomod/git.py:195`, `src/repomod/main.py:386`
- **Problem:** `clone` leaves HEAD on `default_branch`. If `config.branch` is that same name, `commit_and_push` does not create a new branch. `git push --force origin <branch>` then rewrites the default branch. Nothing compares the two names. The current fixes use other branch names.
- **Recommendation:** Refuse the run when the fix branch equals the repo's default branch, before any commit.
- **Fix with:** durable-state
- **Effort:** S

### RPMD-23 · P2 · ^C on the main thread can deadlock the progress lock

- **Area:** Concurrency
- **Where:** `src/repomod/utils.py:52`, `src/repomod/main.py:540`, `src/repomod/ui.py:259`, `src/repomod/main.py:545`
- **Problem:** The handler calls `pd.print`, which takes `ProgressDisplay._lock`. That lock is a `threading.Lock`. Signal handlers run on the main thread. With `--workers 1`, or a single repo, `process_repo` runs on that thread, and `print` / `finish` / `set_phase` hold the same lock. A handler that runs while the lock is held cannot acquire it: a same-thread `acquire` with a 0.3s timeout returned false in a reproduction of this pattern. The handler returns immediately once the event is set, and the previous SIGINT handler is not restored until `run_work`'s `finally`, which does not run if the handler is stuck. A second ^C does nothing.
- **Recommendation:** The handler should only set the event. Print from the worker loop. On a second ^C, restore the default SIGINT handler.
- **Fix with:** lifecycle-concurrency
- **Effort:** S

### RPMD-10 · P3 · --help says the repo cache expires after one hour

- **Area:** Config
- **Where:** `src/repomod/main.py:96`, `src/repomod/main.py:761`, `README.md:40`
- **Problem:** `SYNC_MAX_AGE` is 24 hours, and the README matches it. The `--refresh` help says the cache is used when it is `<1h` old. An operator who trusts `--help` will not pass `--refresh` and will apply against a stale `repos.yaml`.
- **Recommendation:** Build that help string from `SYNC_MAX_AGE`.
- **Fix with:** single-source-of-truth
- **Effort:** S

### RPMD-11 · P3 · Rollout state sits in the package and is not ignored

- **Area:** Operability
- **Where:** `.gitignore:1`, `src/repomod/main.py:794`
- **Problem:** `repos/` and `logs/` are ignored. `status.yaml` and `patches-*/` are written under `src/repomod/fixes/<name>/` and are not. Committing a `status.yaml` makes the next clone skip every repo whose hash and status say done. The files are untracked until a run creates them.
- **Recommendation:** Ignore `status.yaml` and `patches-*/` next to a fix. Keep ignoring `repos/` and `logs/`.
- **Fix with:** none
- **Effort:** S

### RPMD-12 · P3 · Status writes share one temp name and an in-process lock

- **Area:** Data
- **Where:** `src/repomod/status.py:14`, `src/repomod/status.py:19`
- **Problem:** `atomic_write_yaml` writes `path.with_suffix(".tmp")` (`status.yaml` becomes `status.tmp`) and renames it into place. The rename is atomic for one process on the same filesystem. `open` follows a symlink at that temp path. The lock is a `threading.Lock`, so two `repomod` processes on the same fix can lose each other's repo rows. There is no `fsync` before the rename. Not executed as a two-process race.
- **Recommendation:** Create an exclusive temp file in the destination directory, `fsync`, then `os.replace`. Take an OS file lock around the read-modify-write of `status.yaml`.
- **Fix with:** durable-state
- **Effort:** S

### RPMD-13 · P3 · A branch that starts with a dash is a git option

- **Area:** Security
- **Where:** `src/repomod/git.py:52`, `src/repomod/git.py:54`, `src/repomod/git.py:195`
- **Problem:** Arguments are a list, and there is no shell. There is also no `--` before the ref. `git fetch --depth=1 origin <ref>` with a ref of `--upload-pack=<local wrapper>` ran that wrapper (it wrote a marker file) and exited 0. `git check-ref-format` rejects that string, so a default branch from GitHub cannot be this. `branch` in `config.yaml` is passed through to checkout and to `git push ... origin <branch> --force`, which was not executed. A normal branch name is unaffected.
- **Recommendation:** Insert `--` before every refspec and branch. Reject a configured branch that `git check-ref-format` rejects.
- **Fix with:** os-boundaries
- **Effort:** S

### RPMD-14 · P3 · A no-change repo is not required to be stored as skipped

- **Area:** Tests
- **Where:** `tests/test_main.py:358`, `src/repomod/main.py:332`
- **Problem:** `test_e2e_dry_run_no_changes` allows a missing `status.yaml`. The code writes `skipped` plus `fix_hash`, which is what lets the next run skip the clone. Removing that write leaves the test green, and the docstring that claimed no status was written was false. It was deleted.
- **Recommendation:** Assert the repo is `skipped` with the current `fix_hash`.
- **Fix with:** test-audit
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
