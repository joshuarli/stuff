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

Register the rule in `rules/__init__.py`. Use an empty tag set for a global rule.
Role tags describe products (`tool`, `library`, `app`, etc.). Language tags (`rust`,
`typescript`, `python`, etc.) describe substantial maintained source. Publishing tags
(`crates-io`, `pypi`, `npm`) are explicit release-policy opt-ins; they are not languages.
Do not introduce platform tags or tags for incidental features or vendored dependencies.
The CLI treats multiple `--tag` filters as a union. See `docs/tagging.md`.

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

## Release contracts

`release-policy.toml` defines the supported release conventions. Publishing tags are explicit
in `catalog.toml`; the presence of a package manifest never opts a repository into publishing
policy.

### Cargo

Each crate owns a package version or explicitly inherits `workspace.package.version`. Do not
assume every member of a workspace shares one version. Check package name, version source,
license, repository, and README metadata. A missing repository URL can use the catalogued
GitHub URL. A conflicting existing URL needs review.

Use `cargo set-version --package NAME VERSION` to select the crate whose version changes.
Review the resulting workspace diff before publishing. A package that intentionally gets its
version from another source needs a concrete diagnostic and must keep its existing source.

### PyPI

Use `[project].version` as the static version source and `uv_build` as the preferred build
backend. `uv version --bump patch` is the standard bump operation. Dynamic versioning remains
valid when intentional, but repomod reports its source for review. The rule adds README
distribution metadata when the repository already has a README. It does not invent a PyPI
release workflow.

### npm

Keep one package manager lockfile and make `packageManager`, when present, match that lockfile.
Use the package manager's version command. For npm lockfiles, `package.json`, the lockfile root,
and the package-lock root entry must have the same version. repomod repairs only that
deterministic version drift. It does not rewrite workspace version ownership or binary Bun
lockfiles.

## Verify the rule

Add an offline fixture with a real policy violation and its expected finding. If the fixer is
safe, assert the repaired file, the cleared postcondition, and the one rule-specific commit.
Run the focused test, then `make test`, `uv run ruff check src tests`, and `uv run ty check`
when the change affects shared rule behavior.
