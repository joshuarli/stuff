# RPMD-85: Musl rollout scope and qualification

Status: draft

## Summary

Recheck the owner-run rollout work left after the musl policy implementation, then decide
which consumer migrations and environment proofs still need execution. The implementation is
present in repomod. External rollout completion was not verified in this review.

## Motivation

The closed campaign mixed implemented policy with later fleet work. Keeping its historical
status as current instructions would preserve stale branches, versions, and test results.
Deleting it without carrying forward the unfinished work would lose the canary gate and
explicit owner deferrals. The
[archived record](https://github.com/joshuarli/repomod/blob/94e8b757c16bbcae86eef7a69ce5e44886f0be06/campaign-oci.md)
is the source for the recovery pointers below, not a statement of current fleet status.

## Design

Use [publication and migration](../policy-development.md#publication-and-migration) for the
lasting procedure. Keep the published index digest distinct from a local platform manifest.
Qualify both Linux architectures, asset checksums, static ELF policy, linker traces for image
mode, and each application's runtime behavior before widening a conventional Rust rollout.
The verifier defect tracked in RPMD-77 must be resolved or independently accounted for before
its directory result is trusted as the release gate.

Rechecked 2026-11-11. These outcomes are still unresolved:

- The `tm` canary. Pull requests [#13](https://github.com/laputa-systems/tm/pull/13) and [#14](https://github.com/laputa-systems/tm/pull/14) are merged; neither is a musl migration (#13 applies repomod fixes, #14 installs the tmux reference before tests). The old macOS session test was not reached. Current `main` CI run [43127791466](https://github.com/laputa-systems/tm/actions/runs/43127791466) fails to compile `tm` on macOS and arm64 (`usize: From<NonZero<u16>>`, 73 errors), and the lint workflow fails. Do not treat `tm` as qualified.
- Conventional Rust consumer migration to the approved shared image. Code search found no `ghcr.io/joshuarli/musl-toolchain` consumers. `srv` and `fdu` main CI is green, and `tm` does not compile. Confirm image-mode linker traces before any migration. Publishing the LLVM 23.1.5 image is [STUF-58](../../TODO.md). The local `gh` token has no `write:packages` scope, and the saved `ghcr.io` docker credential is rejected.
- `python-build` release run [41334317681](https://github.com/laputa-systems/python-build/actions/runs/41334317681). Both musl builds and both Linux smokes succeeded, as did the macOS build and smoke. The run failed because GitHub release tag `20261101` already existed. That build was qualified; it was not a new publication.
- Zig/`zzz` runtime and libc-input qualification. Static verification is separate from its host-dependent runtime smoke, which requires a real `/sys/power` environment. This recheck did not run it.
- tansu pull request [#11](https://github.com/joshuarli/tansu/pull/11) is merged. It needs no further action.

Chromium and Node heavy proofs, and Chromium's manual adapter-lock work, were omitted at the
owner's request. Preserve that deferral unless the owner explicitly reopens it. Refresh stays
manual; the owner declined scheduling. This draft creates no authorization to publish an
image, change another repository, merge a pull request, or create a schedule.

For a reopened environment proof, confirm the current builder documentation before using these
recorded starting points:

| Builder | Recorded qualification entry points to confirm |
| --- | --- |
| python-build | `doctor`, `fetch`, `build`, and `qualify` for x86-64 and AArch64 musl |
| chromium-build | Its `release-linux` make target |
| node-build | `doctor`, `fetch`, `build`, `qualify`, and `check-budgets` |
| git-build | Its `git` target with `DOCKER_PLATFORM=linux/amd64` and `linux/arm64` |
| zzz | `make TARGET=x86_64-linux-musl CPU=x86_64_v3` and `make TARGET=aarch64-linux-musl CPU=cortex_a53` |

Heavy bespoke qualification belongs on an appropriate Linux host. These adapters retain their
own build systems; the shared Rust image is not a mandate to convert them.

## Alternatives considered

A broad fleet migration before rechecking the canaries repeats possibly completed work and
risks treating static checks as runtime qualification. Leaving the campaign's old status in
place has the same ambiguity. Closing the residual work by an explicit owner decision is
valid if shared-image consolidation no longer has enough value.

## Open questions

- Does the owner still want shared-image consolidation for all conventional Rust builders, or a smaller set? Recommendation: recheck the three canaries and inventory current consumers before choosing the remaining scope.
- Should the deferred Chromium/Node proofs be reopened? Recommendation: keep them deferred unless those builders are part of the chosen rollout.
- Which host will supply the remaining target-specific runtime proof? Recommendation: use an appropriate Linux environment and retain the actual artifact checks, not a new infrastructure project.

## Decision

Pending. Once the chosen rollout is completed or declined, retain only lasting operational
decisions in the policy guide and delete this RFC and its TODO ticket.
