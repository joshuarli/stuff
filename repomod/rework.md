# Persistent repository library and isolated fix workspaces

Status: ready
Preflight date: 2026-10-17

## Goal

Add a persistent, shallow repository library while keeping repomod's mass-fix eligibility, review, resume, and pull request behavior. Run each fix in a disposable linked worktree so fix code never edits the canonical checkout.

## Deliverables

1. A validated, versioned `catalog.yaml` selects owner repositories and pinned repositories for the library.
2. `repomod sync` refreshes `repos.yaml` and reconciles every repository in the library without discarding local checkout state.
3. `repomod add`, `repomod list`, and `repomod path` manage and inspect the library.
4. Fixes run in detached worktrees at recorded base commits. Validation, dry-run reuse, apply, interruption recovery, and PR updates keep their current behavior.
5. The README, design guide, and tests cover the new command and storage contracts. No test depends on live GitHub or a network remote.

## Charter

This plan grants no execution authority. Start only from a launch instruction that authorizes source, test, and documentation edits plus local commits on campaign branches. Never push, create or update a real pull request, mark a pull request ready, or run the application against live GitHub repositories. Exercise GitHub behavior with the existing fake-`gh` harness and Git behavior with local bare repositories.

If the launch instruction does not authorize local commits, stop before creating lane branches and report that hard stop. Do not ask during the campaign.

Keep all campaign worktrees and scratch files under `.work/`. Use `.work/wt/<lane>` for lane worktrees, `.work/wt/verify/<group>` for merged-tree checks, and `.work/tmp` for scratch. Before the first write, verify that `.work/` is ignored through `.git/info/exclude`. Set `TMPDIR=$PWD/.work/tmp` for commands that use temporary files. Never use `/tmp` or `$TMPDIR` without that setting.

Use one lane for each disjoint ready workstream. The first implementation tranche has a floor of two lanes: catalog and Git storage. Add the runtime lane as soon as its store and catalog interfaces land. Start the documentation lane as soon as the user-facing contracts are fixed. Never leave a ready disjoint lane idle. Replace or resume a stopped lane in the same turn. Integrate and verify each report while the next tranche starts. Run checks in separate scratch worktrees when their inputs are disjoint. Never collect or run two checks against the same tree.

The preflight host had 32 logical CPUs, 53 GiB available memory, 862 GiB free disk, and load averages of 11.52, 6.27, and 6.22. Recheck load, memory, swap, and disk at launch and before each tranche. Budget 2 GiB peak memory and 5 GiB scratch disk per lane, and leave 25% of available memory free. Set the lane ceiling to the lowest of ready disjoint work, logical CPUs minus one, available memory after the reserve divided by 2 GiB, and free disk divided by 5 GiB. Launch every ready lane under that ceiling. Measure the first tranche's peak memory and adjust the width from observed use. Do not allow swapping or an out-of-memory kill.

Each lane owns the files listed in its workstream and does not edit another lane's files. Lanes do not coordinate directly. The integrator owns shared contracts, reviews every diff, merges with fast-forward only, and runs the checks for the merged changes in a scratch worktree. Lane commits use Conventional Commit subjects with the workstream as scope and the item ID in the body. Never format files or change CI.

Use the smallest reversible design choice that meets this contract. Record a judgment in the decision log. After three failed cause-and-fix attempts on one item, write the smallest reproduction and the evidence, park that item, and continue with independent work. Before a retry, establish the cause from the first failing command and the actual repository state. Retry only transient network failures, at most three times with the existing bounded retry policy. A technical blocker belongs to the integrator. Fix a shared contract or tool and resume the lane. Stop only when every deliverable passes or each remaining item is parked with its evidence.

Do not add dependencies. Use the standard library, PyYAML, Git, and the existing `gh` CLI. If an implementation needs another dependency, park that item with its build-time, binary-size, startup, runtime, license, and maintenance costs. Preserve the `accept_repo(repo, config)` and `run(repo: Path, config)` fix interface.

## Decisions

### Owner-decided on 2026-10-17

- Include the repository library, catalog configuration, and `add`, `list`, and `path` commands.
- Expand `repomod sync` to reconcile canonical checkouts while refreshing `repos.yaml`.
- Use shallow canonical clones with `--depth=1 --filter=blob:none`.

### Prescribed

- Store `catalog.yaml` as tracked configuration with `version: 1`, `owners`, and `pinned` fields. Seed `owners` with the current owners, `joshuarli` and `laputa-systems`, and `pinned` with an empty list. Reject an unknown version, unknown key, duplicate, malformed entry, or wrong YAML shape before any write.
- Automatically collect every non-archived repository under `owners`. A pin also collects an inactive, archived, or third-party repository. A pin never makes that repository eligible for a mass fix. Fix eligibility stays based on current owner and activity rules, `repos-disabled.yaml`, fix-specific skips, and `accept_repo()`.
- Keep `repos.yaml` as the generated active-target list with its current shape and 24-hour cache for fix selection. A direct `repomod sync` always refreshes discovery and the target list, then fetches or reconciles every collected repository, regardless of cache age. A fix run fetches the chosen remote default branch before preparing its worktree. `--plan` never clones, fetches, or changes a checkout.
- `repomod add OWNER/REPO` first validates the address syntax and atomically adds it to `pinned`, then looks it up and tries to create its shallow canonical checkout. If lookup or clone fails, keep the pin and return failure; a later `sync` resumes it. Repeating `add` is idempotent. `list` reads local canonical checkouts and reports their health without network access. `path OWNER/REPO` prints the existing canonical absolute path and returns failure when it is absent.
- Place canonical checkouts at `repos/<owner>/<repo>`. Validate a repository address once before it reaches path or Git construction. Reject path separators inside either segment, dot segments, NUL, symlinked path components, and any resolved path outside the canonical root. Do not rewrite an existing remote. Accept only an HTTPS or SSH GitHub origin that identifies the requested repository. A wrong origin or invalid clone is reported without fetching, moving, or deleting it.
- Fetch a valid canonical checkout's remote default head before deciding whether to advance it. If the checkout is dirty, detached, or on a non-default branch, fetch only and preserve its files, index, branch, and local commits. On a clean default branch, leave an equal head alone. If ancestry is unknown because of the shallow boundary, deepen in chunks of 32, 64, 128, 256, 512, and 1024 commits, checking after each fetch. Fast-forward only when Git proves the old head is an ancestor. If the local head is ahead, the histories diverge, or the 2016-commit bound is reached while ancestry remains unknown, fetch only and report the exact state. A later sync may continue from the current shallow boundary. Never unshallow, hard-reset, clean, stash, switch branches, or delete local branches in a canonical checkout. Never run generic `git worktree prune`.
- Create each fix workspace at `.work/wt/repomod/<run-id>/<owner>/<repo>`, detached at the immutable remote default-branch SHA. Keep it alive from fix execution through diff validation, status publication, the second pull-request snapshot, and any push. The canonical checkout stays untouched by fix code. Sparse paths apply only inside the fix workspace. Push an apply result as `HEAD:refs/heads/<fix-branch>`, retaining the current default-branch refusal, open-PR force-push behavior, and merged-or-closed protections.
- Replace stash-based baseline validation with a second worktree at the same base SHA and sparse paths. Git stash refs are shared by linked worktrees. This preflight reproduced two detached worktrees making independent commits and pushes while the canonical checkout remained clean on its original branch and SHA. It also reproduced a stash made in one worktree appearing in the other. Keep both facts as real local-Git regression tests.
- A depth-one fetch did not let Git prove that the previous head was an ancestor in a local bare-remote experiment. Deepening by four commits exposed the ancestry and allowed the proof. Keep the bounded-deepening rule above and test it against a local bare remote.
- Use the current `status.yaml` fields and atomic patch-plus-status publication unchanged. Keep old pending patches readable and byte-exact. Keep the flattened `owner-repo` patch and log artifact names for persisted-data compatibility. Before processing a fix batch, detect any selected repositories whose flattened artifact names collide; record those repositories as failed without writing an ambiguous patch or log. New canonical checkout and worktree paths use separate owner and repository segments.
- Allow different fixes to use separate worktrees for the same canonical repository. Hold one nonblocking process lease per fix directory while that fix can write status, patches, or a PR branch. A second process for the same fix exits with a clear busy error before writing. Read-only commands and `--plan` remain available.
- Leave any existing per-fix `repos/` directories untouched and unused. Do not add a prune or delete command. The repository library is durable user data.
- Preserve the exact `--apply` GitHub API-call contract in `tests/test_apply_gh_calls.py`. Keep API calls batched or once per run. A dry run makes no GitHub write calls.
- Preserve shutdown behavior. Put `except ShutdownError: raise` before broad handlers. Never call `pd.finish()` for an interrupted repository. Release every lease on normal return, error, and graceful interrupt. After a forced process death, recover only repomod-owned worktrees whose process lock is free. Cleanup errors must not hide the primary failure.
- Return failure from `sync` when discovery fails or any repository fails reconciliation. A discovery failure leaves `repos.yaml` unchanged. A per-repository failure does not stop other repositories; report each failed repository and exit nonzero. Keep the existing transient network retry bound. Do not retry malformed configuration, unsafe paths, dirty checkouts, wrong origins, or diverged branches.
- Keep Git subprocesses in `git.py`, catalog parsing and read-modify-write policy in `catalog.py`, GitHub discovery and `repos.yaml` writes in `sync.py`, and fix orchestration and CLI handling in `main.py`. Keep shared dataclasses, named tuples, and aliases in `types.py`. Keep status, patch, log, locks, and the atomic YAML writer in `status.py`; catalog and sync call that writer. The store module may coordinate these owners but does not call subprocess directly.
- Remove the old per-fix clone/reset implementation and stash helper after migrating every live caller. Keep no runtime compatibility path for those internal functions. Leave old generated clone directories untouched. Retain legacy patch readers only because current status rows can point to those persisted files; new writes use the current format.
- Do not touch `repos-disabled.yaml`, hand-edit generated files, change the fix API, add CI, or make live GitHub changes.

## Baseline and verification loop

At preflight, the current baseline was clean: `309 passed, 2 skipped` out of 311 tests. `make test` took 4.51 seconds wall time. `uv run ruff check src tests` took 0.07 seconds. `uv run ty check` took 0.14 seconds. Each time is one run, not a performance comparison. The suite uses the offline fake-GitHub harness. No product performance benchmark was run.

The preflight host had 32 logical CPUs and 53 GiB available memory, with 862 GiB free disk. Re-measure at campaign launch. The current generated `repos.yaml` contains 52 active targets. No root `repos/` directory existed at preflight. Recheck both before changing or adopting data.

At P0, rerun these commands on the campaign base and record existing failures before editing:

- `make test`
- `uv run ruff check src tests`
- `uv run ty check`

For each item, use the narrowest existing test that covers the behavior. Run these focused groups in separate verification worktrees when their inputs are disjoint:

- Catalog and discovery: `uv run --group dev --quiet pytest tests/test_catalog.py tests/test_sync.py`
- Canonical Git storage and worktree lease: `uv run --group dev --quiet pytest tests/test_repo_store.py tests/test_git.py`
- CLI, fix execution, status, validation, and PR flow: `uv run --group dev --quiet pytest tests/test_main.py tests/test_lint.py tests/test_status.py tests/test_types.py tests/test_apply_gh_calls.py tests/test_apply_prs.py tests/test_branch_args.py tests/test_run_fix_and_validate.py`

A lane runs only the cheapest compile or type check and tests it wrote or changed, by name. The integrator runs the focused group for each merged item. Run `make test` at the end of P2 because catalog, storage, fix execution, status, validation, and PR publication now share one lifecycle. Run it again at close. Run Ruff and `ty` on the merged tree at those same two milestones. Do not run a workspace-wide suite before P2. Keep any newly introduced test selector fast enough for the focused loop.

At P0, establish a local Git storage baseline before replacing clone code. Use 20 local bare repositories with 40 commits each and a deterministic 64 KiB file at the tip. Compare two per-fix shallow clones against one canonical shallow clone plus two detached worktrees for each repository. Record clone and worktree counts, summed Git object-directory bytes, expected files, and canonical-checkout state. Measure setup wall time only when `perf-gate-ready` passes before and after the run. Keep the fixture and outputs in `.work/tmp` and remove them after each run. This is a local storage/setup measurement, not a claim about live GitHub latency. If the gate is busy at P0, keep the reference worktree and measure both sides at P4 when ready.

At P4, repeat both sides five times in alternating order with identical Git versions, fixture, and settings. Record median and range for wall time and object bytes, and confirm every operation succeeds and produces the expected files. The target is at most 1.25 times one canonical clone's Git object bytes for two fixes, with no setup time above twice the per-fix-clone baseline. Optimize only if the parity work is complete and a result misses a target. Fix a focused command that exceeds 60 seconds or a matched workflow that regresses by more than 2x; leave smaller differences alone.

Run a time-based performance measurement only when `perf-gate-ready` exits 0 before it and `perf-gate-ready -1` exits 0 immediately after it. Discard a run that overlaps load. If the host remains busy, write exactly `perf gate deferred: host busy` and do not call the campaign complete or release it.

## Sequence

P0 is ground work. Parity and complete behavior come next. Hardening follows. Optimization and polish stay in P4. Close only after the acceptance checks pass or every unfinished item is parked with evidence.

### P0: Establish the baseline and shared contract

- **B0, size M.** Record the baseline commands, current failures, host capacity, and local Git fixture measurement. Commit this plan as `docs(campaign): define repository library rework`, with B0 in the body. Own `rework.md` only. Prerequisite: none. Proof: all three baseline commands and the local fixture check complete, with timing either gate-ready or deferred. Preserve a detached reference worktree at `.work/wt/reference` until parity closes.
- **B1, size M.** Define the shared `RepoAddress`, catalog data, canonical checkout state, and prepared-workspace seam in `src/repomod/types.py`. Own that file and `tests/test_types.py` only. Prerequisite: B0. Proof: focused type tests, Ruff, and `ty` pass. Integrate this shared contract before launching feature lanes.

### P1: Deliver catalog, storage, and fix behavior

- **CAT, size L.** Implement catalog validation, owner and pin discovery, and target-cache refresh. Own `src/repomod/catalog.py`, `src/repomod/sync.py`, `tests/test_catalog.py`, and `tests/test_sync.py`. Do not edit `main.py`. Prerequisite: B1. Proof: the catalog and discovery focused group passes, including archived pins, inactive owner repositories, disabled fix targets, malformed config, batching, partial API responses, and unchanged `repos.yaml` on discovery failure.
- **STORE, size L.** Implement safe canonical clone, fetch, bounded shallow-history proof, fast-forward, path checks, per-repository locking, detached worktree leases, and orphan recovery. Own `src/repomod/repo_store.py`, `src/repomod/git.py`, `tests/test_repo_store.py`, and `tests/test_git.py`. Do not edit `main.py`, `sync.py`, `lint.py`, or status files. Prerequisite: B1. Proof: real local-Git tests cover the full checkout-state matrix, shallow fast-forward and bounded unknown ancestry, symlinks and path traversal, the `.git` file in linked worktrees, sparse isolation, concurrent worktree creation, signal cleanup, process death recovery, and canonical-checkout preservation.
- **RUN, size L.** Add the `add`, `list`, and `path` commands. Migrate dry run, validation, saved-patch reuse, and apply to the store lease. Keep the lease through status publication and PR refresh. Own `src/repomod/main.py`, `src/repomod/lint.py`, `src/repomod/status.py`, `tests/test_main.py`, `tests/test_lint.py`, `tests/test_status.py`, `tests/test_apply_gh_calls.py`, `tests/test_apply_prs.py`, `tests/test_branch_args.py`, and `tests/test_run_fix_and_validate.py`. Do not edit catalog or Git-store files. Prerequisites: CAT and STORE APIs merged. Proof: the CLI-focused group passes, including unchanged fix API behavior, stored patch reuse at the same base, re-run on a moved base, no push for merged or closed PRs, exact API call counts, and interrupted status recovery.
- **DOCS, size M.** Update `README.md` and `docs/design.md` for the final command, catalog, safety, persistence, error, and recovery behavior. Own only those docs. Start once B1 fixes the contracts; update against each merged RUN change. Proof: every documented command and file path matches the merged code; `mdtool check --untracked rework.md README.md docs/design.md` reports no broken links or stale consumers.
- **PARITY, size M.** Compare every baseline command, flag, output class, error, exit code, stored format, and side effect in the reference worktree with the new implementation. Record intentional changes in this plan's decision log. Ensure each row in the parity register below has a named test that fails when the behavior is broken. Own the gaps in the workstream that owns each file. Prerequisites: CAT, STORE, and the first complete RUN path. Proof: each focused test group passes and the parity register has no open row.

The first ready tranche runs CAT, STORE, and DOCS in parallel after B1. RUN starts as soon as CAT and STORE APIs merge. PARITY starts as soon as RUN lands its first complete path. While RUN implements the fix lifecycle, DOCS continues against landed code. The integrator reviews and verifies every completed lane before assigning its next ready slice. If the test queue becomes the critical path, assign a loop-speed lane before adding implementation width. Parallelize the three focused groups on separate worktrees when the changed files and test inputs are disjoint.

### P2: Harden crash and compatibility behavior

- **HARD, size M.** Close every error and interruption window across catalog writes, repository fetches, worktree creation/removal, status publication, and push recovery. Preserve the current status shape and exact patch bytes. Test atomic catalog replacement, concurrent `add`, duplicate sync, same-fix process exclusion, interrupted add, failed cleanup without masking the first error, and a forced process death between each durable write and its next effect. Own the same files as the lane that owns each state. Prerequisites: CAT, STORE, RUN, and PARITY. Proof: focused tests pass for each window and an immediate second run reaches a valid state without human cleanup.

The parity register is:

- Fix selection, `--plan`, `--repos`, `--limit`, `--refresh`, `--workers`, `--status`, `--prs`, `--diffs`, `--undraft`, and dry-run output order.
- Fix hashing, old pending status rows, byte-exact patches, `base_sha`, `pushed_sha`, atomic status writes, and resume after failure or interrupt.
- Activity filtering, bot detection, disabled repos, skip lists, `accept_repo()`, and one failed repository not stopping other work.
- Actionlint checks only changed workflow files and reports only errors introduced by the fix. The baseline uses a separate worktree, never stash.
- PR snapshots, batched lookups, reviewers, draft creation, force-push to an open PR, and no push to a merged, closed, or default branch.
- `sync` reconciles the whole collected library and refreshes the active-target cache. `add` writes a pin and retries safely. `list` is local and read-only. `path` fails for a missing checkout.
- Dirty, detached, feature-branch, ahead, diverged, invalid-origin, invalid-clone, missing, shallow-fast-forward, and bounded-unknown canonical states.
- Malformed catalog, unsafe path, symlink, slug collision, partial GitHub failure, partial Git failure, duplicate submission, same-fix process exclusion, cancellation, and stale-worktree recovery.

### P3: Update docs and remove obsolete code

- **DOCS-CLOSE, size S.** Complete README and design guide updates from merged behavior. Remove any old clone-reset and stash claims. Prerequisite: PARITY. Proof: `mdtool check --untracked rework.md README.md docs/design.md` passes.
- **DELETE, size S.** Search definitions, callers, tests, docs, and fix imports for the old clone/reset and stash paths. Remove them and their tests after all callers use the store. Keep the old persisted patch reader and flattened artifact format. Do not remove ignored per-fix clone directories. Prerequisite: RUN and PARITY. Proof: `rg -n 'stashed_changes|_reset_existing_clone|_clone_fresh|def clone\(' src tests README.md docs` finds no live definition or caller; inspect every remaining match.

### P4: Measure storage and setup cost

- **MEASURE, size M.** Run the repeated local fixture comparison from the baseline section after parity passes. Own only temporary fixture code under `.work/tmp`; do not add a dependency or permanent benchmark framework. Prerequisites: all P1 and P2 items. Proof: five alternating baseline/candidate runs, correct files and exit status, median and range recorded in the campaign log, performance gate ready before and after. Do not start optimization before this item.

### P5: Close

- **CLOSE, size M.** Review all merged diffs. Run `make test`, `uv run ruff check src tests`, `uv run ty check`, the docs check, and the final performance comparison. Run `ultrareview` and fix its P1 and P2 findings. Remove only campaign worktrees that are clean and whose commits are integrated. Keep no lane running. Run `codex-campaign-closeout.py begin` and satisfy its guard before reporting completion. If the campaign resumes, cancel the pending closeout and rerun it after the new work. Prerequisite: every deliverable passes or remaining work is parked with evidence. Proof: all close commands pass, performance gate is not deferred, and the closeout guard passes.

## Acceptance

- Catalog parsing rejects invalid and unknown data before touching the library. Owners and pins have the documented scope. `add` is idempotent, `list` reads without network access, and `path` returns only an existing canonical checkout.
- `sync` refreshes `repos.yaml`, creates missing shallow checkouts, and safely fetches or fast-forwards existing ones. Every dirty, branch-switched, ahead, diverged, wrong-origin, or corrupt checkout keeps its local files, index, branch, and commits. Repository-address-derived checkout paths stay under `repos/`; fix worktrees stay under `.work/wt/`.
- Fix code receives an isolated worktree at one immutable base. The canonical checkout is unchanged by fix, validation, dry run, interruption, and mocked apply. Validation runs before a diff is shown or pushed. A dry-run patch is reused only when its fix hash and base SHA still match.
- The same status rows, patch contents, PR states, GitHub API counts, interruption rules, and fix author API remain valid. The only approved CLI changes are the expanded `sync` contract and the new library commands.
- Every new failure has an owner, path or repository context, and a nonzero command result where appropriate. One repository failure does not silently drop another repository's result.
- All focused tests pass after each merge. Full tests, Ruff, type checking, docs checks, and performance measurement pass at close.

## Execution boundary

The launch instruction must authorize source, test, and documentation edits and local commits. The campaign may create local worktrees and shallow local fixtures. It may read existing generated state but must not hand-edit it.

Never push, call a live GitHub write API, run `--apply` against live repositories, overwrite or delete canonical checkouts, prune worktrees generically, or remove old per-fix checkout directories. Never change `repos-disabled.yaml`. If an item would require one of those effects, park it with the command, state, and reason. The plan itself grants none of these permissions.

## Non-goals

- Full-history clones, indexing, repository search, dependency reports, daemons, containers, or new Git libraries.
- Automatic deletion, quarantine moves, pruning, or cleanup of canonical user repositories.
- Making pinned third-party repositories mass-fix targets.
- Changing the fix API, `repos.yaml` schema, status fields, patch bytes, PR policy, runtime dependencies, or CI.
- Migrating or deleting generated per-fix repository directories.

## Reference and code map

The executable reference is the implementation at campaign launch. Compare it with the baseline tests and docs before changing each behavior. The existing tests in `tests/test_main.py`, `tests/test_git.py`, `tests/test_sync.py`, `tests/test_status.py`, `tests/test_lint.py`, `tests/test_apply_gh_calls.py`, and `tests/test_apply_prs.py` are the current oracle. Add a named regression test for each new library behavior and each deliberate difference.

The current map is:

- `src/repomod/main.py` owns CLI dispatch, cache policy, fix selection, work execution, and PR orchestration.
- `src/repomod/sync.py` owns GitHub repository discovery and writes `repos.yaml`.
- `src/repomod/git.py` owns Git and GitHub CLI subprocesses, clone, validation, commit, push, and PR calls.
- `src/repomod/types.py` owns shared types, including `Repo`, `FixConfig`, `RunContext`, and the fix protocol.
- `src/repomod/lint.py` runs actionlint and fix validation.
- `src/repomod/status.py` owns status, patches, logs, locks, and atomic YAML publication.
- `README.md` documents commands, stored output, reruns, and the fix interface.
- `docs/design.md` documents API budgets, interrupts, sync, status, and validation.

This preflight corrected these draft claims against the current code: per-fix clones use a flattened slug, fetch depth-one remote refs, switch with `--discard-changes`, reset hard, clean untracked files, and delete other local branches. Fresh clones already use `--depth=1 --filter=blob:none`. `lint.py` stashes changes to compare actionlint results. `main.py` keeps a 24-hour active-target cache and `sync` currently refreshes `repos.yaml` only. Linked worktrees are not supported by the current clone checks. Status publication already uses an atomic generation and cross-process lock.

## Decision log

The executor appends one short line for each judgment that changes the implementation or a contract, with the reason. Keep the owner decisions above as the authority. Do not record routine work or repeat a fact already in this plan.

## Campaign log

The executor appends one short line at each phase end. Record the items merged, focused checks, measurement result, and any item parked. The log is a handoff, not proof. Commands and test results remain the proof.

## For the owner afterwards

Review the final catalog defaults, README and design guide, persisted-state compatibility changes, and the closeout report. The integration branch stays local. The executor never pushes.

