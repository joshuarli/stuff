# Dependency inventory

repomod keeps a deterministic, read-only inventory of the dependency inputs in every
catalogued repository. The inventory is tracked in `data/dependencies/v1/`, so `git show`,
code review, `deps diff`, and the dashboard work
without a database, a background worker, or a network connection.

`snapshot` and `check` read each checkout's `refs/remotes/origin/HEAD` tree through Git
plumbing. They never read the working tree and never fetch. An ignored or untracked lock file
therefore cannot change the inventory. `check` regenerates the same bytes in memory and never
writes.

## Commands

All commands accept `--repo` and `--tag` where noted. `--repo` and `--tag` select the same
catalog entries as `lint` and `status`.

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

### `deps render`

Write the static dashboard under `.work/dependencies/dashboard/` by default. It reads the
inventory and the enrichment cache only. See [the dashboard guide](dependency-dashboard.md).

```sh
uv run repomod deps render
uv run repomod deps render --output .work/dependencies/dashboard
```

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

The inventory never records absolute paths, timestamps, commands, credentials, `.npmrc` auth
lines, `_authToken`, `.netrc` contents, environment values, or `GITHUB_TOKEN`-style
expressions. Userinfo is stripped from every URL; download and registry URLs drop their query
strings and fragments. A secret-bearing URL is recorded redacted, never skipped.

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
   `REPOMOD_FLEET=1`.

Source reads must stay EOL-independent: the inventory records blob IDs and content, so Git
line-ending conversion cannot change a fingerprint.

## Budgets and residual limits

- Inventory size: at most 15 MB total and 1.5 MB per repository file. The current fleet is about
  12 MB with an 864 KB largest file.
- `deps snapshot` over the fleet: at most 15 seconds of CPU. Worker output is byte-identical.
- Dashboard output: at most 10 MB.
- Detection-only formats from D12 (`meson.build`, Conan recipes, `vcpkg.json`, `.pc`, `flake.nix`,
  Makefiles, Gradle, Maven, and `Gemfile`) remain `unsupported_format` forever; they are recorded,
  not guessed.
- Local workspace and path packages are `pkg:generic` first-party instances, not registry
  packages. The audit rules still see their own notions; the inventory is a superset where the
  audit drops a second locked version.
- The inventory reads the committed `origin/HEAD` tree. Ignored, untracked, or working-tree
  locks are not read. Nested locks are more precise than a root-only read. Replacements are
  recorded at their resolved target.
- Unpinned or lockless declarations become versionless instances with `declared_only` or
  `unknown` edges. A value the adapter cannot resolve statically becomes `dynamic_value`, never
  a guessed edge.
- The dashboard caps consumers at 20 per package and transitive expansion at 8 per direct edge
  for size. Use `why`, `report --json`, or `diff --json` for complete data.

See [the dependency audit](audit.md) for the policy rules that consume related lock data, and
[the dashboard guide](dependency-dashboard.md) for the generated site.
