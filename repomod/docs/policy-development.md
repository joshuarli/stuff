# Policy development

Rules live in `src/repomod/rules/` and are registered in `rules/__init__.py`. Keep checks
read-only and return structured `Finding` values. Give each rule a stable ID and an explicit
`TagPredicate`; see [tagging semantics](tagging.md) for tag classification and filter behavior.

Operational problems such as an unavailable tool or failed API call are operational findings,
not policy violations. Keep messages concrete and findings deterministic.

## Fixers

A fixer returns a mapping of repository-relative paths to complete UTF-8 file contents. It does
not edit files, stage changes, or commit. Declare the smallest safe `allowed_paths`; use a
rule-specific Conventional Commit subject. Add a validator when the planned files need a
rule-specific check. The runner rejects out-of-scope paths, newly introduced policy errors, and
plans that fail validation or the rule's post-check. A successful fixer that changes files gets
one local commit.

Use fixers only for deterministic repairs. Keep human release choices out of automatic fixes.
Do not generate placeholder README, license, or agent files.

## Release policy

`actions.toml` is the source of truth for approved GitHub Action versions, major channels, and
commit SHAs. `release-policy.toml` defines supported package release conventions. Publishing
checks require explicit `crates-io`, `pypi`, or `npm` catalog tags. Update these policy files
when changing approved release rules; do not duplicate their version or ecosystem details in
rule documentation.

Action checks and pin fixes live in `src/repomod/rules/actions.py`; package checks live in
`src/repomod/rules/release.py`. Rules inspect repository files but do not install dependencies,
build projects, or execute release scripts.

## Fixtures and verification

Use offline fixtures under `tests/fixtures/` to cover real findings and deterministic repairs.
Tests should assert user-visible findings and repaired state without calling GitHub. Follow the
verification commands in [AGENTS.md](../AGENTS.md); run the focused test for the policy being
changed before widening verification when shared rule behavior is affected.
