# Repository tagging semantics

The catalog is a **curated policy manifest**, not a collection of free-form descriptive
GitHub topics. Only uncommented repository entries are managed. Preserve commented-out
entries and their reasons verbatim. `topics apply` publishes the *exact* curated tag
array to GitHub, so avoid incidental keywords.

## Three independent dimensions

**Role** answers what the repository delivers:

- `tool`: command-line program or directly installed executable.
- `library`: independently reusable API/SDK, whether publicly published or internal.
- `app`: substantial end-user application or game with an application lifecycle/UI.
  An app can launch an executable or local HTTP server without becoming a `tool`.
- `extension`: browser/editor extension.
- `system`: operating system, libc, runtime, compiler, or system-level project.
- `build`: toolchain or artifact-building project.
- `config`: dotfiles, workstation, or development configuration.
- `data`: repository primarily containing data.

Require at least one role. Multiple roles are justified when a repository contains
independently useful products, not merely a test helper binary inside a library.
Public publication is not required for `tool` or `library`.

**Language** records substantial, actively maintained first-party source:
`rust`, `typescript`, `python`, `go`, `swift`, `c`, `cpp`, `javascript`,
`zig`, `shell`, and `xsh`. Multiple languages are normal. Do not infer
language tags from vendored code, generated bindings, dependency toolchains, or
a lone test fixture. Manifest and file-based detection activates language checks even when a
tag is deliberately omitted. A language tag with no corresponding project manifest remains a
catalog/policy mismatch.

**Publishing** tags `crates-io`, `pypi`, `npm` opt into *maintained package
release conventions*. Merely consuming, building, or mirroring a package in
an ecosystem does not justify its publishing tag. A wheel prerelease also does
not mean an executable binary release.

## Binary-prerelease override

The standard GitHub binary prerelease workflow builds native executables and
publishes them as GitHub release assets, typically with a SHA-derived
`release-$GITHUB_SHA` tag and `gh release create ... --prerelease`.
For repositories adopting **that specific distribution contract**, the full
curated catalog tag set is exactly `["tool"]` — no secondary role, language,
or publishing topics. Ecosystem linting must detect source manifests rather
than relying on language tags for these repositories.

Representative binary-prerelease tools: `joshuarli/ish`, `joshuarli/gh`,
`joshuarli/mactop`, `laputa-systems/srv`, `laputa-systems/e`, and
`laputa-systems/diff-pretty`.

**Do not detect this solely by workflow filename or a substring match for
`--prerelease`.** For example, `joshuarli/pytest-parallel` uses GitHub
prerelease machinery to distribute Python wheels (`dist/*.whl`) and remains
`["library", "pypi", "python"]`. A legitimate binary-release classifier must
check the release intent and artifact type, and report uncertain cases instead
of silently retagging repositories. `binary-release-tags` reports drift from
the exact role set and emits a warning when a prerelease workflow does not match
the supported native-artifact contract.

## Specific examples

| Repository | Curated tags | Reason |
| --- | --- | --- |
| `joshuarli/tansu` | `app`, `rust`, `typescript` | Rust backend and substantial TypeScript application frontend, no canonical binary prerelease |
| `joshuarli/shadowdriver` | `library`, `tool`, `rust`, `typescript` | Reusable Rust and TypeScript APIs plus independently useful private `grab` and `ig` tools |
| `joshuarli/defuddle-rs` | `library`, `tool`, `rust` | Public library API and CLI, without the standard binary-prerelease contract |
| `laputa-systems/srv` | `tool` | Binary-prerelease distribution, not a `server` topic |
| `joshuarli/pytest-parallel` | `library`, `pypi`, `python` | PyPI pytest plugin; wheel prerelease is not binary-tool publication |
| `laputa-systems/h12tiny` | `library`, `crates-io`, `rust` | Published, maintained Rust API |
| `laputa-systems/laputa` | `system`, `xsh` | OS distribution authored chiefly in XSH |
| `joshuarli/manga` | `data` | Data files, not executable code |

## Policy-selection semantics

A rule for Rust libraries must require **both** `library` and `rust`,
not either. A release rule is gated by an explicit publishing tag, not
`rust`/`python`/`typescript` alone. `--tag` filters select a union of
repositories matching the requested tags; this user filter is distinct
from the predicate determining whether an individual policy rule applies.

Rules use a small `TagPredicate`: `all_of` requires every listed tag, while
`any_of` requires at least one listed tag. A Rust library rule therefore uses
`all_of={"library", "rust"}`. An empty predicate applies globally. The CLI's
`--tag` filters separately select repositories by union, so `--tag tool
--tag app` includes repositories carrying either tag without changing which
rules apply to each selected repository.

Do not alter commented-out catalog entries, automatically replace GitHub
topics, or rewrite a role based only on the presence of a workflow file.
