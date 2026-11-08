# Dependency dashboard

`repomod deps render` writes a static, offline dashboard. It reads the tracked inventory and
the local enrichment cache only; it never opens a socket. The default output is
`.work/dependencies/dashboard/`, which is disposable and not tracked.

```sh
uv run repomod deps render
open file:///Users/josh/d/repomod/.work/dependencies/dashboard/index.html
```

For a local server, `make deps` renders the dashboard and serves it at
`http://127.0.0.1:8765` with `srv`; use `make deps PORT=9000` for another port.

The site has no fetch, CDN, web font, framework, or chart library. Everything is precomputed by
Python; the browser only searches, routes, and expands. It works from `file://`.

## Pages

- **Overview** — counts and the top ten of each list below: vulnerable versions, direct
  dependencies furthest behind, the heaviest direct dependencies, version splits, and
  roles with competing choices; then every repository ranked worst first by vulnerable
  versions, breaking upgrades, versions behind, split versions, and role conflicts.
- **Outdated** — vulnerable versions, then direct dependencies behind their registry's
  latest release, grouped into breaking, minor, and patch upgrades. A breaking upgrade
  changes the leftmost nonzero version component. Needs `repomod deps enrich`.
- **Bloat** — each direct dependency with the lockfile packages it brings in and how many of
  those no other direct dependency in the repository needs, then repositories whose lockfile
  holds two versions of one package. Lockfiles list every platform's packages, so counts are
  upper bounds.
- **Alignment** — packages declared directly by two or more repositories at different
  versions, with a target and the repositories off it. The target is the latest release,
  else the newest version in use, else the most common ref for tags and commits. Alpine packages follow their base image and are left out.
- **Choices** — one card per contested role with an adoption bar (preferred in green,
  other options in amber, banned in red) and repository lists behind expanders; settled
  roles become pills. The how-it-works explanation sits behind a disclosure.
- **Packages** — ecosystem sections with mix charts (edge mix, scopes, sources), a
  "most used" horizontal bar chart per ecosystem, and a table with a usage bar per row.
  A package view adds a consumer map (fan-in SVG, direct vs transitive) for packages in
  four or more repositories, plus versions, sources, enrichment, and entry paths.
  `packages.html#pkg=pkg:cargo/serde` deep-links to a package.
- **Changes** — the working inventory against `HEAD~1` of `data/`, or `no baseline` when the
  previous revision has no inventory.
- **Coverage** — repository states, issue kinds, ecosystem composition, source risk, and
  detection-only formats.
- **Repository pages** — one page per repository under `repos/OWNER/REPO/index.html`,
  starting with what needs attention there (vulnerable versions, upgrades, versions off
  target, role conflicts), then projects, scope counts, direct dependencies, expandable transitive lists, sources,
  duplicates, gap evidence, and links back to the overview.

## Curated roles

`dependency-roles.toml` is curated user data. Each table is a role with an `ecosystem`, an
optional `preferred` option, optional `complements`, and `options` mapping each option to the
package names that mean a repository uses it. A complement is an option that may be used
alongside the preferred one, such as `libc` next to `rustix` for calls rustix does not cover;
it is flagged only in repositories that lack the preferred option. `exceptions` maps a
repository to the reason it keeps a non-preferred option; Choices lists it with that reason
instead of flagging it. The preferred option may be an empty list, which stands for the
standard library (`stdlib = []`): it is never in use itself, so every repository that declares
any other option of the role is off preferred, and repositories that declare none are not
counted. `ban` maps an option to `exact` or `prefix`; the audit fails
repositories that depend on a banned option's crates (see [audit](audit.md)).

Repositories tagged `fork` keep upstream's dependency choices. Outdated, Bloat, Alignment,
Choices, and the repository ranking leave them out; the overview lists them separately with
their vulnerable versions. A package may belong to one option per role. Only direct
declarations count. `repomod deps report` lists the repositories that mix options.

## Interaction

- `/` focuses search. Search matches package names, ecosystems, and PURLs.
- `j` and `k` move through table rows; `Enter` opens the selected row.
- Copy buttons copy a PURL or a path.
- Text uses the system serif; identifiers use the system monospace. There is no animation.

## Coverage and zero states

A repository with no dependencies is `complete: zero dependencies`, not blank. An unavailable
or failed checkout is marked separately. Enrichment shows `not checked` or `stale` rather than a
clean zero; only a fresh cache entry can say there are no known vulnerabilities.

## Refreshing

Regenerate after a snapshot, enrichment, or adapter change:

```sh
uv run repomod deps snapshot
uv run repomod deps check --strict
uv run repomod deps enrich
uv run repomod deps render
```

The renderer refuses to replace a directory that lacks its `.repomod-dashboard` marker, so a
mistyped `--output` cannot delete unrelated files.

## Source and verification

The browser scripts are TypeScript under `src/repomod/dependencies/dashboard_ts/` and compile to
`assets/app.js` and `assets/search.js` with `tsrs` (TypeScript 7.1). `make dashboard-ts` verifies a
frozen pnpm install, type-checks, lints with Oxlint, rebuilds, and fails if the committed
JavaScript is stale. `make deps` only renders and serves; it does not run Node.

## Budgets and verification

The site must stay under 10 MB. The current fleet is 60 pages and about 9.5 MB. The overview
renders in tens of milliseconds from `file://`.

The browser checks use agent-browser at 1440×900 and 390×844. They verify no console errors, no
horizontal page overflow, search for `serde`, the `#pkg=pkg:cargo/serde` route, and a back link
on every repository page. Screenshots are written under `.work/dependencies/shots/`. Tables
wider than a narrow viewport scroll inside their own container; the page itself must not
overflow.
