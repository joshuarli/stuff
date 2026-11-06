---
name: audit-dependencies
description: "Investigate the tracked dependency inventory with deps why, report, diff, enrich, and check, and answer provenance, divergence, source-risk, and freshness questions with evidence instead of guesses."
---

# Audit the dependency inventory

Use this skill when the question is what a repository depends on, why a package is present, which
repositories use it, how versions diverge, where a dependency comes from, or whether the
inventory is current. The inventory is the source of truth. Do not answer from a manifest, a
README, a registry page, or a package name that merely looks related.

This skill applies to the catalogued fleet and reads `data/dependencies/v1`. Do not run it while
changing repomod or the inventory. If the inventory is stale, report that; the owner decides
whether to snapshot.

## Always

1. Run `uv run repomod deps check --strict` first. Drift means the answer would be about stale
   data: stop and report the drifted paths. `--strict` also fails on failed or unavailable
   checkouts.
2. Start from the overview. Run `uv run repomod deps render` and open
   `file:///Users/josh/d/repomod/.work/dependencies/dashboard/index.html`, or read
   `uv run repomod deps report --json` directly. Lead with coverage: a `partial` or `failed`
   repository is not evidence of absence.
3. Follow the package with `uv run repomod deps why <package>`. Accept a name,
   `ecosystem:name`, or either with `@version`; quote scoped npm names. Report the shortest path
   and every distinct scope, resolution, and condition that changes the answer.
4. Find reverse consumers from `uv run repomod deps report --json` concentration rows and the
   package detail, not from `why`, which is forward-only. Distinguish repositories that use the
   package directly from those that only inherit it transitively.
5. Check divergence across the fleet and within one repository from the report and the dashboard
   dot plot. Name each version, its repositories, and whether the split follows a deliberate
   major-version boundary, a fork, or an accidental drift.
6. When a path is transitive-only, form a source hypothesis from the consuming instance, not the
   package name. An edge is first-party only when the source is a catalogued Git URL, a native
   Cargo registry component, a published package name backed by `crates-io` or `pypi` tags, or a
   path/workspace source inside the same repository. A similar name is never a link.
7. Use `uv run repomod deps diff --json` for changes. The default compares `HEAD` with the
   working tree; use `--from` and `--to` for release windows. `no baseline` means there is
   nothing to compare, not "no changes".
8. Report freshness and vulnerabilities only from enrichment. `not checked`, `stale`, and
   `unknown` are real states; missing enrichment is never clean. State the cache's `fetched_at`.
9. Answer with the question, the command or dashboard evidence, the affected repositories, the
   coverage caveats, and the residual uncertainty. Prefer a short table over prose.

## Ask first

- `uv run repomod deps enrich` uses the network. Ask before running it online, or use
  `--offline` for a cache-only answer. `--refresh` refetches every component.
- `uv run repomod deps snapshot` writes tracked data. Ask before regenerating, and never edit
  `data/dependencies/v1` by hand.
- A recommendation that would change a manifest, lockfile, workflow, tag, or policy is a
  repository change, not an inventory change. Propose the change and its evidence; do not apply
  it as part of the audit.

## Never

- Never infer a dependency from a similar package name, a repository description, or a registry
  page. Only the inventory's edges and first-party evidence count.
- Never treat missing or stale enrichment as a clean result.
- Never add or remove an edge, instance, or identity. Adapters and `snapshot` own extraction.
- Never run `snapshot`, `fix`, or `push` as part of an audit, and never reset, stash, clean,
  switch, or discard changes in a canonical checkout.
- Never open a network socket from `snapshot`, `check`, `render`, `why`, `report`, or `diff`.
  Enrichment is the only network path.
- Never copy inventory text into HTML without escaping. The dashboard already escapes names,
  paths, and inline JSON.

## Dashboard

Use the dashboard for orientation, then the JSON commands for exact answers. Search with `/`,
move with `j`/`k`, open with Enter, and copy a PURL with the copy button. The landscape matrix is
prevalence, the divergence dot plot is version spread, changes is `HEAD~1` against the working
tree, and coverage lists states and detection-only formats. Every repository page links back to
the overview, and `packages.html#pkg=<purl>` deep-links a package.

## References

- [Dependency inventory](../../../docs/dependency-inventory.md): commands, data layout, coverage
  kinds, update flow, budgets, and residual limits.
- [Dependency dashboard](../../../docs/dependency-dashboard.md): pages, routing, keyboard,
  refresh, and browser checks.
- [Repository tagging](../../../docs/tagging.md): first-party evidence from `crates-io`, `pypi`,
  Git URLs, path/workspace sources, and the rule that a similar name never links.
