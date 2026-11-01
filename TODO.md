Prefix: STUF
Next: 55

### STUF-52 · OCI A11 decides the deployed 307 blob path

- **Area:** Performance
- **Where:** `crates/stuff-worker/entry.ts:549`, `docs/oci-benchmarks.md`, `deploy/test-oci-redirect.sh`
- **Problem:** The conditional `307` path is implemented and locally proven when `STUFF_OCI_BLOB_ORIGIN` is set, but the O2 gate needs the deployed warm-p95 and full-pull comparison. The loopback matrix cannot settle whether direct CDN blobs beat the Worker path by 10% with non-overlapping p95 over five interleaved runs.
- **Recommendation:** Run `deploy/oci-bench.sh` against the deployed Worker with and without `STUFF_OCI_BLOB_ORIGIN` and the blob cache rule; adopt the redirect if O2 passes, otherwise close A11 with the recorded numbers and remove the conditional path.
- **Blocked:** Needs the Cache Rule for `oci-blobs.17166969.xyz/sha256/*` and a representative image on the deployed registry; the buckets, Worker, and blob domain now exist. Unblock: run the baseline matrix without `STUFF_OCI_BLOB_ORIGIN` and the rule, then create the rule, set the origin, and run it again.
- **Fix with:** make-it-fast
- **Effort:** M

### STUF-53 · Provision and verify the live OCI registry

- **Area:** Operations
- **Where:** `docs/PUBLISHING.md`, `docs/oci-design.md` (Needs environment)
- **Problem:** The `stuff-oci` and `stuff-oci-blobs` buckets exist, `oci-blobs.17166969.xyz` is active on the blob bucket, the Worker is deployed with the OCI bindings and the `STUFF_OCI_PUSH_TOKEN` and `STUFF_OCI_PUSH_SECRET` secrets, and `example/app:stable` (generation `3789424fc0854a037de2fc87`, the fixture layout) is published; anonymous reads of tags, the index, a blob, and referrers match the pinned digests. A push with the right token returns Worker error 1101 ("OCI push writer is not configured") because there is no writer host or `STUFF_OCI_WRITER_ORIGIN`. Live conformance, Docker/ORAS interop, and the 1 GiB push/pull remain unrun.
- **Recommendation:** Start `deploy/myoci-writer.sh` with TLS on a host, move `~/.local/state/stuff/oci/{push-secret,r2-writer.env}` there, set `STUFF_OCI_WRITER_ORIGIN` on the Worker, then run `make smoke-deployed`, the live conformance commands, Docker/ORAS pulls and pushes, and the 1 GiB push/pull.
- **Blocked:** The writer host. Unblock: choose the host.
- **Fix with:** stuff-registries
- **Effort:** M

### STUF-54 · P3 · `deploy/publish oci` cannot make a first publication

- **Area:** Operations
- **Where:** `deploy/publish:200-246`, `deploy/r2-pull.py`
- **Problem:** When the local site is missing, the wrapper adopts the live publication. On an empty `stuff-oci` bucket there is no `current` marker, so adoption fails with `cannot read the current marker: HTTP 404` before anything uploads. The first production publish needed `myoci init`, `import`, and `publish` by hand, then `deploy/publish oci --republish`.
- **Recommendation:** Treat a bucket with no objects as a first publication: skip adoption and let `publish-myoci.sh` build from the imported layout. Cover it in `deploy/test-oci-publish.sh` with an empty simulated bucket and no seeded site.
- **Fix with:** stuff-registries
- **Effort:** S

### STUF-45 · P3 · mypypi sdist builds run third-party code with the publisher's filesystem

- **Area:** Security
- **Where:** `crates/mypypi/src/build.rs:488-510`, `crates/mypypi/src/process_policy.rs:15-72`, `deploy/publish-mypypi.sh:105`
- **Problem:** `uv build` runs the sdist's build backend with an allowlisted environment, scratch homes, and a process-group timeout, but no filesystem or network isolation: it runs as the publisher and can read `~/.ssh`, the `cf` keyring, and other credential files by absolute path. On macOS the build runs on the host. `--require-build-hashes` and `--build-constraints` exist but `publish-mypypi.sh` passes neither. Inputs are exact pins, so this is supply-chain exposure, not an open hole.
- **Recommendation:** Pass `--require-build-hashes` from the publish helper and decide whether macOS sdist builds move to a credential-free user or account.
- **Blocked:** `--require-build-hashes` without a hash-pinned build-constraints file fails every build (`Hash-checking is required, but no trusted hash was provided for: setuptools`), so turning it on needs a maintained constraints file; macOS isolation needs an owner decision on a credential-free user or account. Unblock: supply the constraints file and the account decision.
- **Fix with:** security
- **Effort:** S
