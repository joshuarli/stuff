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

`repomod discover` compares GitHub against `catalog.toml`. It lists uncatalogued
repositories and catalog entries missing from GitHub. It checks every catalogued
owner unless you pass `--owner`.

```sh
uv run repomod discover
uv run repomod discover --owner joshuarli
```

Add each wanted repository to `catalog.toml` as `name = ["tags"]` under its
`[repos."owner"]` section. Include at least one primary tag. See
[tagging semantics](docs/tagging.md) for tag meanings. Then fetch checkouts:

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

`repomod status` shows local Git, current default-branch CI, the latest status for each GitHub
workflow, outstanding ticket headings in each root `TODO.md`, lint rules with findings, and, for
forks, how many commits behind their upstream parent they are.
`status --offline` reads cached CI and upstream data without network calls. `--failed` filters to CI failures,
error-level policy findings, operational lint findings, and forks behind their upstream.

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

`repomod fix` repairs policy violations and opens a pull request per repository.
Each successful fixer gets one local commit on a `repomod/fix/*` branch,
which is pushed and opened (or reused) as a pull request.
Use `--rule` to select fixers and `--repo`/`--tag` to select repositories.

```sh
uv run repomod fix --rule audit-upgrades
```

See [policy development](docs/policy-development.md) for rule behavior. Report problems in the
[repomod issue tracker](https://github.com/joshuarli/repomod/issues).

See [GitHub Actions integration](docs/policy-development.md#github-actions-with-pinact) for the
Pinact checker, approved catalog, and fix-plan flow.

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
would merge. Use `--repo`/`--tag` to select repositories.

```sh
uv run repomod merge --dry-run
uv run repomod merge
```

## Dependency upgrades

The `audit-upgrades` rule reports locked direct dependencies that lag their registry.
`repomod fix --rule audit-upgrades` puts the upgrades up as pull requests.
Upgrades that need manifest edits stay manual. See [dependency audit](docs/audit.md)
for rule behavior.
