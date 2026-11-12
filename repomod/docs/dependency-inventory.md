# Dependency inventory

repomod keeps a deterministic, read-only inventory of the dependency inputs in every
catalogued repository. The inventory is tracked in `data/dependencies/v1/`, so `git show`,
code review, `deps diff`, and the dashboard work
without a database or a background worker. The stored data and generated pages work offline.
The `why`, `report`, and `render` CLI handlers consult cached GitHub visibility and may contact
GitHub. `render --status` also collects live status; `enrich` contacts registries and OSV.
`snapshot`, `check`, and `diff` do not fetch network data.

`snapshot` and `check` read each checkout's `refs/remotes/origin/HEAD` tree through Git
plumbing. They never read the working tree and never fetch. An ignored or untracked lock file
therefore cannot change the inventory. `check` regenerates the same bytes in memory and never
writes. Git tree and blob reads disable lazy fetches, so a missing object becomes a coverage
failure. Extraction never executes managed-repository code, package managers, build scripts,
Swift or Zig manifests, or commands such as `cargo metadata` and `go mod graph`. Static
uncertainty becomes a coverage issue instead of an execution fallback or a guessed edge.

## Commands

All commands accept `--repo` and `--tag` where noted. `--repo` and `--tag` select the same
catalog entries as `lint` and `status`. Exit 0 means success; exit 1 means drift, strict coverage
failure, or no `why` path. Operational errors exit 2. JSON modes write JSON to stdout and
reserve stderr for diagnostics.

### `deps snapshot`

Read the selected checkouts and write `data/dependencies/v1/`. A repository file is rewritten
only when its bytes change; an application-only commit changes only `index.json`. An unfiltered
snapshot deletes files for repositories that are no longer selected. A `--repo` or `--tag`
snapshot is incremental: it touches only the selected files and their `index.json` entries.

```sh
uv run repomod deps snapshot
uv run repomod deps snapshot --repo joshuarli/kache --workers 8
```

`--workers` defaults to 4. Output is byte-identical for any worker count. `--strict` exits 1
when a selected repository is `failed` or `unavailable`; the data is still written.

### `deps check`

Regenerate the selected inventory in memory and list every file that differs from
`data/dependencies/v1/`. It writes nothing and exits 1 on drift. `--strict` also exits 1 when a
selected repository is `failed` or `unavailable`.

```sh
uv run repomod deps check --strict
```

### `deps why`

Print every simple path from each selected project to matching package instances, shortest
first and then lexicographically, capped by `--max-paths` per project. Cycles are safe because a
path never repeats a node. The package can be a name, `ecosystem:name`, or either with
`@version`; scoped npm names keep their leading `@`. It reads the inventory only and exits 1 when
no path matches.

```sh
uv run repomod deps why tokio
uv run repomod deps why cargo:serde@1.0.229 --max-paths 5
uv run repomod deps why npm:@types/node --json
```

### `deps report`

Summarize composition, concentration, divergence, source risk, scope, first-party links, and
curated-role overlap from `dependency-roles.toml`. Every metric carries a unit and a denominator. Freshness and
vulnerability counts come only from the enrichment cache and are labeled with its `fetched_at`;
missing or stale enrichment is `not checked`, never clean.

```sh
uv run repomod deps report
uv run repomod deps report --json
```

### `deps diff`

Compare `data/dependencies/v1` at two Git revisions. `--from` defaults to `HEAD`; `--to`
defaults to the working tree. Versions order with PEP 440 for PyPI and numeric dotted keys for
other ecosystems; a comparison that neither orders is `changed`. It reports added and removed
repositories, instances, versions, sources, and issues. When the `--from` revision has no
inventory it prints `no baseline`.

```sh
uv run repomod deps diff
uv run repomod deps diff --from HEAD~1 --to HEAD --json
```

### `deps enrich`

Query latest versions and OSV for resolved Cargo, PyPI, npm, and Go components. Results are
written to `io.cache_file("deps-enrichment.json")`, outside the repository, as
`{fetched_at, results: {purl: {latest, latest_source, vulns, fetched_at}}}`. Entries older than
24 hours are stale. `--refresh` refetches everything; `--offline` reads only the cache and
never opens a socket. Components in other ecosystems render as `not checked`.

```sh
uv run repomod deps enrich
uv run repomod deps enrich --offline --json
```

Enrichment never adds edges and never changes package identities.
An OSV transport, response, or classification failure exits 2 and records an error in JSON output.
Failed refreshes preserve previous advisories and their check timestamps, marking them stale.
Packages without a successful check remain `not checked`. A later successful refresh clears the error.

### `deps render`

Write the static dashboard under `.work/dependencies/dashboard/` by default. Generation reads
the inventory and enrichment cache and consults GitHub visibility. `--status` adds live status.
See [the dashboard guide](dependency-dashboard.md).

```sh
uv run repomod deps render
uv run repomod deps render --output .work/dependencies/dashboard
```

### Source risk

`deps report` counts the inputs that can change without a new commit. Each source-risk metric
prints one line per location as `repository evidence-file package-url`; `--json` carries the
same rows under `source_risk.locations`. Pinned GitHub Actions are not also counted as Git
sources. Only the repository-root `.gitmodules` is read, so nested copies such as test data are
ignored. A download counts as pinned when its output file is verified with `sha256sum -c` or
`shasum -a 256 -c` and a 64-character SHA-256 anywhere in the same `run` block, including on a
later line. A regex fragment whose URL stops at a backslash is not a download.

Repositories tagged `fork` are reported apart, under `source_risk_forks` and a separate text
section, because their upstream workflows stay outside local policy. `source_risk` and the
dashboard count only the other repositories. An instance used by both kinds counts in both.

A catalog `rust-project:lockfile:DIR` override also hides that directory's missing-lock issue
from the report, so lint and the inventory share one exemption.

## Data layout

- `index.json` records the schema, `PARSER_VERSION`, and each repository's commit,
  inputs fingerprint, and state.
- `repos/OWNER/REPO.json` records tags, `{path, blob}` inputs, the inputs fingerprint, projects,
  components, instances, edges, and coverage issues.
- `inputs_fingerprint` is the SHA-256 of the sorted `(path, blob)` list plus `PARSER_VERSION`.
- Components are canonical package URLs. Instances are `<purl>#<context>`; the context is a
  pnpm peer suffix, a uv resolution-marker digest, or a Cargo source digest when one name and
  version exists in more than one source.
- A repository is `complete` with no issues, `partial` with only gaps, `failed` with an error
  issue, and `unavailable` when its checkout or default ref cannot be read. Zero dependencies is
  `complete` with no edges.
- Coverage issue kinds are closed: `checkout_missing`, `origin_mismatch`,
  `default_ref_missing`, `git_failure`, `parse_error`, `lock_mismatch`, `missing_lock`,
  `unsupported_format`, `dynamic_value`, `symlink_skipped`, `submodule`, `vendored_unknown`,
  and `non_utf8`.

The tracked inventory includes structured metadata from private managed repositories: names,
versions, source identities, relative evidence paths, and locators. Reporting visibility is a
separate contract described in [tagging](tagging.md). Repository code remains private: never
store file contents, excerpts, script bodies, or command lines, and never copy private source
files into fixtures. Use synthetic inputs or public examples.

The output contract excludes absolute paths, timestamps, credentials, `.npmrc` auth lines,
`_authToken`, `.netrc` contents, environment values, and `GITHUB_TOKEN`-style expressions.
Strip userinfo from every URL and queries and fragments from download and registry URLs.
This applies to constraints, origin fields, identity qualifiers, evidence, and diagnostics.
YAML parser diagnostics omit source excerpts because they can contain partial credentials.
Record useful sanitized source information instead of skipping the dependency.

### Identity and resolution

Configured mirrors of public registries map to public package identities while retaining the
observed registry URL. Native and distinct registries retain source qualifiers. Action subpaths
and Docker registry qualifiers are identity fields; collapsing them loses dependencies.
Extraction deduplicates these instances by their complete canonical ID. Before writing a repository,
every edge must connect an emitted project or instance to an emitted instance.
Evidence locators are structured key paths or line references, never source excerpts.
First-party links require positive evidence as defined in [tagging](tagging.md), never a
similar package name.

Edge resolution distinguishes `resolved`, `conditional`, `declared_only`, `unknown`, and
`unsupported`. A declared dependency whose target cannot be resolved stays visible as an
unresolved declaration, rather than disappearing or gaining a guessed version. The model in
`src/repomod/dependencies/model.py` owns the format; there is no separate JSON Schema authority.
Serialization sorts records deterministically. Snapshot replacement is atomic per file, not
one transaction across the whole inventory.

## Layering

`src/repomod/dependencies/` owns the inventory. `git_tree.py` performs Git reads through
`git.py`; `discover.py` classifies recognized inputs; adapters under
`dependencies/adapters/` are frozen classes with `name`, `matches(path)`, and
`extract(inputs)`, and perform no I/O. `snapshot.py` orchestrates extraction and serialization;
`graph.py`, `analysis.py`, `diff.py`, `enrich.py`, and `render.py` are read-only consumers,
except that `enrich` writes its local cache and `render` writes its output directory.

`cmd/deps.py` imports `repomod.dependencies` inside the handler, so `repomod --help` and the
other commands do not load `ruamel.yaml` or `packaging`. Policy rules stay read-only and fixers
keep explicit, validated file plans. The inventory does not feed lint or fix decisions
automatically; it informs people and agents. Do not add a database, a background worker, or a
generic workflow framework to support it.

## Updating the inventory

1. Change the adapter or model. Bump `PARSER_VERSION` in `snapshot.py` when extraction output
   changes, so fingerprints and incremental snapshots invalidate.
2. Run `uv run repomod deps snapshot`.
3. Run `uv run repomod deps check --strict` and `uv run repomod deps render`.
4. Commit code and data together. A data-only commit is `chore(deps): ...`; an adapter change
   that replaces data is one coherent commit with both.
5. Run the full test suite and, for adapter or model changes, the opt-in fleet test with
   `REPOMOD_FLEET=1`. That opt-in check compares audit coverage, parser errors, repeated runs,
   worker counts, and checkout HEAD/status preservation. It needs the actual canonical fleet.

Source reads must stay EOL-independent: the inventory records blob IDs and content, so Git
line-ending conversion cannot change a fingerprint.

## Budgets and residual limits

These are acceptance targets, not current measurements. Measure representative fleet data
before claiming a target is met. Visibility and live-status network time are separate from
the original render target.

| Workflow or artifact | Target |
| --- | --- |
| Tracked inventory | 15 MB total; 1.5 MB per repository |
| Fleet snapshot CPU; fleet check | 15 seconds each |
| `deps why serde`; `deps report --json` | 2 seconds each |
| Inventory-to-dashboard render, without live status | 10 seconds |
| Dashboard bytes; initial overview render | 10 MB; 1.5 seconds |
| Full test suite; one dependency test file | 30 seconds; 10 seconds |
| CLI help imports | Neither `ruamel` nor `packaging` |

- Detection-only formats (`meson.build`, Conan recipes, `vcpkg.json`, `.pc`, `flake.nix`,
  Makefiles, Gradle, Maven, and `Gemfile`) produce `unsupported_format`. Add a parser when a
  concrete managed consumer justifies it; never execute the file to discover dependencies.
- JavaScript lock parsing supports pnpm v9 and npm package-lock v3. Yarn and Bun locks are
  detected as unsupported. Swift manifests use a static scan and Zig uses a literal ZON subset.
- Go dependency edges come from `go.mod`. `go.sum` and `go.work` are fingerprinted but add no
  edges, and `go.mod` exclusions are not modeled. Free-form vendored metadata such as
  `README.chromium` is not parsed.
- Local workspace and path packages are `pkg:generic` first-party instances, not registry
  packages. The audit rules still see their own notions; the inventory is a superset where the
  audit drops a second locked version.
- The inventory reads the committed `origin/HEAD` tree. Ignored, untracked, or working-tree
  locks are not read. Nested locks are more precise than a root-only read. Replacements are
  recorded at their resolved target.
- Unpinned or lockless declarations become versionless instances with `declared_only` or
  `unknown` edges. A value the adapter cannot resolve statically becomes `dynamic_value`, never
  a guessed edge.
- A Cargo manifest whose package is locked as a path package by another lock in the same
  repository, such as a vendored crate excluded from the workspace, resolves through that lock
  instead of reporting `missing_lock`. A cargo-fuzz harness with `[package.metadata] cargo-fuzz =
  true` is development-only and does not report `missing_lock` either. Other lockless manifests
  still do.
- The dashboard caps consumers at 20 per package and transitive expansion at 8 per direct edge
  for size. Use `why`, `report --json`, or `diff --json` for complete data.

The inventory supplements the audit rather than replacing its collectors or upgrade decisions.
Curated roles provide deterministic comparisons. A hosted service, history ledger, speculative
functional-overlap inference, extra network enrichment, and CycloneDX export need a concrete
consumer before they become part of this subsystem.

See [the dependency audit](audit.md) for the policy rules that consume related lock data, and
[the dashboard guide](dependency-dashboard.md) for the generated site.
