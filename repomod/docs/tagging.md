# Repository tagging semantics

The catalog is a curated policy manifest, not a set of descriptive GitHub topics. Only active
repository entries are managed. Preserve commented entries and their reasons. `topics apply`
publishes the exact catalog tag set.

Each repository needs at least one role tag:

- `tool`: command-line program or directly installed executable.
- `library`: independently reusable API or SDK, whether public or internal.
- `app`: substantial end-user application or game. Launching an executable or local server
  does not by itself make an app a tool.
- `extension`: browser or editor extension.
- `system`: operating system, libc, runtime, compiler, or system-level project.
- `build`: toolchain or artifact-building project.
- `config`: dotfiles, workstation, or development configuration.
- `data`: repository primarily containing data.

Use multiple roles when the repository contains independently useful products, not just a
test-helper binary inside a library. Public release is not required for `tool` or `library`.

Language tags describe substantial, maintained first-party source: `c`, `cpp`, `go`,
`javascript`, `python`, `rust`, `shell`, `swift`, `typescript`, `xsh`, and `zig`. Do not tag
vendored, generated, dependency, or incidental test code. Source-manifest checks can apply
without a language tag; a language tag with no matching project is a catalog mismatch.

Publishing tags `crates-io`, `pypi`, and `npm` opt a repository into that ecosystem's release
policy. Package use, mirroring, or a manifest alone does not opt in.

## Binary prereleases

The supported native binary prerelease workflow has a manual dispatch, builds and transfers
native artifacts, and publishes executable release assets under a SHA-derived tag targeting
the source commit. Repositories using that contract have exactly the `tool` tag, regardless of
their implementation languages. A wheel prerelease is a package release, not a binary-tool
release. The `binary-release-tags` rule checks the artifact and release contract; ambiguous
workflows are reported for review.

## Rule selection

Rule predicates and repository filters are separate. `TagPredicate(all_of={"library", "rust"})`
requires both tags; `any_of` accepts any listed tag. Multiple CLI `--tag` filters select the
union of repositories carrying those tags and do not change rule applicability.
