# Repository tagging semantics

The catalog is a curated policy manifest, not a set of descriptive GitHub topics. Only active
repository entries are managed. Preserve commented entries and their reasons. `topics apply`
publishes the exact catalog tag set.

Each repository needs at least one role tag:

- `tool`: command-line program or directly installed executable.
- `private-tool`: an executable maintained for private use; GitHub prerelease policy does not apply.
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
vendored, generated, dependency, or incidental test code. Rust, JavaScript, TypeScript, and
Python project checks inspect supported manifests even when the matching language tag is absent.
The language-tag rule warns when detected source lacks a matching tag or a language tag has no
detected source. Detection follows the supported source layouts and manifest requirements.

Publishing tags `crates-io`, `pypi`, and `npm` opt a repository into that ecosystem's release
policy. Package use, mirroring, or a manifest alone does not opt in.

Platform tags `linux` and `macos` declare a single-OS release scope for a `tool`. A tool
without a platform tag is portable and releases for all supported targets. `mactop` is
macOS-only, so it carries `macos` with its implementation language. `zzz` is Linux-only,
so it carries `linux`.

The `fork` tag marks repositories whose upstream GitHub workflows are outside local policy.
Fork workflow checks and status include only files named `lint`, `prerelease`, or `test` under
`.github/workflows/`. `status` also reports how many commits each fork is
behind its upstream parent, regardless of tagging, whenever GitHub reports a parent.

## Binary prereleases

The supported native binary prerelease workflow follows `ish`: a manual dispatch guarded by
a preflight default-branch check, a build matrix over exactly the scope's targets, and
per-platform zip archives with a verified `SHA256SUMS` file published under a
SHA-derived tag targeting the source commit. The tag is created at the triggering commit
through the GitHub API and `gh release create` passes `--verify-tag`. Portable tools
build `aarch64-apple-darwin`, `x86_64-unknown-linux-musl`, and
`aarch64-unknown-linux-musl`. macOS-only tools build only `aarch64-apple-darwin`.
Linux-only tools build the musl targets for both `x86_64` and `aarch64`. No glibc
targets are supported. The expected target sets live in the `[binary]` section of
`release-policy.toml`.

Repositories using that contract carry the `tool` role with only `fork`, platform, and
language tags. A wheel prerelease is a package release, not a binary-tool release.
`private-tool` repositories are exempt from the prerelease policy. `fork` repositories
defer matrix and format standardization while their upstream workflows stay outside
local policy. The `binary-release-tags` rule checks the artifact and release contract
and curated tags. It reports ambiguous workflows for review. The
`binary-release-standard` rule checks the target matrix and ish format for non-fork
tools. The `rust-musl-image` rule checks the Alpine musl static builder image behind
Rust tool releases against the ish Dockerfile.

## Rule selection

Rule predicates and repository filters are separate. `TagPredicate(all_of={"library", "rust"})`
requires both tags. `any_of` accepts any listed tag. Multiple CLI `--tag` filters select the
union of repositories carrying those tags and do not change rule applicability.
