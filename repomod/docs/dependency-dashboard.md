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

For a local server, `make deps` renders the dashboard with `--status` and serves it at
`http://127.0.0.1:8765` with `srv`; use `make deps PORT=9000` for another port.

Private repositories are left out of every page, count, and change list by default. Repositories
says how many are hidden. Use `make deps PRIVATE=1`, or `deps render --include-private`, to include
them. Repositories the catalog no longer manages, including those marked `IGNORE`, are left out of
every page as well. Their inventory files remain until the next unfiltered `deps snapshot` deletes
them. With `--repo` or `--tag`, pages, search, counts, changes, and status include only the selected
reportable repositories. Explicit `--repo` selection includes that repository even when it is private.

The site has no fetch, CDN, web font, or chart library, and it works from `file://`. Python
precomputes every page. The browser only filters, searches, routes, and expands.

The pages use the CSS-only mode of `@laputa-systems/ui-dashboard`. `src/repomod/dependencies/ui.py`
owns the `ui-*` markup. `assets/app.css` holds only what the library lacks, built from `--ui-*`
tokens.

## Pages

The sidebar groups pages by the work they support. A page shows something only when it tells
you what to change or decide. Everything else is a counter or is left out. Package pages are the
one lookup surface, reached through search.

- **To do** (`index.html`) starts with open work per repository, then groups each change once:
  **Fix now** for vulnerable packages with a clean upgrade, **Decide** for those without one,
  **Clear in one pass** for lockfiles, pinned Python tools, and all minor and patch upgrades, and
  **Plan** for breaking upgrades by repositories affected. Forks are left out. `triage.py` builds
  this list.
- **Repositories** groups repositories with work to do as Vulnerable, Breaking upgrades, or
  Maintenance, with bars on one scale. Clean repositories are a count. Forks appear only when
  they carry vulnerable versions. With `--status`, a Status group comes first: one card for each
  repository, forks included, with whatever `repomod status` flags (CI and workflows, releases,
  upstreams, pull request conflicts, and local checkout state), most failures first. A pulsing dot
  marks CI or a workflow that was running when the page was rendered.
- **Repository pages** (`repos/OWNER/REPO/index.html`) hold what to do in that repository,
  including coverage issues with a link to the file, and the repository's status with `--status`.
- **Security** has one card per owned repository listing its vulnerable packages. A latest
  release counts as a clean upgrade only when it is newer than the vulnerable version and has no
  advisories of its own. Forks with vulnerable versions sit in a collapsed panel.
- **Upgrades** (`outdated.html`) charts upgrades by ecosystem, then groups them as breaking,
  minor, and patch, with bars for repositories affected. A breaking upgrade changes the leftmost
  nonzero version component. Needs `repomod deps enrich`.
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
- **Bloat** charts direct dependencies whose removal would drop lockfile packages, split into
  those only it brings in and those shared. Lockfiles list every platform's packages, so counts
  are upper bounds.
- **Coverage** has one card per fixable issue kind with the action and the repositories. Its
  **Source risk** group lists every git source, unpinned action, floating image, missing lock,
  unpinned artifact, and vendored tree, with the repositories that use it. Known limits, such as
  formats with no parser, are counts. In forks, only checkout problems count.
- **Changes** counts everything since `HEAD~1` of `data/` and lists only downgrades, source
  changes, and new issues. It says `no baseline` when the previous revision has no inventory.
- **Packages** counts packages per ecosystem. Search (`/`) finds any package.
  `packages.html#pkg=pkg:cargo/serde` deep-links to its versions and consumers.

Charts use status colors only where color means severity, and one hue or emphasis elsewhere. The
library's categorical series colors fail the normal-vision separation check. Hovering a mark shows
its value, and every charted value also appears as text.

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

The browser scripts are TypeScript under `src/repomod/dependencies/dashboard_ts/` and compile to
`assets/app.js` and `assets/search.js` with `tsrs` (TypeScript 7.1). `make dashboard-ts` verifies a
frozen pnpm install, type-checks, lints with Oxlint, rebuilds, and fails if the committed
JavaScript is stale. The renderer copies the pinned library's base `styles.css` and `tokens.css`
and the `layout.css` and `data.css` family sheets from the installed package in `node_modules`, so
rendering needs `pnpm install --frozen-lockfile` once and fails with that instruction otherwise.
`make deps` only renders and serves; it does not run Node.

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

`make deps` is `uv run repomod deps render --status` followed by `srv`. The render is the part
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
