Prefix: RPMD
Next: 86

### RPMD-74 · P1 · Checkout removal can destroy unpublished branches and linked-worktree metadata

- **Area:** Data
- **Where:** `src/repomod/clean.py:28`, `src/repomod/clean.py:149`, `src/repomod/clean.py:155`, `src/repomod/sync.py:99`
- **Problem:** Archived sync and unforced GC decide whether to delete an entire repository from the checked-out branch's status and stash count. A clean main at origin/main passes even when another local branch holds unpublished commits. A disposable real-Git reproduction committed a feature branch, switched to main, and called `sync_one` with archived metadata: the result was `removed`, the checkout disappeared, and the remote lacked the feature commit. `gc_one(force=False)` also removed that state. A second fixture with a dirty registered linked worktree lost its shared Git metadata; its files survived but `git status` exited 128. The current guard cannot establish that the whole repository is disposable.
- **Recommendation:** Share a repository-wide removal inspection between archived sync and GC. Refuse ordinary removal when local refs lack verified published counterparts, recovery refs exist, or another registered worktree depends on the common Git directory. Treat unknown publication state as a blocker and retain the existing path, origin, dirty-tree, and stash checks. Add real-Git regression scenarios to the existing removal tests.
- **Fix with:** `durable-state`
- **Effort:** M (days)
- **Design:** [Checkout removal contract](docs/rfc/RPMD-74-checkout-removal.md)

### RPMD-75 · P1 · Git display configuration hides untracked files from deletion guards

- **Area:** Data
- **Where:** `src/repomod/git.py:260`, `src/repomod/git.py:408`, `src/repomod/clean.py:29`, `src/repomod/sync.py:99`
- **Problem:** Both safety status commands omit an explicit untracked-files mode, so Git honors `status.showUntrackedFiles=no`. In a real-Git fixture with that setting and a nonignored `uncommitted.txt`, explicit `git status --porcelain=v2 --untracked-files=all` reported the file while `repomod.git.status(...).dirty` was false. Actual archived sync then deleted the file without a force flag. GC uses the same false-clean result, and the fast-forward cleanliness recheck has the same configuration dependency. This is independent of other local branches or worktrees.
- **Recommendation:** Pass `--untracked-files=all` in both status commands used for safety decisions. Verify that local and global display settings cannot hide files from sync, GC, or fixer admission, using the existing local-Git test helpers.
- **Fix with:** `os-boundaries`
- **Effort:** S (hours)

### RPMD-76 · P1 · Dependency inventory serializes URL credentials into tracked data

- **Area:** Security
- **Where:** `src/repomod/dependencies/adapters/javascript.py:339`, `src/repomod/dependencies/adapters/javascript.py:517`, `src/repomod/dependencies/adapters/javascript.py:889`, `src/repomod/dependencies/adapters/zig.py:166`, `src/repomod/dependencies/snapshot.py:328`
- **Problem:** Some instance fields are redacted while other representations retain raw URLs. An npm v3 lock with `https://user:example-secret@example.com/pkg.tgz?token=example-query` preserved that complete URL in `Edge.constraint`; `_origin()` also retained userinfo in registry/repository URLs and encoded identity qualifiers. A hashless Zig URL produced the same raw constraint. These real-adapter outputs flow into the tracked aggregate inventory, extending any input credential exposure to another repository and its consumers. The probes used invented credentials; they do not establish that a real secret has already leaked.
- **Recommendation:** Apply the URL privacy contract to every persisted representation, including constraints, origins, qualifiers, and diagnostics. Assert that entire serialized npm and Zig records contain neither invented secret nor userinfo/query values. Preserve sanitized dependency facts, bump the parser version, and regenerate affected inventory. If an actual leaked credential is found during remediation, handle its revocation and historical exposure explicitly.
- **Fix with:** `security`
- **Effort:** M (days)

### RPMD-77 · P1 · Directory ELF verification skips artifacts that violate release policy

- **Area:** Build
- **Where:** `src/repomod/rules/static_elf.py:211`, `src/repomod/rules/static_elf.py:214`, `src/repomod/rules/static_elf.py:243`, `src/repomod/rules/binary_release.py:612`
- **Problem:** The directory scanner catches every `ValueError` from `check_elf` and skips the file. That exception also means a dynamic interpreter/dependency, GCC-built objects, unsupported machine, or malformed ELF. The generated release workflow verifies `dist` in this mode. A real subprocess running `render_script()` rejected the dynamic fixture and a GCC-comment ELF individually with exit 1, but accepted a directory containing both rejected files plus valid x86-64 and AArch64 fixtures with exit 0 and `verified 2 ELF artifacts`. Rejected binaries can therefore remain in the directory being packaged and published.
- **Recommendation:** Skip only non-ELF inputs; propagate every error after ELF magic is recognized. Preserve directory architecture coverage. Add a mixed valid/invalid directory regression and regenerate the image's shared verifier. Update the verifier marker/recognition or compare canonical script content so `_static_smoke_valid` rejects stale embedded copies; changing the script alone does not make those copies eligible for the existing fixer. Then refresh the affected workflows.
- **Fix with:** `error-model`
- **Effort:** S (hours)

### RPMD-78 · P1 · Failed OSV requests replace vulnerability evidence with fresh clean results

- **Area:** Errors
- **Where:** `src/repomod/audit.py:1322`, `src/repomod/audit.py:1325`, `src/repomod/audit.py:1333`, `src/repomod/dependencies/enrich.py:155`, `src/repomod/dependencies/enrich.py:160`, `src/repomod/dependencies/analysis.py:193`
- **Problem:** OSV transport errors, invalid JSON, and malformed/missing result rows become empty advisory lists. Enrichment persists those lists with the current successful-looking timestamp. With only `urllib.request.urlopen` forced to raise `OSError`, actual enrichment reported `checked=1`, `not_checked=0`, `vulnerable=0` and cached `vulns=[]`. A stale cache seeded with an advisory was also overwritten to freshly checked zero. The shared lookup feeds policy auditing as well as dashboard enrichment, so a failed security check can look clean for the cache lifetime.
- **Recommendation:** Preserve a failed/not-checked outcome from transport through both consumers. Validate result cardinality and row shape; only successful responses advance the vulnerability-check timestamp. Keep previous advisories on failed refreshes, label them stale, and expose the operational failure. Cover missing rows and invalid responses as well as transport failure.
- **Fix with:** `error-model`
- **Effort:** M (days)

### RPMD-79 · P1 · Dependency deduplication drops identities and leaves dangling graph edges

- **Area:** Data
- **Where:** `src/repomod/dependencies/adapters/actions.py:325`, `src/repomod/dependencies/adapters/actions.py:525`, `src/repomod/dependencies/adapters/containers.py:148`, `data/dependencies/v1/repos/laputa-systems/rustls.json:2805`
- **Problem:** Draft keys omit fields that form the final instance ID: Action subpaths and Docker registry qualifiers. Real extraction of `actions/cache/restore@v4` plus `actions/cache/save@v4` emitted one instance and two edges; the save target was absent and no issue was reported. Two `acme/tool:1` images from ghcr.io and quay.io behaved the same way. This has already corrupted tracked data: the rustls workflow edge for `google/oss-fuzz`'s `infra/cifuzz/actions/run_fuzzers` targets a missing instance, while the build action exists. A read-only integrity scan found one such dangling target in the current inventory.
- **Recommendation:** Deduplicate by complete canonical identity, including subpath and registry qualifiers. Validate that every emitted edge endpoint belongs to the emitted project/instance set before serialization. Add the two adapter regressions, bump `PARSER_VERSION`, and regenerate affected inventory so graph consumers no longer silently lose dependencies.
- **Fix with:** `make-invalid-states-unrepresentable`
- **Effort:** M (days)

### RPMD-80 · P2 · Vulnerability collection omits additional locked versions of a package

- **Area:** Data
- **Where:** `src/repomod/audit.py:653`, `src/repomod/audit.py:769`, `src/repomod/audit.py:1458`, `src/repomod/audit.py:1465`, `src/repomod/audit.py:1483`
- **Problem:** Cargo, uv, and pnpm vulnerability collection reduces each package name to its first locked version. Coexisting transitive versions are normal. Separate real lint fixtures contained `transitive@2.0.0` followed by `transitive@1.0.0`, with an OSV transport stub reporting an advisory only for 1.0.0. All three queried only 2.0.0 and returned no findings. The real resolver ran; the vulnerable tuple never reached it. Cargo already has an all-version reader, but this path does not use it.
- **Recommendation:** Collect every distinct ecosystem/name/version tuple for vulnerability checks, using the all-version TOML reader for Cargo and uv and equivalent pnpm behavior. Deduplicate only identical tuples. Keep direct-upgrade selection separate and prove the second-version advisory appears through `lint_repositories` for all three formats.
- **Fix with:** `single-source-of-truth`
- **Effort:** S (hours)

### RPMD-81 · P2 · Registered audit rules report clean when a lockfile cannot be parsed

- **Area:** Errors
- **Where:** `src/repomod/rules/audit_findings.py:24`, `src/repomod/rules/audit_findings.py:35`, `src/repomod/audit.py:1368`, `src/repomod/audit.py:1376`, `src/repomod/audit.py:1587`
- **Problem:** `audit-lock` checks existence through `_policy_findings` but does not retain collection errors. `audit-vuln` discards `_collect_locked`'s operational findings and returns early when the failed collection is empty. A valid Python manifest requiring requests plus `uv.lock` containing `this is not TOML` produced `[]` from real `lint_repositories(..., rule_ids=("audit-lock", "audit-vuln"))`; OSV was never called. The legacy `_audit_one` path preserves these errors, so the registered rules lost an existing contract.
- **Recommendation:** Have a shared cached collection outcome retain dependencies and read/parse failures. Make `audit-lock` report those operational failures and ensure `audit-vuln` cannot classify failed collection as an empty successful audit. Cover the CLI's operational exit 2 as well as the findings.
- **Fix with:** `error-model`
- **Effort:** S (hours)

### RPMD-82 · P2 · Fix publishing pushes a mutable ref after validating a different commit

- **Area:** Concurrency
- **Where:** `src/repomod/fix.py:558`, `src/repomod/fix.py:570`, `src/repomod/cmd/fix.py:163`
- **Problem:** `push_fix_branch` checks the branch against `branch_head`, queries the remote and rechecks status, then pushes the local branch name. Ordinary Git writers do not honor repomod's lock. A deterministic real-Git interleaving advanced that ref immediately before the actual push: the function returned `('pushed', None)` while the remote received the unvalidated commit. The caller can then open its PR and enable auto-merge. Ordinary `push_repository` already pushes its checked SHA and does not have this gap.
- **Recommendation:** Use the immutable `branch_head:refs/heads/{branch}` push refspec with normal fast-forward semantics. Reuse the existing concurrent-push fixture pattern to prove that moving the local branch cannot change the published object.
- **Fix with:** `lifecycle-concurrency`
- **Effort:** S (hours)

### RPMD-83 · P2 · Workflow settings fixes discard working-directory from flow mappings

- **Area:** Build
- **Where:** `src/repomod/rules/workflow_settings.py:223`, `src/repomod/rules/workflow_settings.py:231`, `src/repomod/rules/workflow_settings.py:243`
- **Problem:** `_ensure_defaults` replaces nonempty inline `defaults` and `run` values with empty block headers. The actual fixer transformed `defaults: {run: {shell: bash, working-directory: frontend}}` into a mapping containing only the required shell. Its own postcondition and actual Pinact validation both returned no findings. Subsequent `run: make test` now executes at the repository root, so a policy repair silently changes workflow behavior while passing its validator.
- **Recommendation:** Preserve all existing mapping fields when changing the shell, or return a manual finding for unsupported inline forms instead of a lossy plan. Cover both inline `defaults` and inline `run` mappings through the fixer and validator, asserting that the working-directory remains frontend.
- **Fix with:** `blast-radius`
- **Effort:** S (hours)

### RPMD-84 · P2 · Filtered dashboard renders unselected repositories without visibility checks

- **Area:** Data
- **Where:** `src/repomod/cmd/deps.py:119`, `src/repomod/cmd/deps.py:120`, `src/repomod/cmd/deps.py:122`, `src/repomod/dependencies/render.py:185`
- **Problem:** Rendering computes hidden names only within the selector result and removes those from the full inventory. Unselected managed repositories remain. A real CLI-handler/renderer fixture selecting only public `acme/public` queried visibility only for that repository but still generated private `acme/secret`'s page and package search entry. Counts and change lists can therefore disagree with the requested scope. Private structured metadata is intentionally allowed in the tracked inventory; this is a broken selection/visibility contract, not proof of a previously secret dataset being exposed.
- **Recommendation:** Construct every render input from the reported repository allowlist. Apply it consistently to pages, search, aggregates, changes, and status. Preserve intentional inclusion of a private repository when that repository is explicitly selected with `--repo`.
- **Fix with:** `none`
- **Effort:** S (hours)

### RPMD-85 · P3 · Reconcile the remaining musl rollout and environment qualification

- **Area:** Operability
- **Where:** `musl-toolchain.toml:132`, `src/repomod/cmd/musl.py:91`, `docs/policy-development.md:282`
- **Problem:** The closed musl implementation work left owner-run canaries, shared-image consumer migration, and some environment qualification unresolved. Those external outcomes were not rechecked in this repository review, so completion must not be inferred from the presence of the lock and image recipe. This carries forward existing deferred work rather than asserting a new fleet defect. The draft below preserves the recovery pointers and owner deferrals from the removed campaign record.
- **Recommendation:** Recheck the named external outcomes before deciding what remains. Agree on the conventional-consumer migration scope, then follow the publication and canary gate in the policy guide. Keep owner-deferred Chromium/Node work and scheduling out of the run unless explicitly reopened. Delete this item after the remaining scope is completed or deliberately declined and its durable decisions are recorded.
- **Fix with:** `none`
- **Effort:** M (days)
- **Design:** [Musl rollout scope and qualification](docs/rfc/RPMD-85-musl-rollout.md)
