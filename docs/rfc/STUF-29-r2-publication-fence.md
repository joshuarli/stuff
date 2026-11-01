# STUF-29: R2 publication fence

Status: draft

## Summary

Every operation that changes a registry bucket should run behind one fence: publish, sync, promote, rollback, garbage collection, and adoption. Today only the three `publish-*.sh` helpers take a lock, and that lock lives on the publishing machine. This RFC proposes three changes. Upload the generation manifest last. Pin each promotion to the generation its build started from. Make gc and rollback take the same per-registry lock. A bucket-side lease would extend the fence across machines, if that case is real.

## Motivation

A bucket holds immutable generations under `generations/<id>/`, shared content-addressed stores (`blobs/`, `tarballs/`, `wheels/`), one `manifests/<id>.json` per generation, and a mutable `current` marker. Three writers change it without coordinating:

- **Publish** (`r2-sync.sh` through `r2-promote.py`) uploads objects, then the manifest, then verifies and moves `current`.
- **Garbage collection** (`r2-gc.py`) deletes generations outside its retention window, and every shared object that no retained manifest references.
- **Rollback** (`r2-rollback.sh`) verifies a target manifest and moves `current`.

Two failures follow, and both were reproduced on the repository's fake S3:

1. **gc deletes what a publish needs (STUF-29).** Keys upload in sorted order, so `manifests/<id>.json` lands before the `tarballs/` and `wheels/` objects that sort after it. Once the manifest exists, gc no longer sees the generation as in flight, and the default `GC_KEEP_NEWER=0` deletes it. gc also checks for concurrent change only once, before it starts deleting. A publish that promotes during the delete phase can end with `current` naming a generation whose objects are gone.
2. **A stale base wins (STUF-30).** No helper passes `--expected`, and S3 promotion reads the `If-Match` ETag after the upload. A publish rendered from old local state, whether from a second machine or a restored backup, replaces a newer live generation, and the next gc deletes the newer generation's objects.

## Design

1. **Manifest last.** `r2-sync.sh` uploads `manifests/<id>.json` after every other put and multipart upload has finished. A generation directory without a manifest then always means a publish in flight, and gc's existing in-flight rule protects it.
2. **Promote against the build's base.** Before a helper builds, it reads the generation the local site's `current` names. It passes that generation to `r2-sync.sh --expected`. Promotion then fails when the bucket has moved on, and the error tells the operator to adopt again. The existing `If-Match` write stays as the guard for the verification window.
3. **One lock per registry for every bucket writer.** `make gc`, `r2-rollback.sh`, manual `make sync`, and adoption all take the same `$STUFF_STATE_HOME/<registry>/publish.lock` that the helpers take. gc locks one bucket at a time, so `make deploy -j3` keeps its parallel republishes. gc re-reads `current` and the manifest set before each delete batch and stops if either changed.
4. **Cross-machine lease (optional).** A `lease` object in the bucket, written with `If-None-Match: *` and carrying holder and expiry, is taken by every writer above when S3 credentials are present. This item depends on the first open question.

## Alternatives considered

- **Leave gc unfenced and document "do not run during a publish".** This is the current state, and `docs/PUBLISHING.md` already says it. It does not protect a cron'd gc, or a publish started from another checkout.
- **Never delete shared objects.** This removes the gc race at the cost of unbounded storage growth, which `make gc` was added to stop.
- **Promote only with `--promote=s3`.** That alone does not fix the stale base, because the ETag is read too late. It also makes S3 credentials mandatory.

## Open questions

1. **Is publishing from more than one machine, or from CI, a real use case?** If yes, item 4 is required and STUF-30 becomes the first fix. If no, items 1 to 3 close both tickets for one machine, and docs should state the single-publisher assumption. Recommendation: assume one machine for now. Implement items 1 to 3 and defer the lease.
2. **Should `GC_KEEP_NEWER` default to 1 after a rollback?** Today gc deletes the generation you rolled back from, which makes roll-forward impossible. Options: keep 0 and document it, or have gc retain every generation newer than `current` until a flag drops them. Recommendation: retain newer generations by default. A rollback is rare, and the window it opens is cheap to keep.

## Decision

Pending owner review.
