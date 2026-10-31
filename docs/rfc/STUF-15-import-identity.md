# STUF-15: Migrate the npm project import identity

Status: accepted

## Summary

Include the package constraints and project snapshot in the identity that
`mynpm` uses to decide whether a lockfile import is new. Decide whether the
first import after the change appends a duplicate project record or replaces
the existing record in place.

## Motivation

`project_stable_string` (`crates/mynpm/src/inventory.rs:1274`) decides whether
a lockfile is already imported. It includes each package's key, name, version,
integrity, resolved URL, and source kind. It omits `platform` (`os`, `cpu`,
`libc`), `flags`, `aliases`, and the project snapshot.

`merge_project` (`crates/mynpm/src/inventory.rs:1564`) returns the existing
record when the strings match. A lockfile regenerated with new platform
constraints or optional flags is therefore treated as unchanged, even though
`crates/mynpm/README.md:16-19` says a changed lockfile is recorded as a new
import. `plan` uses the stale platform to choose between hosted and
metadata-only versions (`crates/mynpm/src/acquire.rs:564`), and `validate`
uses the stale flags to require or skip packages
(`crates/mynpm/src/validate.rs:1132-1139`).

## Design

Add the omitted fields to the per-package tuple in `project_stable_string`:
`platform`, `flags`, and `aliases`, in their canonical serialized form. Add a
SHA-256 digest of the project snapshot alongside `pin`.

The string is not a wire format. It is a local identity used only by
`merge_project`, so changing it does not affect published packuments or
inventory schema versions. Changing it does make every existing project look
new on its next import.

The recommended migration is append-only. The inventory already records a
changed lockfile as an additional import under the same project id, and
`fetch` and `publish` deduplicate packages and artifacts by identity. The
first re-import after the upgrade would append one duplicate record per
project, then settle. Confirm that the catalog and doctor counts do not
double-count before releasing.

## Alternatives considered

- Replace the existing record when the project id matches and only the newly
  included fields differ. This avoids duplication but adds a migration rule
  that can discard a genuinely different import.
- Keep the identity unchanged and compare a separate constraints digest only
  when deciding hosted versus metadata-only. This leaves snapshot changes
  invisible and splits one identity across two comparisons.

## Open questions

Resolved by the decision below.

- Is a one-time duplicate import per project acceptable, or should the merge
  replace the old record in place?
- Does the inventory tolerate duplicate project records with identical
  packages without inflating catalog or `doctor` counts?

## Decision

Append-only. Add `platform`, `flags`, `aliases`, and a snapshot digest to
`project_stable_string`. The first re-import after the upgrade appends one
duplicate record per project. Verify before release that the inventory,
catalog, and `doctor` counts handle the duplicate records without inflating
package or version totals.
