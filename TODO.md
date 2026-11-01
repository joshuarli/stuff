Prefix: STUF
Next: 57

### STUF-53 · Provision and verify the live OCI registry

- **Area:** Operations
- **Where:** `docs/PUBLISHING.md`, `docs/oci-design.md` (Needs environment)
- **Problem:** The `stuff-oci` and `stuff-oci-blobs` buckets exist, `oci-blobs.17166969.xyz` is active with its Cache Rule, the Worker is deployed with the OCI bindings, the push token and secret, and the blob redirect, and `example/app:stable` and `example/bench:stable` are published. Live smoke, the pinned read conformance suite, and Docker and ORAS pulls pass. A push with the right token returns Worker error 1101 ("OCI push writer is not configured") because there is no writer host or `STUFF_OCI_WRITER_ORIGIN`, so push conformance, Docker/ORAS pushes, and the 1 GiB push/pull remain unrun.
- **Recommendation:** Start `deploy/myoci-writer.sh` with TLS on a server, move `~/.local/state/stuff/oci/{push-secret,r2-writer.env}` there, set `STUFF_OCI_WRITER_ORIGIN` on the Worker, then run `deploy/oci-conformance.sh --push` against it, Docker and ORAS pushes, and the 1 GiB push/pull.
- **Blocked:** The writer host. Unblock: choose the host.
- **Fix with:** stuff-registries
- **Effort:** M

### STUF-55 · P3 · `deploy/publish oci` fails on layers over 300 MB

- **Area:** Operations
- **Where:** `deploy/publish:114-122`, `deploy/r2-sync-blobs.sh`
- **Problem:** The wrapper mints S3 credentials only for `stuff-oci`, in a subshell. `r2-sync-blobs.sh` then needs `R2_S3_*` for `stuff-oci-blobs` to upload a layer over the 300 MB single-PUT cap and stops with `R2_S3_ENDPOINT is not set`. Publishing the 1 GiB benchmark layer worked only after syncing blobs by hand with the writer key and then running `deploy/publish oci --republish`.
- **Recommendation:** Mint credentials for the blob bucket before the blob sync and for the metadata bucket before the metadata sync, or have `r2-sync-blobs.sh` mint its own as the other syncs do. Cover a layer over 300 MB in the offline publish test.
- **Fix with:** stuff-registries
- **Effort:** S

### STUF-56 · P4 · Two integrity checks have no test that isolates them

- **Area:** Quality
- **Where:** `crates/myoci/src/verify.rs:32-39`, `crates/stuff-server/src/serve.rs:230`
- **Problem:** The test renovation found mutants that no test kills. Disabling the manifest digest check or the manifest size check in `myoci verify` changes no result, because a later check rejects the same tampering. Disabling the `canon.starts_with(&route.canonical_site_dir)` check in `stuff-server` changes no result, because `has_symlink_component` rejects first. Each is either dead defense in depth or a check whose own failure no fixture reaches.
- **Recommendation:** For each, build the fixture that reaches only that check (a manifest whose bytes and descriptor disagree but whose later graph checks pass; a path that canonicalizes outside the site without a symlink component) and keep the check with its test, or delete the check if no such input exists.
- **Fix with:** test-audit
- **Effort:** S

### STUF-45 · P3 · mypypi sdist builds run third-party code with the publisher's filesystem

- **Area:** Security
- **Where:** `crates/mypypi/src/build.rs:488-510`, `crates/mypypi/src/process_policy.rs:15-72`, `deploy/publish-mypypi.sh:105`
- **Problem:** `uv build` runs the sdist's build backend with an allowlisted environment, scratch homes, and a process-group timeout, but no filesystem or network isolation: it runs as the publisher and can read `~/.ssh`, the `cf` keyring, and other credential files by absolute path. On macOS the build runs on the host. `--require-build-hashes` and `--build-constraints` exist but `publish-mypypi.sh` passes neither. Inputs are exact pins, so this is supply-chain exposure, not an open hole.
- **Recommendation:** Pass `--require-build-hashes` from the publish helper and decide whether macOS sdist builds move to a credential-free user or account.
- **Blocked:** `--require-build-hashes` without a hash-pinned build-constraints file fails every build (`Hash-checking is required, but no trusted hash was provided for: setuptools`), so turning it on needs a maintained constraints file; macOS isolation needs an owner decision on a credential-free user or account. Unblock: supply the constraints file and the account decision.
- **Fix with:** security
- **Effort:** S
