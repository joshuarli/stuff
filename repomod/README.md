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

## Dependency upgrades

The `audit-upgrades` rule reports locked direct dependencies that lag their registry.
`repomod fix --rule audit-upgrades` puts the upgrades up as pull requests.
Upgrades that need manifest edits stay manual. See [dependency audit](docs/audit.md)
for rule behavior.
