Prefix: STUF
Next: 54

### STUF-52 · OCI A11 decides the deployed 307 blob path

- **Area:** Performance
- **Where:** `crates/stuff-worker/entry.ts:549`, `docs/oci-benchmarks.md`, `deploy/test-oci-redirect.sh`
- **Problem:** The conditional `307` path is implemented and locally proven when `STUFF_OCI_BLOB_ORIGIN` is set, but the O2 gate needs the deployed warm-p95 and full-pull comparison. The loopback matrix cannot settle whether direct CDN blobs beat the Worker path by 10% with non-overlapping p95 over five interleaved runs.
- **Recommendation:** Run `deploy/oci-bench.sh` against the deployed Worker with and without `STUFF_OCI_BLOB_ORIGIN` and the blob cache rule; adopt the redirect if O2 passes, otherwise close A11 with the recorded numbers and remove the conditional path.
- **Blocked:** Needs provisioned buckets, the deployed Worker, the `oci-blobs` custom domain, and a Cache Rule. Unblock: run the deployed matrix.
- **Fix with:** make-it-fast
- **Effort:** M

### STUF-53 · Provision and verify the live OCI registry

- **Area:** Operations
- **Where:** `docs/PUBLISHING.md`, `docs/oci-design.md` (Needs environment)
- **Problem:** The OCI campaign is locally green. The `stuff-oci` and `stuff-oci-blobs` buckets exist, `oci-blobs.17166969.xyz` is attached to the blob bucket, and the push token, push secret, and a `stuff-oci-writer` R2 key scoped to `stuff-oci-blobs` are minted. The Worker secrets and `oci` domain deploy, the writer host, production publication, live read/push conformance, and Docker/ORAS interop remain unrun.
- **Recommendation:** Set the Worker secrets, start `deploy/myoci-writer.sh` with TLS, run `deploy/publish oci`, then run `make deploy`, `make smoke-deployed`, the live conformance commands, and the 1 GiB push/pull.
- **Blocked:** The writer host and the Worker secrets. Unblock: choose the host, set the secrets, then run the deployment checklist.
- **Fix with:** stuff-registries
- **Effort:** M

### STUF-45 · P3 · mypypi sdist builds run third-party code with the publisher's filesystem

- **Area:** Security
- **Where:** `crates/mypypi/src/build.rs:488-510`, `crates/mypypi/src/process_policy.rs:15-72`, `deploy/publish-mypypi.sh:105`
- **Problem:** `uv build` runs the sdist's build backend with an allowlisted environment, scratch homes, and a process-group timeout, but no filesystem or network isolation: it runs as the publisher and can read `~/.ssh`, the `cf` keyring, and other credential files by absolute path. On macOS the build runs on the host. `--require-build-hashes` and `--build-constraints` exist but `publish-mypypi.sh` passes neither. Inputs are exact pins, so this is supply-chain exposure, not an open hole.
- **Recommendation:** Pass `--require-build-hashes` from the publish helper and decide whether macOS sdist builds move to a credential-free user or account.
- **Blocked:** `--require-build-hashes` without a hash-pinned build-constraints file fails every build (`Hash-checking is required, but no trusted hash was provided for: setuptools`), so turning it on needs a maintained constraints file; macOS isolation needs an owner decision on a credential-free user or account. Unblock: supply the constraints file and the account decision.
- **Fix with:** security
- **Effort:** S
