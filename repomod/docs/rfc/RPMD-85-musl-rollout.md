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

Recheck these unresolved outcomes before acting:

- The `tm` canary and its prerequisites. The prior record points to musl PR #13 and CI PR #14, with the macOS test `every_effective_binding_from_a_second_session_leaves_the_first_session_alone` blocking qualification. Confirm their present status; do not assume the old failure persists.
- Conventional Rust consumer migration to the approved shared image. Confirm the `srv`, `tm`, and `fdu` canaries and the required image-mode linker traces before broader migration.
- The qualification result for `python-build` release run `41334317681`. The record did not contain its terminal outcome.
- Zig/`zzz` runtime and libc-input qualification. Static verification is separate from its host-dependent runtime smoke, which requires a real `/sys/power` environment.
- Whether the recorded diverged tansu PR #11 still needs any action.

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
