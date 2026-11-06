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
Python; the browser only filters, sorts, and expands. It works from `file://`.

## Pages

- **Overview** — coverage by state, issue kinds, ecosystem composition, prevalence, divergence
  hotspots, source risk, curated-role overlap, the repository table, and recent changes.
- **Landscape** — a dense repository × package matrix. Sort rows by commonality, ecosystem,
  repository, or coverage. Search a package and use **+ column** to add it; the matrix shows at
  most 60 columns by default.
- **Divergence** — one row per package with more than one version, with a dot plot and the
  repositories per version, plus packages that diverge inside one repository.
- **Packages** — search or pick a package. Each view shows identity, versions and sources,
  repositories, enrichment state, and entry paths with relation, scope, and resolution.
  `packages.html#pkg=pkg:cargo/serde` deep-links to a package.
- **Changes** — the working inventory against `HEAD~1` of `data/`, or `no baseline` when the
  previous revision has no inventory.
- **Coverage** — repository states, issue kinds, and detection-only formats.
- **Repository pages** — one page per repository under `repos/OWNER/REPO/index.html`, with
  projects, scope counts, direct dependencies, expandable transitive lists, sources,
  duplicates, gap evidence, and links back to the overview.

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

The site must stay under 10 MB. The current fleet is 58 pages and about 9.9 MB. The overview
renders in tens of milliseconds from `file://`.

The browser checks use agent-browser at 1440×900 and 390×844. They verify no console errors, no
horizontal page overflow, search for `serde`, the `#pkg=pkg:cargo/serde` route, and a back link
on every repository page. Screenshots are written under `.work/dependencies/shots/`. Tables
wider than a narrow viewport scroll inside their own container; the page itself must not
overflow.
