# Dependency inventory campaign

## Status

**Goal:** a read-only, deterministic, Git-tracked dependency inventory of every managed repository in `catalog.toml`, with `repomod deps` commands for agents (`snapshot`, `check`, `why`, `report`, `diff`, `enrich`, `render`), a static offline dashboard, and a repo-local `audit-dependencies` skill.

**Preflight:** ready on 2026-11-06 at `102b7b4`. Owner decision D1 was given on 2026-11-06.

- **Accepted slices:** S1 — dependency model, tree reader, discovery, snapshot, and check (`db5ffba`); S2 — Cargo adapter and full lock graph (`5b7b4f9`); S3 — uv adapter with markers, extras, groups, and workspace sources (`67eee8e`); S4 — pnpm v9 and npm v3 adapters with peer contexts and nested resolution (`fca699d`); S5 — Go, Swift, and Zig adapters (`8d3bebc`); S6 — Actions, containers, toolchains, submodules, CMake, Wrangler, and vendored inputs (`3887e96`); S7 — graph, `why`, `report`, first-party links, and the opt-in fleet parity test (`38cd08b`); S8 — the tracked fleet inventory (`612571d`); S9 — inventory `diff` and cached enrichment (`82db063`), with a live fetch of 2877 components, 6594 instance records checked, 57 vulnerable records, and a 544 KB cache at 2026-11-06T17:50:32Z; S10 — the static dashboard (`619ea62`), 58 pages and 9.9 MB, overview 26 ms, all pages console-clean and no horizontal overflow at 1440 or 390; S11 — the operator reference, dashboard guide, README, and design/tagging/policy boundary (`30295c7`); S12 — the repo-local `audit-dependencies` skill and its test (`8e86719`), exercised on serde (10 direct and 13 transitive-only repositories, three registry versions, 6594 checked instances, clean diff). This session's skill manifest predates the new file, so the harness invocation needs a new session; the test asserts every referenced command against `deps --help`. The `dataviz` skill is not installed on this host, so S10 applied the D21 Tufte direction directly. The inventory holds 52 repositories (0 complete, 52 partial, 0 failed, 0 unavailable), 601 projects, 2911 package names, 3444 name@version pairs, 7867 instances, and 16751 edges (3910 direct, 12841 transitive). It is 12 MB with an 864 KB largest file; `check --strict` exits 0. The `.gitignore` `repos/` rule is anchored to the repository root so the inventory's `repos/` tree is tracked. The fleet test proves D7 parity with the documented improvements (committed-tree reads, local packages as generic, ignored working-tree locks, nested locks, and replacements), byte-identical output across runs and worker counts, and unchanged checkout state.
- **Red:** none. `make test` is 809 passed and 3 skipped; ruff and ty are clean. `REPOMOD_FLEET=1 uv run pytest tests/test_deps_fleet.py` passes.
- **Parked:** none.
- **Next:** S13.
- **Resume:** "Execute `campaign-deps.md` from **Status**. Run `uv run repomod sync`, then start the next slice."

## Charter

- Decide and keep going. Record a decision in **Decisions** only when a later slice depends on it. Use the smallest reversible assumption for anything unlisted (D24).
- The grant is local: edit this repository, add the two runtime dependencies in D6, run its tests, linters, and `repomod deps` commands, run `uv run repomod sync` as `AGENTS.md` requires, read canonical checkouts through Git object commands, use the network only inside `deps enrich`, and open the generated dashboard with `agent-browser`. Commit accepted slices on `master`.
- Hard stops. Each one parks the item, not the campaign:
  - any remote write: pushing, pull requests, `repomod fix`, `repomod push`, `repomod merge`, `topics apply`;
  - any write to a canonical checkout beyond what `sync` itself does, including checkout, switch, stash, reset, clean, install, lock, or build;
  - executing anything from a managed repository: package managers, build scripts, `Package.swift`, `build.zig`, Makefiles, shell, `cargo metadata`, `go mod graph`;
  - a runtime dependency other than the two in D6;
  - copying file content from a `private-tool` repository into tests, fixtures, docs, or commits;
  - a check that cannot fail.
- Fix an egregious problem met on the way, in its own commit, only when the current slice causes or exposes it: a test it broke, a stale reference to a command or doc it changes, or a budget in **Performance** exceeded by more than 3×. Record anything else under **Status** and leave it. No formatter runs, no general cleanup.
- Parity before optimization. S1–S10 make every deliverable work against the real fleet. Size and speed work beyond the 3× floor waits for S12.
- One writer by default. S3–S6 may run as at most two parallel lanes (see **Sequence**). Lanes use worktrees at `.work/wt/<lane>`, following `orchestrate`'s worktree lifecycle. Never use `/tmp` or `$TMPDIR`; set `TMPDIR=$PWD/.work/tmp` for every command.
- Commits are Conventional Commits with scope `deps` (for example `feat(deps): parse cargo lock edges`). Put the item ID in the body. The new runtime dependencies are a contract change, so S1's commit takes `feat(deps)!` with a `BREAKING CHANGE:` footer naming them. Code comments never cite this document.
- Before you fix a failing check, find its cause (`root-cause`). Never retry an unchanged failure. Time-box each item at three attempts, then park it on `campaign/deps-<id>` and record the cause and a retry condition in **Status**.
- Each slice ends with its acceptance command and the narrowest affected test file, run on `master` after integration. Run the full suite (`make test`, `uv run ruff check src tests`, `uv run ty check`) at S7, S10, and S13. Red work never lands on `master`.
- After each accepted slice, run the parent steering checkpoint in `~/.claude/skills/orchestrate/references/campaigns.md`. Rewrite **Status** only when its content changes; Git history is the log.

## Decisions

### Owner-decided

- **D1 — Names are public; source code is private (2026-11-06).** The full inventory for all 52 managed repositories, including the `private-tool` repositories `joshuarli/shadowdriver` and `joshuarli/toolbox`, is committed to repomod. That covers repository names, package names, versions, sources, file paths, and locators. No private overlay, no visibility policy, no `privacy.py`. This supersedes the draft's privacy section (public-safe inventory, private overlay, separate private Git repository, fail-closed visibility). What stays private is code: the inventory and the dashboard never store file contents, excerpts, script bodies, or command lines from any repository. They store only the structured facts in D10. Tests and fixtures never copy files from `private-tool` repositories; use public repositories or synthetic inputs. Credential-bearing URLs are still redacted (D17); that is a security rule, not a privacy one.
- **Intent carried from the owner's draft:** the inventory lives in Git under `data/dependencies/v1/`, generated HTML is disposable and never committed, extraction is strictly read-only against managed repositories, and the dashboard follows a Tufte-style design (D21). The agent interfaces `why`, `report --json`, and `diff` come before the dashboard. The three analyses with the most visual weight are prevalence (direct versus transitive), version fragmentation, and provenance (why a package is present).

### Prescribed

Each decision gives its reason and what would reverse it.

- **D2 — Selection is `catalog.repositories`.** Use `load_catalog` and `select_repositories` from `src/repomod/cmd/common.py` with the existing `--repo` and `--tag` semantics. Ignored and pending entries are never inventoried. Verified at `102b7b4`: 52 managed, 70 ignored, 0 pending. `nanoshop`, `niceforge`, `usr`, `xsh`, and `repomod` are ignored. Do not hard-code these counts; S1's tests assert the selection logic. Forks are included and carry their `fork` tag, so every aggregate can be filtered with `--tag` and the dashboard has a fork toggle. Reverses only if the catalog grows a dedicated inventory tag.
- **D3 — Inputs are the committed tree at `refs/remotes/origin/HEAD`.** Resolve it with `git rev-parse` in the checkout returned by `repository_path` and validate the origin with `expected_origin` from `src/repomod/git.py`. Read the tree with one `git ls-tree -r -z --full-tree <commit>` and blobs with one `git cat-file --batch` per repository. Never read the working tree, the checked-out branch, or the index. Verified at `102b7b4`: all 52 checkouts exist and all have `origin/HEAD`. `laputa-systems/zzz` is checked out on `all-green/zzz`, and the inventory still reads `origin/master`. Reading the fleet's 871 dependency-input blobs (3.6 MB) took 1.19 s. Git subprocesses live in `src/repomod/git.py` (new functions there) or in `dependencies/git_tree.py`, which calls `run_git`. Reverses if sync stops maintaining `origin/HEAD`.
- **D4 — Missing inputs are coverage, not crashes.** A missing checkout, a mismatched origin, an absent `origin/HEAD`, a Git failure for one repository, a symlinked input (mode `120000`, never followed), a gitlink (mode `160000`), or a non-UTF-8 input becomes a coverage issue on that repository. The run continues. Exit 2 is reserved for operational failures: catalog errors, an unwritable inventory, or a missing `git` binary.
- **D5 — Package layout.** Use the draft's decomposition, merged where modules would be thin. `src/repomod/dependencies/` holds `model.py`, `git_tree.py`, `discover.py`, `snapshot.py`, `graph.py` (normalization, traversal, `why`), `analysis.py`, `diff.py`, `enrich.py`, `render.py`, and `adapters/` with one module per ecosystem family plus `adapters/__init__.py`, which holds the explicit registration tuple. The CLI goes in `src/repomod/cmd/deps.py`, registered in `src/repomod/cli.py`. `cmd/deps.py` imports `repomod.dependencies` inside its handler, so other commands never import `ruamel.yaml`. No plugin framework, no entry points. An adapter is a frozen class with `name`, `matches(path) -> bool`, and `extract(inputs) -> AdapterResult`; it takes typed blob contents and returns typed records. It performs no I/O.
- **D6 — Runtime dependencies: `ruamel.yaml>=0.19.1` and `packaging>=26`.** These are the project's first runtime dependencies; S1 surfaces them as a contract change. Why YAML: the fleet's `pnpm-lock.yaml` files and all 130 workflows need a real YAML parser, and the standard library has none. PyYAML 6.0.3 fails to build on this host's Python 3.15.1 (setuptools hits a `hashlib` signature error). ruamel.yaml 0.19.1 is pure Python, MIT-licensed, typed (`py.typed`), with no required dependencies; it installs at 1.4 MB and imports in 10 ms. On this host it parsed every fleet pnpm lock in at most 80 ms and all 130 workflows without error. Load with `YAML(typ="base")`, so every scalar is a string (no YAML 1.1 `on: → True`, no float versions), and use `load_all`. Why `packaging`: PEP 508 requirement and marker parsing, and PEP 440 ordering for `diff`. It is Apache-2.0 OR BSD-2-Clause, typed, pure Python, has no dependencies, and is already in `uv.lock` through pytest. Gates: no other command imports either package (proved by `-X importtime` in S1's acceptance), and the installed size grows by under 2 MB. TOML stays on `tomllib` and JSON on `json`. Reverses if a pure-standard-library YAML subset parser is preferred over a dependency; this campaign does not write one.
- **D7 — The existing audit is the parity reference.** `src/repomod/audit.py` is not rewritten or migrated. Its line-oriented pnpm parsing (`_pnpm_locked_versions`, `_pnpm_inline_versions`), first-version-wins lock helper (`_locked_versions`), and root-plus-one-level JavaScript discovery (`_js_project_roots`) are the gaps the inventory closes. The inventory must contain every `(ecosystem, name, version)` that `_collect_locked` and `_collect_vuln_candidates` report for a clean checkout whose `HEAD` equals `origin/HEAD`, with the same direct/transitive classification. Every exception must be a documented inventory improvement, such as a second locked version the audit dropped. `docs/dependencies.md` lists the audit paths that could migrate onto the model later; nothing migrates in this campaign.
- **D8 — Coverage model.** Coverage issue kinds form a closed enum, each with a fixed severity:

  | Kind | Severity | Meaning |
  | --- | --- | --- |
  | `checkout_missing` | error | no checkout at `repository_path` |
  | `origin_mismatch` | error | origin URL does not match the catalog ID |
  | `default_ref_missing` | error | no `refs/remotes/origin/HEAD` |
  | `git_failure` | error | a Git read failed for this repository |
  | `parse_error` | error | a recognized input failed to parse |
  | `lock_mismatch` | error | a manifest declaration has no matching lock entry while a lock exists |
  | `missing_lock` | gap | an ecosystem that supports locks has a manifest but no lock |
  | `unsupported_format` | gap | a recognized input with no parser (D12) |
  | `dynamic_value` | gap | a version, image, URL, or ref built from a variable that cannot be resolved statically |
  | `symlink_skipped` | gap | a dependency input is a symlink |
  | `submodule` | gap | a gitlink whose dependency content is not inspected |
  | `vendored_unknown` | gap | a vendored tree with no recognizable metadata |
  | `non_utf8` | gap | an input that is not UTF-8 |

  A repository is `complete` with no issues, `partial` with only gap issues, `failed` with any error issue, and `unavailable` when the issue is `checkout_missing`, `origin_mismatch`, or `default_ref_missing`. "Zero dependencies" means `complete` with no edges; the dashboard renders it differently from every other state. `deps snapshot --strict` and `deps check --strict` exit 1 when any selected repository is `failed` or `unavailable`. Without `--strict`, `snapshot` writes partial data and exits 0.
- **D9 — Resolution states.** A closed enum on every edge:
  - `resolved`: a lock entry, digest, commit, or checksum fixes the version and source.
  - `conditional`: resolved, but included only under a recorded `condition` (marker, target, feature, extra, optional, or peer context).
  - `declared_only`: a declaration with a constraint and no lock for that ecosystem and project; `missing_lock` accompanies it when the ecosystem supports locks.
  - `unknown`: the version cannot be established; `dynamic_value` or `lock_mismatch` accompanies it.
  - `unsupported`: no parser exists; there are no edges, only the `unsupported_format` issue.

  "Genuinely absent" means no input file, no edge, and no issue. Tests cover all three of absent, `unknown`, and `declared_only`.
- **D10 — Identity and records.** Components use canonical Package URLs: `pkg:cargo`, `pkg:pypi` (normalized name; the original spelling kept in `display_name`), `pkg:npm` (scope percent-encoded as the PURL spec requires), `pkg:golang`, `pkg:swift`, `pkg:github` (Actions, `owner/repo` with optional `path` subpath), `pkg:docker` (with `repository_url` for registries other than Docker Hub), `pkg:apk`, and `pkg:generic` (Zig packages, toolchains, download artifacts, Cloudflare services, vendored code).
  - Source qualifiers:
    - mirror registries listed in `registries.toml` (`[cargo].source`, `[npm].registry`, `[python].index`) map to their public upstream identity, and the observed URL is recorded on the instance as `registry_url`;
    - `[cargo].native` and any other registry get a `repository_url` qualifier, so their components are distinct;
    - Git sources get `vcs_url` plus the commit;
    - path and workspace sources use `pkg:generic` with a `first_party` flag.
  - An instance ID is `<purl>#<context>`. The context is the pnpm peer suffix, a uv resolution-marker digest, or a Cargo source digest when one name and version exists in more than one source; otherwise it is empty.
  - A project ID is `<repo-relative-dir>:<ecosystem>`, with `.` for the root.
  - An edge carries:
    - `from` (project or instance) and `to` (instance);
    - `relation` (`direct` or `transitive`) and `scope` (closed enum: `runtime`, `dev`, `build`, `test`, `optional`, `peer`, `ci`, `toolchain`, `image`, `system`, `service`);
    - `optional`, `constraint` (as written), `resolved_version`, and `condition` (normalized string or null);
    - `source_kind` (closed enum: `registry`, `git`, `path`, `workspace`, `url`, `vendored`, `image`) and `resolution` (D9);
    - `evidence`: a list of `{path, locator}` pairs.
  - A locator is a key path for structured formats (`package[name=serde,version=1.0.219].dependencies[0]`, `importers[web].dependencies[react]`, `jobs.build.steps[3].uses`) or `L<line>` for line-oriented formats (Dockerfile, `go.mod`, `.gitmodules`, CMake). It is never an excerpt.
  - Each repository file records its inputs as `{path, blob}` pairs.
  - Model types are frozen dataclasses with enums. Parse raw input into them at the adapter boundary, and reject invalid states in constructors.
- **D11 — Ecosystem coverage, fitted to the fleet.** Counts were taken over all 52 managed repositories' `origin/HEAD` trees on 2026-11-06. Every format with a fleet presence gets a parser:
  - **Cargo** (453 `Cargo.toml`, 84 `Cargo.lock`): nested workspaces, `workspace = true` inheritance, renames (`package = "..."`), features and `default-features`, `[target.'cfg(..)'.*]`, build and dev dependencies, Git and path sources, excluded members, standalone nested projects, and full `Cargo.lock` edges disambiguated by `name version (source)`.
  - **uv** (13 `pyproject.toml`, 9 `uv.lock`): workspaces, optional dependencies, dependency groups, markers, extras, `tool.uv.sources` (git, path, editable, URL), multiple locked versions per resolution marker, and `requirements*.txt` (one in `webpki`) as `declared_only` inputs.
  - **pnpm** (7 locks, all `lockfileVersion: '9.0'`): multi-document files, where the first document is the package-manager environment lock (`packageManagerDependencies`, scope `toolchain`) and the last is the project lock. Also importers, `packages` versus `snapshots` (peer-context instances), aliases (`npm:`), `link:` and `workspace:` specs, scoped names, optional dependencies, overrides, and `ignoredOptionalDependencies`.
  - **npm** (3 `package-lock.json`, v3 assumed; any other version is `unsupported_format`): `packages` entries and nesting.
  - **Other JavaScript package managers:** `yarn.lock`, `bun.lock`, and `bun.lockb` are detected and reported as `unsupported_format`.
  - **Go** (4 `go.mod`, 3 `go.sum`): `require` (with `// indirect`), `replace`, `exclude`, `tool`, `toolchain`, `go.work`, and pseudo-versions. `go.sum` only proves that a checksum exists: it never creates an edge, and Go transitive edges come only from `go.mod`.
  - **Swift** (14 `Package.swift`, 4 `Package.resolved`): `Package.resolved` v1 to v3 pins. For `Package.swift`, a lexical scan of `.package(` calls whose arguments are all string literals (`url:` with `from:`, `exact:`, `branch:`, `revision:`, or `.upToNextMajor(from:)`); anything else is `dynamic_value`. The draft named `view` and `mactop`, but they have no external packages; the real consumers are `swift-bylaws` and `ghostty`.
  - **Zig** (54 `build.zig.zon`, all in `ghostty`): a parser for the ZON literal subset that reads `.dependencies` with `.url`, `.hash`, `.path`, and `.lazy`.
  - **GitHub Actions** (130 workflow files): job and step `uses:`, reusable workflows, local actions, SHA pins with their trailing `# vX` comment recorded as `constraint`, `docker://` images, and `container:`/`services:` images. Expressions (`${{ }}`) become `dynamic_value`.
  - **Containers** (`Dockerfile`, `Dockerfile.*`, `*.Dockerfile`): `FROM` with tag and digest, `ARG` defaults substituted when literal, multi-stage aliases (internal, not external), `COPY --from=<image>`, and `apk add` with `name=version` pins (scope `system`).
  - **Toolchains:** `rust-toolchain.toml`, `.node-version`, `.python-version`, `.tool-versions`, `.nvmrc`, `go.mod` `toolchain`, and `packageManager` in `package.json`.
  - **Download artifacts** in Dockerfile `RUN` and workflow `run:` lines: a literal `https://` URL in a logical command (continuation lines joined) is an `artifact` edge. It counts as pinned when the same logical command contains a 64-hex SHA-256 literal; a URL containing `$` is `dynamic_value`. Record only the URL and the pinned flag, never the command.
  - **Git submodules** (`.gitmodules` in 3 repositories): one edge per submodule with URL and gitlink commit, plus a `submodule` issue, because its contents are not inspected.
  - **Vendored trees** (`vendor/`, `third_party/`, `third-party/`, found in `ghostty`, `play`, `shadowdriver`, `syncthing`, `tansu`, `python-build`, and `webpki`): manifests inside them (`Cargo.toml`, `package.json`, `go.mod`) go through the normal adapters with `source_kind = vendored`; free-form metadata such as `README.chromium` is not parsed. A vendored root with no manifest gets one `vendored_unknown` issue.
  - **CMake** (8 `CMakeLists.txt`): `find_package`, `pkg_check_modules`, and `FetchContent_Declare`/`ExternalProject_Add` with literal `URL`, `URL_HASH`, `GIT_REPOSITORY`, and `GIT_TAG`; anything else is `dynamic_value`.
  - **Cloudflare** (`wrangler.toml`, `wrangler.json`, `wrangler.jsonc`, `wrangler.*.jsonc`; currently only `stats/cloudflare/wrangler.test.jsonc`): D1, R2, KV, Durable Object, service, and queue bindings as `service` edges to `pkg:generic/cloudflare-<kind>/<binding-name>`. Read JSONC with `io.strip_jsonc`.
- **D12 — Formats with no fleet presence get detection only.** These are reported as `unsupported_format` when present and are never parsed: Meson, Conan, vcpkg, pkg-config `.pc` files, `flake.nix` (2 present), Makefiles (76 present; their dependency facts are variable-driven and executing them is forbidden), Gradle, Maven, and Gemfile. This supersedes the draft's requirement to support Meson, Conan, vcpkg, pkg-config, and static Makefile declarations; none of them has a parseable fleet consumer. Reverses when a managed repository adds one.
- **D13 — First-party links need positive evidence.** An edge is `first_party` when one of these holds:
  - a Git source URL normalizes (case-insensitively, without `.git`, over https, ssh, or `git@`) to a catalogued `RepoId`;
  - a path or workspace source resolves inside the same repository;
  - a registry component's name equals a package that a catalogued repository's own manifest declares, and that repository is tagged `crates-io` or `pypi` for that registry, or the component comes from `[cargo].native`.

  Never infer a link from a similar name.
- **D14 — Determinism and layout.**
  - `data/dependencies/v1/index.json` holds `{schema: 1, parser_version, repositories: {"OWNER/REPO": {commit, inputs_fingerprint, state}}}`.
  - `data/dependencies/v1/repos/OWNER/REPO.json` holds `{schema, repository, tags, inputs, inputs_fingerprint, projects, components, instances, edges, issues}`.
  - There is no `schema.json`; the model types are the single source of truth.
  - `inputs_fingerprint` is the SHA-256 of the sorted `(path, blob)` list plus `PARSER_VERSION`, an integer in `snapshot.py` that is bumped whenever output semantics change. A repository file is rewritten only when its bytes change, so an application-code commit changes only `index.json`.
  - Serialize as `json.dumps(value, indent=1, sort_keys=True, ensure_ascii=False) + "\n"`, with every list sorted by ID, which keeps Git diffs line-local.
  - Writes are atomic: a temporary file in the same directory, then `os.replace`.
  - An unfiltered snapshot deletes files for repositories that are no longer selected. A `--repo` or `--tag` snapshot touches only the selected files and their `index.json` entries.
  - Extraction may use a thread pool (`--workers`, default 4). Output is identical for any worker count.
  - The inventory holds no absolute paths, timestamps, hostnames, or network facts.
- **D15 — `deps check` regenerates in memory.** It compares bytes against the tracked files, never writes, and exits 1 listing each drifted path. This supersedes "regenerate to a temporary location": no scratch files are needed.
- **D16 — CLI contract.** Exit codes: 0 means success, 1 means drift (`check`), strict-coverage failure (`--strict`), or a `why` with no path found, and 2 means an operational failure (the existing `main` handler). `--json` writes only JSON to stdout through `print_json`; progress goes to stderr.
  - `deps snapshot [--repo] [--tag] [--workers N] [--strict]` writes `data/dependencies/v1/`.
  - `deps check [--repo] [--tag] [--strict]` (D15).
  - `deps why PACKAGE [--repo] [--tag] [--max-paths 20] [--json]` reads the inventory only. `PACKAGE` is `name`, `ecosystem:name`, or either with `@version`; ecosystems use PURL type names. It prints every simple path from each project to each matching instance, shortest first, then lexicographic, up to `--max-paths` per project, with `truncated` when the cap is hit. Cycles are safe because paths never repeat a node.
  - `deps report [--repo] [--tag] [--json]` prints the D18 analyses.
  - `deps diff [--from REV] [--to REV] [--json]` reads `data/dependencies/v1` at Git revisions of repomod via `git show`; the default is `HEAD` against the working tree.
  - `deps enrich [--refresh] [--offline] [--json]` (D19).
  - `deps render [--output .work/dependencies/dashboard]` reads the inventory and the enrichment cache only.
  - Every subcommand has help text that says what it reads and writes. None prompts.
- **D17 — Redaction.** Strip userinfo from every URL, and drop query strings and fragments from download and registry URLs. Never record `.npmrc` auth lines, `_authToken`, `.netrc`, environment values, or `GITHUB_TOKEN`-style expressions. A secret-bearing URL is recorded redacted, never skipped. The dashboard escapes all text with `html.escape`, and embedded JSON escapes `<`, `>`, `&`, and U+2028/U+2029.
- **D18 — Analyses, each with explicit units and denominators.** No metric mixes names, versions, and instances.
  - Composition: repositories, projects, unique package names, unique `name@version`, instances, direct edges, and transitive edges, per ecosystem.
  - Concentration: repositories that depend directly versus transitively, per package.
  - Divergence: packages with more than one version across the fleet or within one repository, with exact versions, consumers, and sources.
  - Source risk: Git sources, unpinned Actions, floating image tags, missing locks, unpinned artifacts, vendored code, and coverage gaps.
  - Scope and target breakdown.
  - First-party links.
  - Curated-role overlap: repositories that carry two or more packages from one role set. The role sets are defined by `audit.py`'s banned lists and their named alternatives (Rust HTTP clients versus `h12tiny-*`/`laputa-*`, criterion versus `rustybench`); import those constants, do not copy them.
  - Freshness and vulnerabilities, only from the enrichment cache and labeled with its `fetched_at`.

  Speculative functional-overlap detection is a non-goal (see **Non-goals**).
- **D19 — Enrichment is separate, cached, and reused.** `deps enrich` reads the inventory and queries latest versions and OSV for resolved `cargo`, `pypi`, `npm`, and `golang` components. It reuses `audit.py`'s `_resolve_latest`, `_resolve_vulns`, `_osv_query_batch`, and the per-repository registry sources; rename the helpers it calls to public names in `audit.py` and update their callers, without changing behavior. Results go to `io.cache_file("deps-enrichment.json")` as `{fetched_at, results: {purl: {latest, latest_source, vulns, fetched_at}}}`. Results older than 24 hours render as stale. Components in other ecosystems render as `not checked`. Missing enrichment is never shown as clean. Enrichment never adds edges or changes identities. `snapshot`, `check`, `render`, `why`, `report`, and `diff` never open a socket.
- **D20 — Dashboard architecture.** A static multi-page site under `.work/dependencies/dashboard/`:
  - pages: `index.html` (overview), `landscape.html`, `divergence.html`, `packages.html` (one page, with `#pkg=<purl>` hash routing), `changes.html`, `coverage.html`, and `repos/OWNER/REPO.html`;
  - shared `assets/app.css`, `assets/app.js`, and `assets/search.js`, where `search.js` sets `window.DEPS_INDEX`;
  - page data inline in `<script type="application/json">`;
  - no `fetch`, no CDN, no web fonts, no framework, no charting library; Python string building with escaping, no Jinja;
  - precompute everything in Python, so the browser only filters, sorts, and expands;
  - the landscape matrix shows at most 60 columns (the most prevalent packages, plus any the user selects from search) and draws dense cells with CSS grid, not SVG;
  - `changes.html` compares the working inventory against `HEAD~1` of `data/` when that revision exists, and otherwise says "no baseline".

  It works under `file://` in `agent-browser`.
- **D21 — Visual direction.** Tufte-style: high density, thin rules instead of cards, small multiples, dot plots for divergence, inline bars and sparklines, direct labels, restrained semantic color (one accent for gaps and errors, one for direct use), a system serif for prose (`Iowan Old Style, Charter, Georgia, serif`), `ui-monospace` for identifiers, and no animation. Also no KPI tiles and no force-directed graphs. Keyboard support: `/` focuses search, `j`/`k` move through rows, `Enter` opens, and a copy button copies a PURL or a `why` path. Use the `dataviz` skill for every chart. Passing D22 is the bar; owner taste review is not.
- **D22 — Dashboard acceptance is mechanical.** Run `agent-browser` on `file://` pages at 1440×900 and 390×844. It must find no console errors and no horizontal page overflow at 1440. The overview must render within 1.5 s. Search must find `serde`, and `#pkg=pkg:cargo/serde` must open the package view. Every repository page must link back to the overview. Save screenshots under `.work/dependencies/shots/` and inspect them for clipping, overlap, and unreadable labels before accepting S9 and S10.
- **D23 — Fleet parity test.** `tests/test_deps_fleet.py` is skipped unless `REPOMOD_FLEET=1`. When enabled, it runs `snapshot` in memory over the live catalog and asserts:
  - D7 parity for every clean checkout whose `HEAD` equals `origin/HEAD`;
  - zero `parse_error` issues;
  - byte-identical output across two runs and across `--workers 1` and `--workers 8`;
  - unchanged `git status --porcelain` and `HEAD` in every checkout.
- **D24 — Unlisted details use the smallest reversible assumption.** Examples: diagnostic wording, test names, fixture layout, ZON grammar corners, CSS details, and exact report table columns. Record one only when a later slice depends on it, inside the bounds of D8–D22.

### Delegated

- **Parser corner cases** not named in D11: handle them conservatively. An input you cannot parse with confidence becomes a `parse_error` or `dynamic_value` issue, never a guessed edge.
- **Analysis thresholds** (the top N in tables, matrix columns up to 60, and what counts as a divergence hotspot): pick them, and state them in `docs/dependencies.md`.
- **Fixture size:** synthetic Git repositories built with `tests/git_helpers.py`, plus short excerpts from public managed repositories when a real format quirk needs proof. Never from `private-tool` repositories.

## Baseline

Verified on 2026-11-06 at `102b7b4`, after `uv run repomod sync`. All 52 checkouts were current, except that `zzz` is on `all-green/zzz`.

- `make test`: 614 passed, 1 skipped, 9.5 s.
- `uv run ruff check src tests`: 0.07 s. `uv run ty check`: 0.14 s. Both clean.
- `uv run repomod sync`: 10.2 s.
- Fleet Git read (ls-tree plus batched cat-file of 871 inputs): 1.19 s.
- `mdtool`, `agent-browser`, and `perf-gate-ready` are installed. `ultrareview` is not: S13 uses `review` over the campaign diff instead.
- Rough fleet scale: about 8,300 locked package entries; the largest repositories are `shadowdriver`, `rage`, `obsidian-ferrisearch`, and `stats`.

## Performance

These budgets are measured and logged in each slice's commit body from S2 on. Until S12 a slice fails only past 3× a budget. From S12 on the budgets gate. Time the runs with `perf-gate-ready` exiting 0 before and `perf-gate-ready -1` after; if the host stays busy, write "perf gate deferred: host busy". Size budgets always gate.

| Measure | Budget |
| --- | --- |
| `deps snapshot` over the full fleet, CPU time | ≤ 15 s |
| `deps check`, full fleet | ≤ 15 s |
| `deps why serde`, `deps report --json` | ≤ 2 s each |
| `deps render` | ≤ 10 s |
| Tracked `data/dependencies/v1/` | ≤ 15 MB total, ≤ 1.5 MB for any one repository file (gates S8's first data commit) |
| Dashboard output | ≤ 10 MB total; overview first render ≤ 1.5 s |
| `make test` | ≤ 30 s; any one new test file ≤ 10 s |
| `uv run python -X importtime -m repomod.cli --help` | imports neither `ruamel` nor `packaging` |

## Sequence

Items are serial unless marked otherwise. Each item lists its owned files; a test file named `tests/test_deps_<x>.py` is owned with its item. An item may touch `src/repomod/cmd/deps.py` and `src/repomod/cli.py` only to wire its own subcommand.

### Phase 1: parity and completeness

**S1: core model, tree reader, snapshot, and check (size L).**
- Owns: `pyproject.toml`, `uv.lock` (D6), `src/repomod/dependencies/{__init__,model,git_tree,discover,snapshot}.py`, `adapters/__init__.py` (an empty registration tuple), `src/repomod/cmd/deps.py`, `src/repomod/cli.py`, new functions in `src/repomod/git.py`, `tests/test_deps_snapshot.py`.
- Delivers:
  - D2–D4, D8–D10 types, D14, D15, and D17's URL redaction helper;
  - discovery of every input name in D11 and D12, which reports `unsupported_format` for each until its adapter lands;
  - `deps snapshot` and `deps check`.
- Tests cover:
  - selection excluding ignored and pending repositories;
  - reading `origin/HEAD` while a different branch is checked out and the working tree is dirty;
  - each D4 issue (missing checkout, origin mismatch, missing `origin/HEAD`, symlinked input, gitlink, non-UTF-8);
  - D14 behavior: unchanged files are not rewritten, a filtered snapshot preserves the other repositories, an unfiltered snapshot deletes dropped ones, a second run is byte-identical, and `--workers 1` matches `--workers 8`;
  - `check` drift exits 1 and writes nothing;
  - with `socket.socket.connect` patched to raise, `snapshot` and `check` still succeed;
  - the checkout's `HEAD` and `status --porcelain` are unchanged.
- Acceptance: `uv run pytest tests/test_deps_snapshot.py && ! (uv run python -X importtime -m repomod.cli --help 2>&1 >/dev/null | rg -q 'ruamel|packaging')`

**S2: Cargo adapter and first fleet pilot (size L).** Prerequisite: S1.
- Owns: `adapters/cargo.py`, `tests/test_deps_cargo.py`, the registration line.
- Tests cover every Cargo case in D11, plus multiple versions of one crate, Git and path sources, the mirror mapping (D10), and `lock_mismatch`.
- Pilot: run `uv run repomod deps snapshot` against the live fleet with `TMPDIR` set, then record the wall and CPU time, total bytes, and the largest file in the commit body. Fix every `parse_error` before acceptance, or justify it as an issue the input truly has. Do not commit the data; reset `data/` after measuring.
- Acceptance: `uv run pytest tests/test_deps_cargo.py`, plus the pilot log.

**S3: uv adapter (size M).** Prerequisite: S2.
- Owns: `adapters/python.py`, `tests/test_deps_python.py`.
- Tests: the uv cases in D11, extras and markers producing `conditional`, two uv resolutions of one package, and `requirements.txt` producing `declared_only`.
- Acceptance: `uv run pytest tests/test_deps_python.py`.

**S4: JavaScript adapter (size L).** Prerequisite: S2.
- Owns: `adapters/javascript.py`, `tests/test_deps_javascript.py`.
- Tests: D11's pnpm and npm cases, including the two-document v9 lock, peer-suffixed snapshots as distinct instances, multiple importers, aliases, scoped names, and `yarn.lock` as `unsupported_format`. The adapter must not contain one regular expression applied to lockfile text.
- Acceptance: `uv run pytest tests/test_deps_javascript.py && ! rg -n 're\.(match|search|findall|finditer|fullmatch)' src/repomod/dependencies/adapters/javascript.py`.

**S5: Go, Swift, and Zig adapters (size M).** Prerequisite: S2.
- Owns: `adapters/{go,swift,zig}.py`, `tests/test_deps_{go,swift,zig}.py`.
- Tests: Go replacements, pseudo-versions, `// indirect`, and `go.sum` creating no edge; `Package.resolved` v2 and v3; `Package.swift` literal and dynamic declarations; ZON URL, hash, path, and lazy dependencies.
- Acceptance: `uv run pytest tests/test_deps_go.py tests/test_deps_swift.py tests/test_deps_zig.py`.

**S6: infrastructure adapters (size L).** Prerequisite: S2.
- Owns: `adapters/{actions,containers,toolchains,artifacts,submodules,vendored,cmake,cloudflare}.py`, `tests/test_deps_infra.py`.
- Tests:
  - pinned and floating Actions, reusable workflows, and `${{ }}` producing `dynamic_value`;
  - Docker digests, `ARG` substitution, multi-stage aliases, and `apk` pins;
  - toolchain files;
  - artifact URLs, pinned and unpinned;
  - submodules, vendored roots, and CMake literal and dynamic declarations;
  - Wrangler bindings;
  - a userinfo URL being redacted.
- Acceptance: `uv run pytest tests/test_deps_infra.py`.

**Parallel rule:** after S2 is accepted, S3, S4, S5, and S6 own disjoint files and may run as at most two concurrent lanes (pairs S3+S5 and S4+S6, in either order). The parent alone edits the registration tuple in `adapters/__init__.py` when it integrates each lane. A lane that needs a model change stops and hands the change to the parent, which lands it serially before resuming lanes.

**S7: graph, `why`, `report`, and first-party links (size L).** Prerequisites: S3–S6.
- Owns: `dependencies/{graph,analysis}.py`, `tests/test_deps_graph.py`, `tests/test_deps_fleet.py` (D23).
- Delivers D13, D16 `why` and `report`, and D18 except freshness.
- Tests:
  - "why tokio" in a Cargo workspace;
  - a reverse-consumer query;
  - a graph cycle;
  - `--max-paths` truncation;
  - a first-party Git link and a near-name non-link;
  - divergence across and within repositories;
  - every metric carrying a unit and a denominator;
  - `why` and `report` with sockets blocked.
- Acceptance: `uv run pytest tests/test_deps_graph.py && REPOMOD_FLEET=1 uv run pytest tests/test_deps_fleet.py`, then the full suite (milestone).

**S8: commit the fleet inventory (size S).** Prerequisite: S7.
- Run `uv run repomod deps snapshot`, then confirm the size budgets and that `uv run repomod deps check --strict` exits 0. If a strict failure comes from a real input defect in a managed repository, record it in **Status** and commit with plain `check` passing. Do not edit the managed repository.
- Commit `data/dependencies/v1/` as `chore(deps): add fleet dependency inventory`. Record the repository counts by state, the project count, unique packages, edges, and supported and unsupported formats in the commit body.
- Acceptance: `uv run repomod deps check && du -sh data/dependencies/v1`.

**S9: diff and enrichment (size M).** Prerequisite: S8.
- Owns: `dependencies/{diff,enrich}.py`, public renames in `src/repomod/audit.py` and their callers, `tests/test_deps_diff.py`, `tests/test_deps_enrich.py`.
- `diff` tests: additions, removals, upgrades, downgrades (PEP 440 for `pypi`, numeric dotted keys for the rest, `changed` when neither orders), new sources, resolved and new issues, and "no baseline". Build the history from fixture commits in a temporary Git repository.
- `enrich` tests use fake OSV and registry responses, as `tests/test_audit.py` does. Run one live `uv run repomod deps enrich` to prove the path; if the network is unavailable, move the live proof to **Needs environment** and continue.
- Acceptance: `uv run pytest tests/test_deps_diff.py tests/test_deps_enrich.py tests/test_audit.py`.

**S10: dashboard (size L).** Prerequisite: S9.
- Owns: `dependencies/render.py`, `src/repomod/dependencies/assets/` (packaged with `uv_build`), `tests/test_deps_render.py`.
- Delivers D20–D22, with every view from the draft:
  - overview: coverage, totals, ecosystem composition, prevalence, divergence hotspots, source risk, and recent changes;
  - landscape matrix, sortable by commonality, ecosystem, repository, and coverage;
  - divergence dot plot;
  - repository detail: projects, scope counts, direct dependencies, an expandable graph, sources, duplicates, gaps, findings, and evidence links to `https://github.com/OWNER/REPO/blob/<commit>/<path>`;
  - package detail: identity, versions, consumers, direct versus transitive, entry paths, scope, platform, optionality, and enrichment state;
  - changes;
  - coverage.
- Tests: HTML escaping of a hostile package name and path, the `</script>` escape, no `fetch(` in assets, render with sockets blocked, and `unknown` rendered differently from zero.
- Then run D22 on the real inventory and fix what the screenshots show.
- Acceptance: `uv run pytest tests/test_deps_render.py && uv run repomod deps render`, then the D22 `agent-browser` checks, then the full suite (milestone).

### Phase 2: hardening

**S11: skill, docs, and help (size M).** Prerequisite: S10.
- Owns: `.agents/skills/audit-dependencies/SKILL.md`, `docs/dependencies.md`, `docs/audit.md` (point Swift and inventory questions to the new guide), `docs/design.md` (module map), `README.md`, `AGENTS.md` (the guide link only).
- The skill follows `upgrade-dependencies`' frontmatter with `disable-model-invocation: true`. It is observational only: select the fleet, check the checkouts, snapshot, check, diff, render, report insights, and explain gaps. It never upgrades, installs, or rewrites anything.
- `docs/dependencies.md` states the D8 and D9 semantics, the D18 units, the delegated thresholds, the D7 migration candidates, and the D12 non-goals with reasons.
- Acceptance: `mdtool check --untracked docs/dependencies.md .agents/skills/audit-dependencies/SKILL.md && uv run repomod deps --help && for c in snapshot check why report diff enrich render; do uv run repomod deps $c --help >/dev/null || exit 1; done`.

### Phase 3: optimization

**S12: budgets gate (size M).** Prerequisite: S11.
- Measure every row of **Performance**, interleaving baseline and candidate runs. Use the `make-it-fast` and `benchmark-checklist` methods, and report the median CPU time and spread. Fix any miss.
- Then do one polish pass on the dashboard from fresh D22 screenshots: density, alignment, and label truncation.
- Acceptance: every budget row is met, or "perf gate deferred: host busy" is recorded; D22 passes.

### Phase 4: close

**S13: close (size S).** Prerequisite: S12.
- Delete code the campaign made dead. Prove liveness with `rg` for each removed symbol and the full suite (`dead-code-archaeology`). Only `audit.py` helpers this campaign renamed may change there.
- Run `review` over `102b7b4..HEAD` and fix its P1 and P2 findings.
- Run the full suite and `REPOMOD_FLEET=1 uv run pytest tests/test_deps_fleet.py`, then `uv run repomod deps check --strict`.
- Rewrite **Status** with the final report: repositories selected, inspected, partial, and failed; projects; packages; edges; supported and unsupported formats; the dashboard path; at least five findings from real data the inventory supports; outstanding coverage limits with their causes; and a short architecture summary.
- Remove `.work/wt/*` and `.work/dependencies/shots`.

## Acceptance

| Deliverable | Proof | Success | Failure |
| --- | --- | --- | --- |
| Read-only extraction | D23 and S1 tests | checkouts' `HEAD` and status unchanged; no socket opened | any mutation or connection |
| Determinism | S1 tests, D23 | byte-identical across runs and worker counts | any difference |
| Coverage honesty | S1–S6 tests, `deps check --strict` | every gap is a typed issue; strict exits 1 on `failed`/`unavailable` | a parse failure with no issue, or "zero" shown for unknown |
| Audit parity | D23 | every audit `(ecosystem, name, version)` present, or listed as an improvement | an unexplained miss |
| Inventory in Git | S8 | `deps check` exits 0 on `master`; size budgets met | drift or an over-budget commit |
| Agent interfaces | S7, S9 tests | `why`, `report --json`, and `diff --json` give deterministic JSON with units | progress text on stdout, or a mixed-unit metric |
| Enrichment | S9 | fake-network tests pass; stale and missing results render as unknown | enrichment changes edges, or missing shows as clean |
| Dashboard | S10, D22 | all views render offline with no console errors or overflow | a broken view, `fetch`, or a CDN |
| Skill and docs | S11 | `mdtool` clean; help text on every subcommand | missing or stale docs |
| Budgets | S12 | every **Performance** row met | a regression larger than the measured spread |

## Needs environment

- A live `deps enrich` run, if S9 finds no network: `uv run repomod deps enrich --refresh && uv run repomod deps render`.
- Publishing: the owner pushes `master` after review. The campaign never pushes.

## Execution boundary and non-goals

**Authorized:** local edits, tests, `deps` commands, `sync`, Git object reads of canonical checkouts, network inside `deps enrich`, `agent-browser` on `file://`, and local commits on `master`.

**Forbidden:** everything listed under hard stops in **Charter**.

**Contract changes for the owner:**
- the first runtime dependencies, `ruamel.yaml` and `packaging` (D6);
- the new `repomod deps` command group and its exit codes (D16);
- the new tracked data directory `data/dependencies/v1/` and its schema (D14);
- public renames of the `audit.py` enrichment helpers (D19), which are internal callers only.

**Non-goals:**
- Migrating existing audit or upgrade rules onto the inventory (D7). The candidates are documented.
- A CycloneDX export. It was optional in the draft and has no consumer.
- License metadata and deps.dev enrichment. Each would be a new network surface with no consumer; reverses when a report needs licenses.
- Functional-overlap detection beyond curated role sets. There is no deterministic evidence, and guesses would mislead.
- Parsers for formats with no fleet presence (D12).
- Executing `cargo metadata`, `go mod graph`, or Swift and Zig manifests. They are machine-dependent or execute repository code.
- A private overlay or visibility policy. Superseded by D1.
- A database, background worker, hosted dashboard, history ledger, or JSON Schema file.

## Reference

- Catalog selection: `src/repomod/catalog.py:133` (`load_catalog`), `:362` (`repository_path`); `src/repomod/cmd/common.py:32` (`select_repositories`), `:53` (`print_json`).
- Git: `src/repomod/git.py:45` (`run_git`), `:74` (`expected_origin`).
- CLI: `src/repomod/cli.py:25` (`_parser`), `:120` (`main`, exit 2 on operational errors).
- Audit reference: `src/repomod/audit.py:662` (`_locked_versions`), `:757` (`_js_project_roots`), `:785`–`:853` (pnpm line parsing), `:1317` (`_osv_query_batch`), `:1369` (`_collect_locked`), `:1459` (`_collect_vuln_candidates`), `:1913` (`_resolve_latest`), `:2000` (`_resolve_vulns`).
- I/O helpers: `src/repomod/io.py:24` (`strip_jsonc`), `:128` (`cache_file`).
- Registry mirrors: `registries.toml`.
- Test helpers: `tests/git_helpers.py`, `tests/conftest.py:9` (`git_environment`).
- Prior campaign conventions: `campaign-oci.md`.
