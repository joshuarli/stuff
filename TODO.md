Prefix: STUF
Next: 58

### STUF-45 · P3 · mypypi sdist builds run third-party code with the publisher's filesystem

- **Area:** Security
- **Where:** `crates/mypypi/src/build.rs:488-510`, `crates/mypypi/src/process_policy.rs:15-72`, `deploy/publish-mypypi.sh:105`
- **Problem:** `uv build` runs the sdist's build backend with an allowlisted environment, scratch homes, and a process-group timeout, but no filesystem or network isolation: it runs as the publisher and can read `~/.ssh`, the `cf` keyring, and other credential files by absolute path. On macOS the build runs on the host. `--require-build-hashes` and `--build-constraints` exist but `publish-mypypi.sh` passes neither. Inputs are exact pins, so this is supply-chain exposure, not an open hole.
- **Recommendation:** Pass `--require-build-hashes` from the publish helper and decide whether macOS sdist builds move to a credential-free user or account.
- **Blocked:** `--require-build-hashes` without a hash-pinned build-constraints file fails every build (`Hash-checking is required, but no trusted hash was provided for: setuptools`), so turning it on needs a maintained constraints file; macOS isolation needs an owner decision on a credential-free user or account. Unblock: supply the constraints file and the account decision.
- **Fix with:** security
- **Effort:** S

### STUF-57 · P3 · OCI adoption under the local cf simulation stalls for over ten minutes

- **Area:** Deploy
- **Where:** `deploy/publish:207-258` (`adopt_oci`), `deploy/test-oci-publish.sh:107-110`
- **Problem:** In CI run 41935480336 (ubuntu-24.04, job `worker-runtime`), the final step of `deploy/test-oci-publish.sh`, `STUFF_STATE_HOME=$WORK/home-adopt deploy/publish oci --republish`, printed `adopting the live publication` and produced nothing for ten minutes before the job timed out. The earlier publish, republish, GC, tag-move and rollback steps of the same test against the same fixture finished in about two minutes together, so this is a stall, not slowness. The cause has not been located: the suspects are `r2-pull.py` listing or fetching the meta bucket through `cf --local`, the per-blob `cf r2 objects get` loop, or the republish that follows adoption. CI no longer runs the test (stuff `527abaf`); it needs workerd, which does not run on musl hosts.
- **Recommendation:** Run `deploy/test-oci-publish.sh` on macOS with `sh -x` on `deploy/publish`, find the step that stops making progress, and fix it with a failing reproduction first.
- **Fix with:** root-cause
- **Effort:** S
