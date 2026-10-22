Prefix: RPMD
Next: 38

### RPMD-35 · P2 · Batch generated-artifact ignore probes

- **Area:** Performance
- **Where:** `src/repomod/rules/consistency.py:333`, `src/repomod/rules/consistency.py:342`, `src/repomod/git.py:120`
- **Problem:** The global `generated-artifacts` rule checks each ignore probe separately. `root_ignore_matches` starts a Git process for each probe, up to thirteen times per repository, on every fleet lint.
- **Recommendation:** Send all probe paths through one `git check-ignore --stdin` call and map its results back to the probes.
- **Fix with:** make-it-fast
- **Effort:** S

### RPMD-36 · P2 · Skip the duplicate status pass in online sync

- **Area:** Performance
- **Where:** `src/repomod/sync.py:37`, `src/repomod/sync.py:40`, `src/repomod/sync.py:65`, `src/repomod/sync.py:68`, `src/repomod/git.py:189`
- **Problem:** For an existing checkout, `sync_one` runs `status` before the dry-run branch and repeats it under the repository lock. Even when GitHub's reported SHA already matches HEAD, the no-op path runs both status passes plus `git rev-parse HEAD`. Each status pass starts four Git subprocesses.
- **Recommendation:** Keep the first status pass for dry-run. For online sync, resolve the checkout path first and collect status once under the lock.
- **Fix with:** make-it-fast
- **Effort:** S

### RPMD-37 · P3 · Detect language tags without matching source

- **Area:** Policy
- **Where:** `src/repomod/rules/languages.py:521`, `src/repomod/rules/languages.py:596`, `tests/test_consistency.py:69`
- **Problem:** `_language_tag_findings` reports detected languages missing from catalog tags, but never reports language tags without matching first-party source. For example, a `typescript` tag with a `package.json` and no TypeScript source is accepted. The test suite covers the missing-tag direction only.
- **Recommendation:** Compare detected source languages with catalog language tags in both directions, retaining the binary-prerelease exception.
- **Fix with:** none
- **Effort:** S
