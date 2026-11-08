# Repository tagging semantics

The catalog is a curated policy manifest, not a set of descriptive GitHub topics. Only active
repository entries are managed. Pending and ignored entries are not managed. `topics apply`
publishes the exact catalog tag set.

`IGNORE` excludes a repository from management and discovery. It is not a GitHub topic.
Other tags may remain on an ignored repository for later use.

No tag marks a repository as private. GitHub's visibility decides, and repomod caches it for an hour
in `~/.cache/repomod/visibility.json`. Reporting commands (`status`, `lint`, `deps report`,
`deps why`, and `deps render`) leave private repositories out unless given `--include-private` or
the repository by `--repo`. `sync`, `fix`, `push`, `merge`, `topics`, `deps snapshot`, `deps check`,
and `deps enrich` still cover them. A repository GitHub cannot show is treated as public, so its
error still appears. Offline reporting needs a cached visibility and fails without one.

Each repository needs at least one role tag:

- `tool`: command-line program or directly installed executable.
- `library`: independently reusable API or SDK, whether public or internal.
- `app`: substantial end-user application or game. Launching an executable or local server
  does not by itself make an app a tool.
- `extension`: browser or editor extension.
- `system`: operating system, libc, runtime, compiler, or system-level project.
- `build`: toolchain or artifact-building project.
- `cloudflare`: project substantially deploying an application or service on Cloudflare.
- `config`: dotfiles, workstation, or development configuration.
- `data`: repository primarily containing data.

Use multiple roles when the repository contains independently useful products, not just a
test-helper binary inside a library. Public release is not required for `tool` or `library`.

Language tags describe substantial, maintained first-party source: `c`, `cpp`, `go`,
`javascript`, `python`, `rust`, `shell`, `swift`, `typescript`, `xsh`, and `zig`. Do not tag
vendored, generated, dependency, or incidental test code. Rust, JavaScript, TypeScript, and
Python project checks inspect supported manifests even when the matching language tag is absent.
The language-tag rule warns when detected source lacks a matching tag or a language tag has no
detected source. Detection starts from managed project manifests (`Cargo.toml`, `go.mod`,
`package.json` or `tsconfig.json`, `pyproject.toml`, `Package.swift`, `build.zig`) and the
first-party source roots beside them, such as `src`, `cmd`, `ts`, and `Sources`. Shell, C, and
C++ files, and Python files outside the repository root, `build`, and `buildsys`, count only in a
repository with no managed manifest of another ecosystem, so helper scripts in a Rust, Go,
TypeScript, Swift, or Zig project are not project languages. A `pyproject.toml` makes Python a
managed ecosystem. Repositories tagged `fork` use only root
manifests, because their upstream layout is outside local policy. A Rust executable requires
`Cargo.lock` at the root of its own Cargo workspace.

Publishing tags `crates-io`, `pypi`, and `npm` opt a repository into that ecosystem's release
policy. Package use, mirroring, or a manifest alone does not opt in.

The dependency inventory uses `crates-io` on a Cargo repository and `pypi` on a Python
repository as first-party publishing evidence: a registry component whose name equals one of
that repository's own package names links to the repository. Native Cargo registry components
and path or workspace sources inside the repository link the same way. A Git source links only
when its URL normalizes to a catalogued repository; a similar name never links. The inventory
does not change tags.

Platform tags `linux` and `macos` declare a single-OS release scope for a `tool`. A tool
without a platform tag is portable and releases for all supported targets. `mactop` is
macOS-only, so it carries `macos` with its implementation language. `zzz` is Linux-only,
so it carries `linux`.

The `fork` tag marks repositories whose upstream GitHub workflows are outside local policy.
Fork workflow checks and status include only files named `lint`, `prerelease`, or `test` under
`.github/workflows/`. `status` also reports how many commits each fork is behind its upstream
parent, regardless of tagging, whenever GitHub reports a parent.

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
Private repositories are exempt from the prerelease policy. `fork` repositories
defer matrix and format standardization while their upstream workflows stay outside
local policy. The `binary-release-tags` rule checks the artifact and release contract
and curated tags. It reports ambiguous workflows for review, except in repositories tagged
`extension` or `build`, which publish plugin archives or build archives rather than native tools.
A workflow in those repositories that does establish the binary contract still requires the
`tool` role and allows only `fork`, platform, and language tags beside it, so `extension` or
`build` on such a repository is reported. The
`binary-release-standard` rule checks the target matrix and ish format for non-fork
tools. The `linux-musl-toolchain` rule checks every musl builder against
`musl-toolchain.toml`: the Alpine base digest, exact direct package versions, checksummed
toolchain archives, the Rust release-builder contract, and the bespoke builder adapters. It
applies to every repository and returns no findings when no musl builder exists. `fork` and
`macos` repositories and private repositories are skipped.

`binary-release-static` also checks fork workflows that publish tool prereleases. It checks
downloaded Linux ELF binaries before packaging and publication with the shared verifier in
`src/repomod/rules/static_elf.py`. The verifier rejects ELF interpreters and shared-library
dependencies and, when scanning a directory, requires one binary for each Linux architecture in
`release-policy.toml`. Image-mode builders must also pass a linker trace that names the builder's
`/usr/lib/libc.a` and CRT paths and rejects a rustup self-contained or glibc path. It skips tools
tagged `macos` and private repositories.

`registry-sources` skips repositories tagged `fork`, whose upstream manifests and lockfiles stay
outside local policy.

## Rule selection

Rule predicates and repository filters are separate. `TagPredicate(all_of={"library", "rust"})`
requires both tags. `any_of` accepts any listed tag. Multiple CLI `--tag` filters select the
union of repositories carrying those tags and do not change rule applicability.
