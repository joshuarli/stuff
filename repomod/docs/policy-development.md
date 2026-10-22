# Add a policy rule

Rules live in `src/repomod/rules/`. Keep a check read-only and return structured findings.
Put the rule in the existing module for its policy area unless it has a separate check and
fixer lifecycle.

## Define the rule

Add a function with the existing `Rule` protocol:

```python
def check(root: Path, repository: Repository) -> list[Finding]:
    ...


def fix(root: Path, repository: Repository) -> FixPlan:
    ...
```

Register the rule in `rules/__init__.py`. Use an empty `TagPredicate()` for a global rule.
Role tags describe products (`tool`, `library`, `app`, etc.). Language tags (`rust`,
`typescript`, `python`, etc.) describe substantial maintained source. Publishing tags
(`crates-io`, `pypi`, `npm`) are explicit release-policy opt-ins; they are not languages.
Do not introduce platform tags or tags for incidental features or vendored dependencies.
Use `Rule.applies_to=TagPredicate(all_of={...})` for conjunction and
`TagPredicate(any_of={...})` for alternatives. The CLI's multiple `--tag`
filters instead select repositories by union. See `docs/tagging.md`.

Each finding includes the repository, rule ID, file when known, message, severity, and
fixability. Keep messages concrete enough to explain the next step. A missing optional tool or
failed API call is an operational finding, not a policy violation.

## Add a fixer

Return a deterministic mapping of repository-relative paths to complete UTF-8 file contents.
Do not edit files, stage paths, or commit inside the rule. Declare the smallest safe path
patterns in `allowed_paths`. Give each rule its own Conventional Commit subject.

The fix runner validates the path, runs the optional rule validator, re-runs the check, and
commits only that rule's staged files. A failed fixer loses only its uncommitted changes in the
disposable worktree. The canonical checkout is updated only after all independent fixers
finish and the starting checkout still matches.

Use a fixer only when it can make a deterministic change. Do not generate placeholder
documentation or rewrite human release choices. If only part of a violation is safe, split
that part into a rule with a precise postcondition.

Before a fixer commits, repomod compares policy findings before and after the file plan. A new
error finding rejects the fixer even if its own finding disappeared. Keep checks deterministic
and report operational failures as operational findings so an existing unavailable tool does
not look like a new source violation.

## GitHub Actions

`actions.toml` records each approved semantic version, major channel, and verified commit SHA.
`pins refresh` resolves tags without crossing the approved major by default. A workflow whose
reference or version comment declares another major gets a non-fixable review finding. A stale
SHA without a version comment, an unknown action, dynamic reference, or conflicting version
labels also fails closed. `--allow-major` changes the central channel only when explicitly
requested. Node 24 actions require self-hosted GitHub runners version 2.327.1 or newer.

The approved channels keep `setup-node` on v6 because v7 removes the dummy
`NODE_AUTH_TOKEN` fallback and the deprecated `always-auth` input. `setup-python` stays on v6
because v7 removes `pip-install`. `github-script` stays on v8 because v9 changes `getOctokit`
and breaks CommonJS `require('@actions/github')` scripts. These majors need an explicit workflow
migration. `actions/cache` is on v6: its release notes describe ESM and dependency updates, with
no public input or output change. Artifact actions use their current v7/v8 channels, which
include retry handling for HTTP 429 responses.

## Release contracts

`release-policy.toml` defines the supported release conventions. Publishing tags are explicit
in `catalog.toml`; the presence of a package manifest never opts a repository into publishing
policy.

### Cargo

Each publishable crate owns a package version or explicitly inherits
`workspace.package.version`; unpublished members (`publish = false` or
`publish = []`) are excluded. Independently versioned workspace packages keep
their own version. Check package name, version source, license, repository, and
README metadata. A missing repository URL is safely inferred from the catalog;
a conflicting existing URL needs review.

Keep one version-controlled script under `tools/bump_minor_release.sh` or
`scripts/bump_minor_release.sh`. It must identify the crate with Cargo owner and
search checks, run `cargo publish --dry-run`, then commit and tag the selected
version. The representative repositories use manual scripts, so this policy
does not demand a hosted release workflow or run the script during lint/fix.

### PyPI

Use `[project].version` as the static version source and `uv_build` as the preferred build
backend. `uv version --bump patch` is the standard bump operation. Explicit dynamic versioning
is retained and reported for review rather than rewritten. The rule checks `uv.lock` Python
range coherence and adds README distribution metadata when a README already exists. It does not
invent a PyPI release workflow; wheel prereleases do not activate native binary role policy.

### npm

Keep one package manager lockfile and make `packageManager`, when present, match that lockfile.
Use the package manager's version command. Public root and workspace packages need explicit
versions. For npm v2/v3 lockfiles, check each public workspace entry against its manifest;
repomod repairs deterministic single-package root version drift only. Private workspace roots
are excluded when they contain no public package. It does not rewrite workspace version
ownership or binary Bun lockfiles.

## Verify the rule

Add an offline fixture with a real policy violation and its expected finding. If the fixer is
safe, assert the repaired file, the cleared postcondition, and the one rule-specific commit.
Run the focused test, then `make test`, `uv run ruff check src tests`, and `uv run ty check`
when the change affects shared rule behavior.

## Repository consistency checks

`markdown-links` checks simple repository-local links in root README files and Markdown under
`docs/`. It skips external URLs, anchors, and links inside code. It reports missing targets and
paths that resolve outside the checkout.

`language-tags` warns when a supported first-party source layout clearly contains Rust,
TypeScript, JavaScript, Python, Go, Swift, C, C++, Zig, shell, or XSH source without the matching
catalog tag. It ignores common generated, vendored, and test directories. A recognized binary
prerelease repository tagged only `tool` is exempt because its classification intentionally omits
language tags.

`workflow-make-targets` checks literal `make target` commands in workflows against explicit
targets in the root `Makefile`. It skips workflows with `working-directory`, included or
conditional Makefiles, dynamic target names, and `make -C` or `make -f` invocations.

`generated-artifacts` composes a common ignore profile with profiles detected from first-party
source. It checks the root `.gitignore` for effective coverage, independent of user-global Git
excludes, and reports tracked cache or build outputs. It does not rewrite ignore files. It
does not treat generic `build/` or `dist/` paths as generated because projects may use them as
source or distribution content.
