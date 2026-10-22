# repomod

repomod keeps local checkouts of the repositories in `catalog.toml`, reports Git and
GitHub Actions status, checks tag-selected policy, and repairs safe violations with local
commits.

```sh
repomod discover
repomod sync
repomod status
repomod lint
repomod fix
repomod push
```

Install Python 3.14, [`uv`](https://docs.astral.sh/uv/), Git, `gh`, and `actionlint`.
Authenticate `gh` with `gh auth login`, then run:

```sh
uv sync
uv run repomod --help
```

## Catalog and checkouts

`catalog.toml` is the source of truth for managed repositories and policy tags. Its workspace
root defaults to `~/repos`. Set `REPOMOD_ROOT` to use another absolute path:

```sh
REPOMOD_ROOT="$HOME/work/repos" repomod list
```

The catalog's `~/repos` setting is preserved for this checkout. Changing `REPOMOD_ROOT`
changes path resolution for that command and does not move existing checkouts.

Personal repositories owned by `joshuarli` use `$REPOMOD_ROOT/REPO`. Other owners use
`$REPOMOD_ROOT/OWNER/REPO`. `repomod path OWNER/REPO` prints the canonical path. A bare
repository name works only when it is unique. For example, `fd` needs its owner.

`repomod discover` lists active repositories owned by `joshuarli` and `laputa-systems` that
are missing from the catalog. It includes forks. Discovery never edits the catalog or clones
repositories. Add a repository and its primary tag explicitly:

```sh
repomod add OWNER/REPO --tag tool
repomod list
```

`repomod sync` processes catalog entries only. It checks GitHub's current archive and default
branch metadata before cloning. It reuses valid checkouts and fetches the current default
branch unless a clean checkout on that branch already matches GitHub's reported tip. Git
fast-forwards only a clean checkout on the default branch when ancestry proves the move is
safe. Dirty, detached, non-default, ahead, and divergent checkouts keep their checked-out
files and local commits. Fetch may update the remote-tracking ref. A failed repository does
not stop other entries. The command writes progress to stderr and sorted per-repository
results to stdout.

Use `repomod sync --dry-run` to inspect local clone or fetch plans. It makes no network calls.

## Tags and policy

Every repository has at least one **role** tag: `tool`, `library`, `app`, `extension`,
`system`, `build`, `config`, or `data`. Multiple roles are legitimate when a repository
contains independently useful products. Curated **language** tags (`rust`, `typescript`,
`python`, etc.) describe substantial maintained source. Manifest detection also activates
lightweight language checks when a language tag is intentionally absent; a language tag with
no corresponding project manifest is reported. The `crates-io`, `pypi`, and `npm` tags
explicitly opt into package-publishing policy.

Repositories with the standard **binary GitHub prerelease** workflow use exactly `tool`
as their catalog topic set: the workflow's distributed product is an executable, irrespective
of its internal implementation languages. A prerelease that publishes wheels, such as
`pytest-parallel`, is not that binary distribution contract. See [tagging semantics](docs/tagging.md)
for detailed criteria and examples.

Global rules check GitHub Action pins, approved version comments, workflow syntax, repository-local
Markdown links, literal workflow `make` targets, generated artifacts, topics, and basic repository
integrity. Generated-artifact policy combines common ignore patterns with profiles detected from
first-party source. It reports tracked build outputs and missing root `.gitignore` coverage; it does
not edit `.gitignore`. Language checks warn when clear first-party source is missing its
catalog language tag, except for recognized binary-prerelease repositories whose only tag is
`tool`. Primary tags activate semantic template checks. Publishing tags activate their ecosystem
checks. `--tag` filters use union semantics.

```sh
repomod rules
repomod lint --tag tool
repomod lint --repo laputa-systems/hi-lite
repomod lint --rule gha-pin --json
repomod lint --strict
```

Lint reports structured findings with repository, rule, file, severity, and fixability.
Error violations exit with code 1. Warning-only findings exit successfully unless `--strict`
is set. Operational failures, such as unavailable `actionlint` or GitHub access, exit with
code 2. `status --failed` includes CI failures, policy errors, and operational findings;
warning-only findings remain visible in status JSON without being failures. `status` runs only
fast local checks. It does not build projects or contact GitHub to compare topics.

## Git and CI status

`repomod status` shows every catalog entry, its local checkout state, GitHub's actual default
branch, the exact current default-branch commit, CI, and quick local lint count. Local ahead
and behind counts use the last-fetched tracking ref and are labelled as such.

CI uses GitHub GraphQL `Commit.statusCheckRollup` for that exact remote commit. It retains
distinct check-suite and workflow-run identities, considers only the newest attempt of a rerun,
and keeps same-named checks from separate workflows independent. The result
distinguishes passed, failed, pending, cancelled, skipped, neutral, no checks, unknown, and
stale data. Failed checks are never hidden by an older successful run. `--details` prints
failing check names and GitHub URLs.

CI results are cached for five minutes under the user cache directory and keyed by repository
and commit SHA. `--refresh` bypasses the cache. `--offline` makes no network calls. It labels
cached results with their age and source SHA; without a cache it reports unknown.

```sh
repomod status --offline
repomod status --dirty
repomod status --failed
repomod status --repo joshuarli/ish --json
```

## Topics

Catalog tags are the complete desired GitHub topic set. `topics diff` reports additions and
removals. `topics apply` replaces the whole set only when it differs:

```sh
repomod topics diff
repomod topics diff --repo laputa-systems/srv
repomod topics apply --repo laputa-systems/srv
```

Only `topics apply` writes GitHub topics. The catalog entry for `laputa-systems/srv` contains
only `tool`.

## Fix and push

`repomod fix` selects applicable fixers and runs them in deterministic rule order. It requires
a clean checkout on the current GitHub default branch. Each repository gets one disposable
detached Git worktree at its starting commit. A fixer returns an explicit file plan, may change
only its declared paths, runs validation, and must clear its fixable findings.
An explicitly selected repository skipped because it is dirty, detached, or on another branch
returns code 1.

Each successful fixer that changes files gets its own local commit. A compliant rule creates
no commit. A failed fixer loses only its uncommitted changes in the disposable worktree, and
later independent fixers still run. Successful commits fast-forward the canonical checkout
when it remains clean at its starting HEAD. If the checkout changes concurrently, repomod
keeps the commits under a `refs/repomod/recovery/...` ref and reports that ref.
If it cannot update a recovery ref or remove its disposable worktree, it retains and reports
the worktree. Later fixes stop while an unresolved recovery ref or repomod worktree exists;
inspect or recover it before retrying.

```sh
repomod fix --tag tool
repomod fix --repo joshuarli/ish --rule gha-pin
repomod fix --repo joshuarli/ish --push
repomod push --tag library
```

Fixing commits locally by default. `--push` and `repomod push` use ordinary fast-forward Git
pushes and never force-update a remote. A rejected push leaves local commits intact. Pushes
require a clean checkout whose current branch tracks the same `origin` branch.

`gha-pin` and `gha-version` use verified versions and commit SHAs in `actions.toml`. A stale
pin in its approved major channel can be repaired. A workflow that declares a different major
is reported for review and is never rewritten across that boundary. `repomod pins refresh`
resolves newer tags inside each approved major channel. Use `--allow-major` only when you
intend to change the centrally approved channel. Workflow fixes require `actionlint` before
repomod records a successful commit.

## Policy development

Add a normal Python rule in `src/repomod/rules/` and register it in `rules/__init__.py`.
Checks return `Finding` values and do not mutate repositories. A fixer returns a deterministic
`FixPlan`, declares allowed paths, and has a rule-specific commit subject. See
[policy development](docs/policy-development.md) for release contracts and examples.

## Existing generated files

The old `repos.yaml`, `repos-disabled.yaml`, `repos/`, `status.yaml`, `patches-*`, and `logs-*`
data are not read or written by the new commands. repomod leaves any existing copies in place.
