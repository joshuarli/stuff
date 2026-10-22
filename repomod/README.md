# repomod

repomod keeps local checkouts of the repositories in `catalog.toml`, shows Git and GitHub
status, checks repository policy, and can repair safe violations with local commits.

## Get started

Use Python 3.14, [`uv`](https://docs.astral.sh/uv/), and Git. Install `gh` and authenticate
with `gh auth login` for commands that read GitHub. Install `actionlint` for workflow checks
and fixes.

From this checkout:

```sh
uv sync
uv run repomod --help
```

## Catalog and checkouts

`catalog.toml` selects managed repositories and their policy tags. The checkout root defaults
to `~/repos`; set `REPOMOD_ROOT` to use another absolute path. `repomod path OWNER/REPO`
prints a repository's canonical checkout path. Checkouts owned by the catalog's personal owner
live directly under the root; other owners use `OWNER/REPO` subdirectories.

`repomod discover` lists active repositories owned by the configured personal account and
`laputa-systems` that are missing from the catalog. It does not edit the catalog or clone
repositories. Add entries explicitly, then inspect them with `list`:

```sh
uv run repomod add OWNER/REPO --tag tool
uv run repomod list
```

## Sync and status

`repomod sync` clones missing active catalog entries and safely fast-forwards existing
checkouts. It reads GitHub metadata, then skips Git fetch when a clean checkout on the default
branch already matches the reported tip. Dirty, detached, non-default, ahead, and divergent
checkouts keep their checked-out files and local commits. Progress goes to stderr; sorted
repository results go to stdout. `sync --dry-run` inspects local checkouts without network
calls. A failure for one repository does not stop the remaining entries.

`repomod status` shows local checkout, GitHub default-branch, CI, and quick policy status.
`status --offline` makes no network calls. `--failed` filters to CI failures, policy errors,
and operational failures.

```sh
uv run repomod sync
uv run repomod status --offline
uv run repomod status --failed
```

## Policy and changes

`repomod lint` reports policy findings. Use `repomod rules` to list the rules and
`--tag` or `--repo` to select repositories. Catalog tag meanings are documented in
[tagging semantics](docs/tagging.md).

```sh
uv run repomod lint --tag tool
uv run repomod lint --repo OWNER/REPO
```

`repomod topics diff` compares GitHub topics with catalog tags. `topics apply` replaces the
whole topic set with those tags. Each successful `repomod fix` fixer that changes files gets
one local commit. Push only with `fix --push` or `repomod push`; pushes are normal fast-forward
pushes. For rule changes, see [policy development](docs/policy-development.md).
