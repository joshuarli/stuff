# STUF-45: Isolate Python package execution

Status: draft

## Summary

Run sdist build backends and native wheel import checks outside the publisher's credential-bearing account and filesystem view. Keep acquisition policy, validation, and authenticated R2 publication as separate stages.

## Motivation

`crates/mypypi/src/validate.rs` installs wheels and imports their modules. `crates/mypypi/src/process_policy.rs` replaces environment variables and scratch homes, but leaves the process's filesystem and network authority intact. `--allow-sdist` controls source builds; wheel-only publication still executes imported package code. STUF-45 records the source trace and an offline sentinel-read proof.

## Design

The publisher prepares exact inputs and starts a worker with a private workspace, explicit toolchain access, and an output directory. The worker receives no R2 credentials, `cf` keyring, SSH directory, publisher home, container socket, or writable host checkout. It runs both build backends and import checks under the same boundary. The publisher treats returned artifacts as untrusted, checks their identities and hashes, and performs the authenticated upload itself.

Use a dedicated unprivileged worker/container on Linux with restricted mounts. On macOS, prefer a separate account with private publisher files inaccessible to it, or a VM when account separation cannot meet that contract. A sandbox wrapper is an alternative only after the required negative tests pass for the actual toolchain. Environment filtering remains defense in depth, not the execution boundary.

Resolve build dependencies before offline execution where possible. Hash-pinned build constraints constrain dependency selection; they do not isolate the selected code. Maintain them as a separate input rather than turning on `--require-build-hashes` without a usable constraints file.

Acceptance requires hostile but harmless sdist and wheel fixtures that attempt to read a publisher-owned sentinel and modify a file outside the worker output. Both attempts must fail while a real native build and import check succeed. Confirm that the publisher alone can access R2 credentials and that worker failure leaves publication inactive.

## Alternatives considered

- **Keep scratch homes alone:** the sentinel probe demonstrates why this is insufficient.
- **Disable imports or source builds:** removes validation or supported inputs without defining the required execution boundary.
- **Use a VM for every host:** gives a clearer boundary, with additional toolchain and artifact-transfer work.

## Open questions

- Which macOS execution host is acceptable: separate account, verified sandbox wrapper, or VM? Recommendation: a separate account if permissions and toolchain access satisfy the negative tests; otherwise a VM.
- Must build backends access the network? Recommendation: allow acquisition in a separate stage and keep import validation offline; add narrowly defined build exceptions only for inputs that require them.
- Who maintains the hash-pinned build constraints? Recommendation: a checked-in input with an explicit refresh procedure before enabling mandatory build hashes.

## Decision

Pending. This RFC authorizes no credential changes or execution-host provisioning. Delete it when the chosen boundary is implemented and document the resulting workflow in `crates/mypypi/README.md` and `docs/PUBLISHING.md`.
