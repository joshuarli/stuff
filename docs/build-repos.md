# Build repositories

A repository tagged `build` produces release archives of upstream software (CPython, Node.js,
CMake, FFmpeg, Git, OpenSSH, Chromium, Pi). This document is the contract those repositories share.
repomod checks it with the `build-release-standard`, `build-make-targets`, and `build-locks` rules.

## Make targets

The root `Makefile` defines `build`, `verify`, `lint`, and `clean`. `build` produces the release
archives for one target, `verify` checks them, `lint` runs the repository's static checks, and
`clean` removes build output. Optional steps use these names when they exist: `fetch` downloads
and verifies pinned inputs, `package` assembles archives from a finished build, and `repro`
rebuilds and compares archives byte for byte.

`TARGET` selects the target as a Rust target triple: `aarch64-apple-darwin`,
`x86_64-unknown-linux-musl`, or `aarch64-unknown-linux-musl`. No glibc targets are released.

## Pinned inputs

All three lock files are JSON objects with an integer `schema_version` of `1`.

`sources.lock.json` pins every upstream input the build downloads:

```json
{
  "schema_version": 1,
  "inputs": [
    {
      "name": "cpython",
      "version": "3.15.1",
      "url": "https://www.python.org/ftp/python/3.15.1/Python-3.15.1.tar.xz",
      "sha256": "673ae7232d84c894729882c8257d300ef2d26523911bfd3917ec8ce7f526f99b",
      "size": 35789408,
      "role": "source",
      "license": "Python-2.0",
      "purpose": "The distributed interpreter"
    }
  ]
}
```

Each input has a unique `name`, a `version`, a lowercase hex `sha256`, and either `url` (a string)
or `urls` (a non-empty list of mirrors serving identical bytes). `size`, `role`, `target`,
`license`, and `purpose` are optional; repositories may add other fields. Every build repository
has a `sources.lock.json`.

`release-budgets.json`, when present, has a `targets` object keyed by target triple; each value
holds that target's size or resource limits. `bootstrap.lock.json`, when present, pins the
toolchain used to build, and its other keys belong to the repository.

The shared Python package `laputa-buildlib` (repository `laputa-systems/buildlib`, published to
the stuff PyPI registry) reads and validates these files. Python build repositories depend on it
instead of keeping their own readers.

## Releases

`.github/workflows/release.yml` publishes a release. It runs only on `workflow_dispatch` and
starts with a preflight job that rejects any ref other than the default branch.

Each product and target produces two assets:

- `<product>-<version>-<target>.tar.xz`, the archive.
- `<product>-<version>-<target>.manifest.json`, with at least `schema_version` (1), `product`,
  `version`, `target`, `source_revision` (the full commit SHA), and `sources_lock_sha256` (the
  SHA-256 of `sources.lock.json` at that commit).

A repository whose consumers need another layout publishes those assets beside the standard ones
and names the consumer in its README. python-build keeps its python-build-standalone layout
(`cpython-<version>+<date>-<target>-install_only_stripped.tar.gz`) because `uv python install`
reads it.

A `SHA256SUMS` file covers every other asset, sorted by file name, and the workflow checks it with
`sha256sum -c` before publishing.

The tag is `release-${{ github.sha }}`. The workflow refuses to run when the tag exists, creates it
at `$GITHUB_SHA` through the GitHub API, writes notes with the shared `Write release notes` step,
and publishes with:

```sh
gh release create "$RELEASE_TAG" --target "$GITHUB_SHA" --prerelease --verify-tag \
  --notes-file "$RUNNER_TEMP/release-notes.md" <assets>
```

Linux archives are built in a musl builder that passes `linux-musl-toolchain`.

Other workflows (lint, tests, qualification, benchmarks) may exist. A workflow that mirrors a build
input, such as a bootstrap toolchain, publishes outside this contract and never uses a `release-` tag. `lint.yml` runs `make lint`.
