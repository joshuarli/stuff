# repomod

repomod checks repository policy across the `catalog.toml` fleet and opens pull requests for safe fixes.

## Get started

Use Python 3.15, [`uv`](https://docs.astral.sh/uv/), and Git. Install `gh` and authenticate with
`gh auth login`. Keep `pinact` on `PATH` for GitHub Actions checks and fixes, and provide the
read-only documentation auditor at `~/.local/bin/mdtool`.

From this checkout:

```sh
uv sync
uv run repomod --help
```

## Catalog and checkouts

`catalog.toml` selects repositories and policy tags. Checkouts live under `~/repos` by default.
Set `REPOMOD_ROOT` to use another absolute path. Personal-owner repositories use a flat path.
Other owners use `OWNER/REPO`.

## Discover and add repositories

`repomod discover` compares active GitHub repositories against `catalog.toml`. It adds new
repositories with empty tag arrays and lists pending repositories and tracked entries missing from GitHub.
It checks every owner in the catalog unless you pass `--owner`. The command writes the catalog.

```sh
uv run repomod discover
uv run repomod discover --owner joshuarli
```

Add tags to a pending entry in its `[repos."owner"]` section to manage it. For example,
change `example = []` to `example = ["tool"]`. Include at least one primary tag. Add `"IGNORE"`
to exclude a repository from management and discovery; other tags can stay for later. See
[tagging semantics](docs/tagging.md) for tag meanings. Then fetch checkouts:

Discovery's JSON output includes `pending`, `uncatalogued`
(entries added in this run), and `missing`.
If your catalog has a `[pending]` table from an earlier version, move each entry to its
owner's `[repos."owner"]` table and remove the `[pending]` table.
Move entries from an older `[ignored]` table to their owner sections and add `"IGNORE"` to their tags.

```sh
uv run repomod sync --dry-run
uv run repomod sync
```

Tag edits need a separate publish step: `sync` only manages checkouts and never
touches GitHub topics. Preview the difference with `topics diff`, then publish
the catalog tag set with `topics apply`:

```sh
uv run repomod topics diff --repo joshuarli/mypypi
uv run repomod topics apply --repo joshuarli/mypypi
```

When a repository leaves the catalog (for example after a rename), its old
checkout stays on disk. Preview the leftovers with `gc --dry-run`, then remove
them with `gc`. `gc` only removes clean checkouts and empty directories. It
skips checkouts with uncommitted changes, unpushed commits, or an unverified
layout, as well as stray files and symlinks, unless you pass `--force`:

```sh
uv run repomod gc --dry-run
uv run repomod gc
```

## Status and lint

`repomod status` shows only what needs attention: repositories with local Git work, CI or workflow
failures, conflicting pull requests, forks behind their upstream, or 10 or more commits since the last
successful release (latest published GitHub Release, or newest version tag when the
repository has no releases). Passing repositories and passing workflow entries are omitted.
`--json` still reports every selected repository. `status --offline` reads cached CI, upstream,
and release data without network calls. `--failed` narrows the report to CI failures, conflicting
pull requests, and forks behind their upstream.

```sh
uv run repomod status --offline
uv run repomod status --failed
```

`repomod lint` reports policy findings. Use `--tag` or `--repo`
to select repositories. See [tagging semantics](docs/tagging.md) for catalog tag meanings.

`lint` and `status` accept `--checkout PATH` with exactly one `--repo` to evaluate a Git worktree
(for example a fix branch) instead of the canonical checkout, so a fix can be verified before it is
fast-forwarded. The path must be a real directory with no symlink components, a Git work tree
whose `origin` matches the catalogued repository. It is only read.

```sh
uv run repomod lint --repo acme/tool --checkout .work/wt/fix
```

## Fix

`repomod fix` syncs the selected repositories, repairs policy violations, and opens a pull
request per repository. Each successful fixer gets one local commit on a `repomod/fix/*` branch,
which is pushed and opened (or reused) as a pull request.
Use `--rule` to select fixers and `--repo`/`--tag` to select repositories.

```sh
uv run repomod fix --rule audit-upgrades
```

See [policy development](docs/policy-development.md) for rule behavior. Report problems in the
[repomod issue tracker](https://github.com/joshuarli/repomod/issues).

See [GitHub Actions integration](docs/policy-development.md#github-actions-with-pinact) for the
Pinact checker, approved catalog, and fix-plan flow.

## Pins

`repomod pins refresh` updates the approved pin files from their upstream sources:
`actions.toml`, `python-tools.toml`, `typescript-tools.toml`, and `musl-toolchain.toml`.
`--only` restricts the run to `actions`, `python`, `typescript`, or `musl`. `--check`
fetches and compares without writing, prints the files that would change, and exits 1 on
drift; a failed fetch or malformed source exits 2 and writes nothing. Pin bumps that select
a new toolchain version, such as LLVM, mold, or rustup, are manual, checksum-verified edits.
`repomod lint --rule linux-musl-toolchain` enforces the lock across the conventional Rust
builders, tansu, and the bespoke musl builder adapters.

```sh
uv run repomod pins refresh
uv run repomod pins refresh --check
uv run repomod pins refresh --only musl
```

## Canonical musl image

`docker/musl/Dockerfile` builds the shared musl toolchain image. `repomod musl build`
validates its build arguments and package pins against `musl-toolchain.toml`, builds one
platform with the lock's values, and writes a local OCI layout. It prints the layout's
manifest digest and has no push path. Publishing is an owner-run step: build with `--push`,
record the index digest in `[image]`, then run `pins refresh --check`.

```sh
uv run repomod musl build --platform linux/amd64 --output .work/oci/musl
```

## Dependency inventory

`repomod deps` keeps a deterministic, Git-tracked inventory of every catalogued repository's
dependency inputs in `data/dependencies/v1`. It records projects, package instances, edges, and
typed coverage issues for Cargo, uv, pnpm/npm, Go, Swift, Zig, GitHub Actions, containers,
toolchains, submodules, CMake, Wrangler, and vendored trees. `snapshot`, `enrich`, and `render`
write; the other commands read the inventory only. Only `enrich` uses the network, and it writes
a local cache.

```sh
uv run repomod deps snapshot
uv run repomod deps check --strict
uv run repomod deps why tokio
uv run repomod deps report
uv run repomod deps diff --from HEAD~1
uv run repomod deps enrich
uv run repomod deps render
```

For a local server, `make deps` renders the dashboard and serves it at
`http://127.0.0.1:8765` with `srv`; use `make deps PORT=9000` for another port.
The dashboard scripts are TypeScript; run `make dashboard-ts` after editing them.

See [dependency inventory](docs/dependency-inventory.md) for the data layout, coverage kinds,
update flow, and limits. Open the generated dashboard with
`file:///Users/josh/d/repomod/.work/dependencies/dashboard/index.html`; see
[dependency dashboard](docs/dependency-dashboard.md). The repo-local
[`upgrade-the-world`](.agents/skills/upgrade-the-world/SKILL.md) skill upgrades every public,
non-fork repository and loops until CI is green.

## Push

`repomod push` pushes each selected checkout's current branch when it is ahead of
its origin upstream. It reports `pushed`, skips clean or non-pushable checkouts
with a reason, and fails loudly without rewriting remote history when the remote
has moved. Use `--repo`/`--tag` to select repositories.

```sh
uv run repomod push
```

## Merge

`repomod merge` merges open pull requests whose head branch is a `repomod/fix/*` branch, which is
how `fix` identifies its pull requests. It pins each merge to the head commit listed before the
merge, so a branch that moves is rejected instead of merging unreviewed commits. Drafts are
skipped and merge failures are reported; both leave the pull request open. `--dry-run` lists what
would merge. The command reports listing progress and a final summary, including when no pull
requests are found. Use `--repo`/`--tag` to select repositories.

```sh
uv run repomod merge --dry-run
uv run repomod merge
```

## Dependency upgrades

The `audit-upgrades` rule reports locked direct dependencies that lag their registry.
`repomod fix --rule audit-upgrades` puts the upgrades up as pull requests.
Upgrades that need manifest edits stay manual. See [dependency audit](docs/audit.md)
for rule behavior.
