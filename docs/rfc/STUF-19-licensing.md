# STUF-19: Ship license texts with release artifacts

Status: accepted

## Summary

Distribute the Apache-2.0 license and the imported MIT notice with every
release artifact, and correct the copyright line in `crates/mycrates/LICENSE`.

## Motivation

Every first-party crate declares `license = "Apache-2.0"`, and the imported
`h12tiny-*` crates declare MIT. The release workflow packages only the `stuff`
binary in each zip (`.github/workflows/release.yml:107`) and attaches only the
zips and `SHA256SUMS` (`:193`). The repository root has no `LICENSE`. The one
license file, `crates/mycrates/LICENSE`, ends with
`Copyright 2022 Functional Software, Inc. dba Sentry`, which names the wrong
project.

Apache-2.0 section 4 requires a recipient of the work to receive a copy of the
license. The MIT license requires its copyright and permission notice to
accompany copies or substantial portions. Linking h12tiny into `stuff` makes
the released binary a substantial portion.

## Design

Add a root `LICENSE` containing the Apache-2.0 text. Add a root `NOTICE` with
the h12tiny MIT copyright and permission notice. Copy both files into
`dist/` before zipping the binary, and attach them to the release. Correct the
copyright stanza in `crates/mycrates/LICENSE` to the project's actual holder
and year.

The owner must confirm the copyright holder and year for the first-party
code, and the copyright line for the imported h12tiny code. This checkout does
not contain an h12tiny license file or a copyright header, so the notice text
must come from the upstream project.

## Alternatives considered

- Attach the license files only as release assets and leave them out of the
  zips. This works for a reader who opens the release page, but a copied zip
  still lacks the texts.
- Keep the license statement in `README.md` only. Prose is not a copy of the
  license and does not satisfy either license.

## Open questions

Resolved by the decision below.

- Who is the copyright holder, and which year goes on the first-party
  license?
- What copyright line belongs in the h12tiny MIT notice?
- Should the h12tiny MIT text also be added under `crates/h12tiny-*`, or is
  the root `NOTICE` sufficient for the distributed binary?

## Decision

Accepted with the following texts and placement.

- Root `LICENSE`: the Apache-2.0 text, with `Copyright 2026 Joshua Li` in the
  appendix.
- Root `NOTICE`: the MIT text with `Copyright (c) 2026 joshuarli`, naming the
  imported `h12tiny-*` crates. The upstream license at
  `https://github.com/laputa-systems/h12tiny` carries that line.
- Correct `crates/mycrates/LICENSE` from the Sentry line to
  `Copyright 2026 Joshua Li`.
- Copy `LICENSE` and `NOTICE` into `dist/` before zipping, so each
  `stuff-<target>.zip` carries both. No separate per-crate MIT file is
  required for the binary release.
