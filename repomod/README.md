# repomod

repomod keeps local checkouts of the repositories in `catalog.toml`, shows GitHub CI status,
checks repository policy, and can repair safe violations with local commits.

## Get started

Use Python 3.15, [`uv`](https://docs.astral.sh/uv/), and Git. Install `gh` and authenticate with
`gh auth login` for commands that read GitHub. Install `actionlint` for workflow checks and fixes,
and provide the read-only documentation auditor at `~/.local/bin/mdtool`.

From this checkout:

```sh
uv sync
uv run repomod --help
```

## Catalog and checkouts

`catalog.toml` selects repositories and policy tags. Checkouts live under `~/repos` by default.
Set `REPOMOD_ROOT` to use another absolute path. Personal-owner repositories use a flat path.
Other owners use `OWNER/REPO`. Run `repomod path OWNER/REPO` to print a checkout path.

`repomod discover` lists active repositories owned by the configured personal account and
`laputa-systems` that are missing from the catalog. It does not edit the catalog or clone
repositories. Add entries explicitly:

```sh
uv run repomod add OWNER/REPO --tag tool
```

## Sync and status

`repomod sync` clones missing active entries and safely fast-forwards clean checkouts on the
default branch. It skips Git fetch when the checkout already matches GitHub's reported tip.
Dirty, detached, non-default, ahead, and divergent checkouts keep their checked-out files and
local commits. For forks, sync also ensures an `upstream` remote pointing at the parent
repository and fetches the parent default branch. `sync --dry-run` inspects local state without network calls. Progress goes to
stderr. Sorted repository results go to stdout. A failure for one repository does not stop the
others.

`repomod status` shows local Git, current default-branch CI, the latest status for each GitHub
workflow, outstanding ticket headings in each root `TODO.md`, lint rules with findings, and, for
forks, how many commits behind their upstream parent they are. It
uses compact repository blocks with terminal colors when stdout is interactive. `status --details`
prints each lint message, failed CI check, and upstream error. `status --json` includes workflow statuses, full lint
findings, upstream comparison details, and the `todo_items` count. A TODO item is a standardized `### ID · Pn · title` heading.
`status --offline` reads cached CI and upstream data without network calls. `--failed` filters to CI failures,
error-level policy findings, operational lint findings, and forks behind their upstream.

```sh
uv run repomod sync
uv run repomod status --offline
uv run repomod status --failed
```

## Policy and changes

`repomod lint` reports policy findings. Use `repomod rules` to list rules and `--tag` or `--repo`
to select repositories. See [tagging semantics](docs/tagging.md) for catalog tag meanings.

`repomod topics diff` compares GitHub topics with catalog tags. `topics apply` replaces the
whole topic set with those tags. Each successful `repomod fix` fixer that changes files gets one
local commit. Push only with `fix --push` or `repomod push`. Pushes are normal fast-forward
pushes. `fix --push` also opens or reuses a pull request for each published fix branch.
`repomod pins refresh` updates the local approved GitHub Action, Python, and TypeScript tool pins.
See [policy development](docs/policy-development.md) for rule behavior. Report problems in the
[repomod issue tracker](https://github.com/joshuarli/repomod/issues).

## Dependency audit

`repomod audit` checks registry dependencies across Python, TypeScript, Rust, and Go
(Swift is out of scope). It trusts lockfiles, reports outdated direct dependencies
against their registries, queries OSV for known vulnerabilities, and enforces
registry-only bans. Use `--offline` to reuse cached registry data, `--refresh` to
ignore the cache, and `--fix` to run native upgraders (`uv lock --upgrade`,
`pnpm update --latest`, `cargo update`, `go get -u`) when available.
`repomod fix --rule audit-upgrades --push` puts the same upgrades up as pull
requests through the standard autofix flow. See [dependency audit](docs/audit.md)
for rule behavior.
