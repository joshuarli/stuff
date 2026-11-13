# Dependency dashboard

`repomod deps render` writes a static dashboard that works offline once generated. Generation
reads the inventory and enrichment cache, and consults GitHub visibility, cached for an hour.
`--status` also fetches live repository status. The default output
is `.work/dependencies/dashboard/`, which is disposable and not tracked.

```sh
uv run repomod deps render
open .work/dependencies/dashboard/index.html
```

The `open` command is for macOS; on Linux, open that file in your browser.

For a local server, `make deps` syncs the checkouts, re-snapshots the tracked inventory
(committing any changes under `fleet/dependencies/v1/`), renders the dashboard with live status,
and serves it at `http://127.0.0.1:8765` with `srv`; use `make deps PORT=9000` for another
port.

Private repositories are left out of every page, count, and change list by default. Repositories
says how many are hidden. Use `make deps PRIVATE=1`, or `deps render --include-private`, to include
them. Repositories the catalog no longer manages, including those marked `IGNORE`, are left out of
every page as well. Their inventory files remain until the next unfiltered `deps snapshot` deletes
them. With `--repo` or `--tag`, pages, search, counts, changes, and status include only the selected
reportable repositories. Explicit `--repo` selection includes that repository even when it is private.

The site has no fetch, CDN, web font, or chart library, and it works from `file://`. Python
precomputes every page. The browser only filters, searches, routes, and expands.

The pages use the CSS-only mode of `@laputa-systems/ui-dashboard`. `repomod/dependencies/ui.py`
owns the `ui-*` markup. `assets/app.css` holds only what the library lacks, built from `--ui-*`
tokens.

## Pages

The sidebar groups pages by the work they support. A page shows something only when it tells
you what to change or decide. Everything else is a counter or is left out. Package pages are the
one lookup surface, reached through search. World is the one orientation page and sits alone under
Overview.

- **To do** (`index.html`) starts with open work per repository, then groups each change once:
  **Fix now** for vulnerable packages with a clean upgrade, **Decide** for those without one,
  and **Clear in one pass** for lockfiles, pinned Python tools, fleet migrations, and all upgrades. Forks are left
  out. `triage.py` builds this list. Migrations are direct dependencies the fleet replaces
  rather than bumps: `rustix` (the laputa-systems fork, through `laputa-rustix`) and
  `oxlint-tsgolint` (Oxlint loads tsrs-lint from the `tsrs` package). They appear on To do and
  under **Migrate, don't bump** on Upgrades, never as an upgrade.
- **Repositories** groups repositories with work to do as Vulnerable or Maintenance, with bars
  on one scale. Clean repositories are a count. Forks appear only when they carry vulnerable
  versions. With `--status`, a Status group comes first: one card for each
  repository, forks included, with whatever `repomod status` flags (CI and workflows, releases,
  upstreams, pull request conflicts, and local checkout state), most failures first. A pulsing dot
  marks CI or a workflow that was running when the page was rendered.
- **World** (`world.html`) shows every package that owned repositories reach, split into yours,
  third-party, and owner unknown. Forks are left out. Yours means a package built inside one of
  your repositories, a package `graph.is_first_party` recognizes, or one whose source repository
  belongs to a catalog owner (`joshuarli`, `laputa-systems`). Owners come from Git and download
  URLs, Go module paths, image names, toolchain names, and the repository and homepage URLs that
  `deps enrich` caches for Cargo, npm, and PyPI packages. A package with none of these stays
  owner unknown rather than being guessed. Bars compare ownership by package and by package
  use. **Mirrored on stuff** compares the Cargo, npm, and PyPI packages in use with the stuff
  catalogs behind `fleet/registries.toml`: on stuff when every version in use is served, versions
  missing when some are not, and not on stuff otherwise. It lists the gaps by repositories
  reached. Other ecosystems and packages built inside your repositories are not counted. A
  treemap groups packages by ecosystem and owner, with area proportional to repositories reached.
  It colors cells by ownership, with lighter cells for transitive-only packages and a red outline
  for vulnerable ones, or, with the toggle, by mirror state. Each cell opens the package. The
  **How much is yours** ecosystem bars are links: each opens a collapsed list of that
  ecosystem's packages, showing the versions in use with their per-version repository counts,
  the owner, the origin, and a per-package deep link to its package section. Reach
  buckets and an owner list, with direct and transitive-only counts, repeat the charted values
  as text.
- **Repository pages** (`repos/OWNER/REPO/index.html`) hold what to do in that repository,
  including coverage issues with a link to the file, and the repository's status with `--status`.
- **Security** has one card per owned repository listing its vulnerable packages. A latest
  release counts as a clean upgrade only when it is newer than the vulnerable version and has no
  advisories of its own. Forks with vulnerable versions sit in a collapsed panel.
- **Upgrades** (`outdated.html`) charts upgrades by ecosystem, then lists them with bars for
  repositories affected. Upgrades are not classified by version component, because packages do not
  reliably follow semver. Needs `repomod deps enrich`. A direct dependency the fleet replaces
  appears under **Migrate, don't bump** instead of as a release bump, and is left out of the
  upgrade counts.
- **Alignment** groups version drift by ecosystem, leaving out repositories that an upgrade to
  the latest release already moves. Those appear on Upgrades. Each card draws the versions from
  oldest to newest, with the target in green, and names the repositories to move.
  The target is the approved pin, else the newest of the upstream release and versions in use,
  else the most common ref for tags and commits. Stale release metadata never recommends a downgrade.
  Cards state when the latest release is unconfirmed. Docker images prefer the newest versioned
  tag over the most common tag and are compared by tag and digest combined
  (a bare name counts as `latest`). Alpine (apk) packages follow their base image and are left
  out.
- **Choices** shows roles where a repository must change, with one adoption bar per option and
  the repositories to move. Compliant repositories, accepted exceptions, and settled roles are
  not listed.
- **Weight** (`weight.html`) shows which owned repositories carry the most third-party code, so you
  can choose where to slim and which dependency to drop. **By repository** draws one bar per
  repository, on one scale, largest first, with the value in text at the bar's end. Each bar stacks
  one segment per language: npm blue, Cargo vermillion, PyPI green, Go orange, and Other gray. A repository with
  no packages for the current selection has no bar: one muted line under the chart names them, and
  the table view still lists every repository. Each repository opens its direct dependencies by what
  removing one frees, then its heaviest packages: name, version, size, registry, and whether it is
  direct or transitive. **By dependency** ranks direct dependencies across repositories by the same
  measure, the top 30 shown. Removing a direct dependency frees it and the packages only it reaches.
  Bars are colored by the dependency's language; the part another counted
  direct dependency also reaches is drawn as a lighter segment of the same hue, and a package that is
  itself declared directly is never freed by removing another. Your own direct packages hold what
  they reach. Each repository keeps its 12 largest dependencies per language and filter
  combination. **Duplicate versions** lists repositories that build two versions of one package. A package counts once per repository however many lockfiles, targets, or projects
  list its version, and packages the World page classifies as yours are left out. Sizes come from
  `deps enrich`, and the page says so when none are cached.
  The sizes are not one unit, and the page states each measure: npm unpacked (installed) size,
  the compressed Cargo `.crate` archive, and a PyPI wheel download (macOS arm64, else Linux, else
  pure Python) or the sdist when there is no wheel. Go and other ecosystems report no size and
  count as size unknown. One filter row above the chart scopes everything below it:
  - **Languages**: TypeScript/JavaScript (npm), Rust (Cargo), Python (PyPI), Go, and Other, all on
    by default. Go and Other only add to the size-unknown counts.
  - **npm platform**: macOS arm64 (default), Linux x64 (musl), Linux arm64 (musl), or All platforms
    (upper bound). Lockfiles list every platform's optional binaries, such as esbuild's, which npm
    never installs on a given target. A package is left out for a platform when every edge reaching
    it is optional and its `os`, `cpu`, or `libc` lists, cached by `deps enrich`, exclude the
    platform under npm's rules: each list names the allowed values, and `!x` forbids `x`; `libc` is
    checked only on Linux. A package with any non-optional edge always counts, and platforms do not
    affect other ecosystems. All platforms counts everything, so it is an upper bound.
  - **Include dev dependencies**, off by default. A package is dev-only when no direct dependency
    with a non-dev scope reaches it. The scopes treated as dev are `dev`, `test`, `build`, and `ci`.
    Lockfiles record the transitive edges of a dev tool as `runtime`, so the page follows
    transitive edges from each direct dependency instead of reading the scope of the edge into a
    package. A package reached from both kinds of dependency counts as runtime. The inventory uses
    `dev` for npm and PyPI dev groups, and `dev` and `build` for Cargo.

  Python precomputes totals for every platform and scope, and the browser only switches between
  them, recomputing totals, bar lengths, order, the collapsed line, and package lists. The choice
  is written to the URL hash: `weight.html#lang=npm` opens TypeScript/JavaScript only,
  `#lang=npm&platform=linux-x64-musl&dev=1` adds a platform and dev dependencies, and
  `#repo=joshuarli/pi-build` opens that repository's row. Without JavaScript the page shows the
  default: every language, macOS arm64, no dev dependencies. A table view repeats the numbers.
  Each repository page links its per-language totals, for that default, to this page.
- **Coverage** has one card per fixable issue kind with the action and the repositories. Its
  **Source risk** group lists every git source, unpinned action, floating image, missing lock,
  unpinned artifact, and vendored tree, with the repositories that use it. Known limits, such as
  formats with no parser, are counts. In forks, only checkout problems count.
- **Changes** counts everything since `HEAD~1` of `data/` and lists only downgrades, source
  changes, and new issues. It says `no baseline` when the previous revision has no inventory.
- **Packages** counts packages per ecosystem. Fleet-local packages are `local`; toolchains,
  downloads, system packages, service bindings, and git dependencies each have their own
  ecosystem. Search (`/`) finds any package.
  `packages.html#pkg=pkg:cargo/serde` deep-links to its versions and consumers.

Charts use status colors only where color means severity, and one hue or emphasis elsewhere; weight.html is the
exception, coloring bar segments by language with colorblind-safe Okabe-Ito hues. The library's categorical
series colors fail the normal-vision separation check. Hovering a mark shows
its value, and every charted value also appears as text.

## Curated roles

`fleet/dependency-roles.toml` is curated user data. Each table is a role with an `ecosystem`, an
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

Repositories tagged `fork` keep upstream's dependency choices. Outdated, Alignment,
Choices, and To do leave them out; Security and Repositories list them separately with
their vulnerable versions. A package may belong to one option per role. Only direct
declarations count. `repomod deps report` lists the repositories that mix options.

## Interaction

- `/` focuses search. Search matches package names, ecosystems, and PURLs.
- Filter boxes and "Show N more" narrow long lists. Every item stays visible when JavaScript
  does not run.
- On Repositories, `j` and `k` move through the cards, and `Enter` opens the selected card.
- Copy buttons copy a command or a PURL.

## Coverage and zero states

A repository with no dependencies is `complete: zero dependencies`, not blank. An unavailable
or failed checkout is marked separately. Enrichment shows `not checked` or `stale` rather than a
clean zero. Tiles show `—` when registry data is missing. Only a fresh cache entry can say there
are no known vulnerabilities.

## Refreshing

Regenerate after a snapshot, enrichment, or adapter change:

```sh
uv run repomod deps snapshot
uv run repomod deps check --strict
uv run repomod deps enrich
uv run repomod deps render
```

Enrichment checks public upstream registries for current releases, consulting configured registries
when a package is unavailable upstream. Mirrors can need refreshing before a recommended version
is installable; `audit-upgrades` continues to check the repository's configured registry.

The renderer refuses to replace a directory that lacks its `.repomod-dashboard` marker, so a
mistyped `--output` cannot delete unrelated files.

## Source and verification

The browser scripts are TypeScript under `repomod/dependencies/dashboard_ts/` and compile to
`assets/app.js`, `assets/search.js`, and `assets/weight.js` (loaded only by the Weight page) with
`tsrs` (TypeScript 7.1). `make dashboard-ts` verifies a frozen pnpm install, type-checks, lints
with Oxlint, rebuilds, and fails if the committed JavaScript is stale. The renderer copies the pinned library's base `styles.css` and `tokens.css`
and the `layout.css` and `data.css` family sheets from the installed package in `node_modules`, so
rendering needs `pnpm install --frozen-lockfile` once and fails with that instruction otherwise.
`make deps` syncs the checkouts and re-snapshots the inventory (committing any changes) before
it renders; it does not run Node.

## Budgets and verification

The output budget is 10 MB. The initial overview-render target is 1.5 seconds.
Measure the generated fleet and the browser load when validating these targets; old fleet
sizes and timings are not current guarantees.

The browser checks use agent-browser at 1440×900 and 390×844. They verify no console errors, no
horizontal page overflow, search for `serde`, the `#pkg=pkg:cargo/serde` route, and a back link
on every repository page. Screenshots are written under `.work/dependencies/shots/`. Tables
wider than a narrow viewport scroll inside their own container; the page itself must not
overflow.

## Measuring the render

`make deps` is `uv run repomod sync` and `uv run repomod deps snapshot` (committing any
inventory changes), then `uv run repomod deps render --status` followed by `srv`. The render is the part
that varies: it reads the tracked inventory, collects live repository status, and writes the
dashboard. `bench/deps_perf.py` separates warm-cache behavior from an isolated cold run.

```sh
make bench              # phase breakdown, cold and warm, 3 runs each
make bench-cli          # the real `uv run repomod deps render --status` command
make bench RUNS=1
```

`warm` uses the real cache directory, as `make deps` does. `cold` points `XDG_CACHE_HOME` at a
scratch directory under `.work/tmp/` and clears it before each run, so it measures the first run
after the caches expire. The `work_*` columns sum operations that run concurrently, so they can
exceed the wall-clock `status` time; `load`, `visibility`, `status`, and `render` partition the
wall-clock `total`.
