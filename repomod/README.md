# repomod

repomod applies one fix across many GitHub repos and opens a draft PR in each repo the fix
changes. It targets every non-archived repo owned by `joshuarli` and `laputa-systems`, forks
included, that has a non-bot commit on the default branch within the last year.

## Install

You need Python 3.14, [`uv`](https://docs.astral.sh/uv/),
[`actionlint`](https://github.com/rhysd/actionlint), and the GitHub CLI `gh`, logged in with
`gh auth login`.

```
uv sync
```

Run commands with `uv run repomod`, or allow `.envrc` with direnv to put `repomod` on your
`PATH`.

## Run a fix

A fix is a directory under `src/repomod/fixes/`. Pass its name, or a path to a fix directory.
Without `--apply`, every command is a dry run that pushes nothing.

```
repomod sync                       # refresh repos.yaml only, run no fix
repomod pin-gha --plan             # list repos that would run, without cloning
repomod pin-gha                    # dry run: clone, run the fix, print colored diffs
repomod pin-gha --repos a/x,a/y    # only these repos
repomod pin-gha --limit 5          # only the first 5 repos
repomod pin-gha --apply            # push branches and open draft PRs
repomod pin-gha --status           # summarize status.yaml
repomod pin-gha --prs              # merged, open, and closed PRs on the fix branch
repomod pin-gha --diffs            # print the stored dry-run diff of every pending repo
repomod pin-gha --undraft          # list the fix's draft PRs. Add --apply to mark them ready.
repomod pin-gha --refresh          # re-sync repos.yaml even if the cache is fresh
repomod pin-gha --workers 1        # run one repo at a time, for debugging
```

repomod reuses `repos.yaml` when it is less than 24 hours old.

Go in stages: `--plan`, then `--limit 5`, then a full dry run, then `--apply`. Every PR opens
as a draft. Run `--undraft --apply` after you review the rollout.

Press `^C` to stop a run. Repos in progress are marked `interrupted`, and the next run picks
up where this one stopped. Press `^C` again to terminate immediately.

## Output

repomod writes these files next to each fix:

- `status.yaml`: each repo's status, the fix hash it ran with, the base commit `base_sha`,
  the PR URL, and `pushed_sha` (the commit last force-pushed, only after a push).
- `patches-{hash}/`: each repo's proposed diff.
- `logs-{hash}/`: details for repos with status `failed`.
- `repos/`: the clones.

At the repo root, `repomod sync` writes `repos.yaml` (the target repos). Don't edit it by
hand.

`repos-disabled.yaml` is the list of repos you ignore for every fix. `sync` leaves a
listed repo out of `repos.yaml`. A run also skips that repo when an older `repos.yaml` still
names it. Edit `repos-disabled.yaml` by hand.

## Re-running a fix

Each status entry records the fix hash: SHA-256 of `main.py`, of `config.yaml` without its
`skip` list, and of every module that file imports from the fix directory or the `repomod`
package, including what those modules import. The standard library and third-party packages
are not part of the hash. YAML comments and formatting don't affect it. When you edit a fix
or one of those modules, the hash changes and every repo runs again. Editing `skip` or
comments keeps saved dry-run results. A hash recorded before imported modules were included
is not current, so those repos run once more.

With a matching hash, repos with status `pr_created`, `pr_merged`, `pr_closed`, or `skipped`
are not run again. A dry run also skips `pending` repos. Repos with status `failed` or
`interrupted` always run again.

`--apply` pushes the stored diff from `patches-{hash}/` when HEAD is still `base_sha`.
If HEAD moved, or the diff is missing or does not apply, repomod re-runs the fix.
A push to an open PR is a force-push: commits added on that branch after the PR opened
are replaced. `pushed_sha` is the branch tip that push sent. repomod never pushes when
that PR is merged or closed, and then it does not write `pushed_sha`. Branch names are
compared exactly (`Main` is not `main`). To roll out again, change `branch` in
`config.yaml`. Older rows without `pushed_sha` still load.

## Write a fix

Create `src/repomod/fixes/<name>/` with `config.yaml` and `main.py`. repomod handles
cloning, diffing, committing, pushing, and PRs. Model a new fix on
`src/repomod/fixes/macos-15/` (a regex rewrite) or `src/repomod/fixes/pin-gha/` (uses the
network).

### `config.yaml`

| Key              | Required | Meaning                                                                                                    |
| ---------------- | -------- | ---------------------------------------------------------------------------------------------------------- |
| `branch`         | yes      | Branch pushed to each repo.                                                                                |
| `pr_title`       | yes      | PR title and commit message.                                                                               |
| `pr_body`        | yes      | PR body. repomod appends a "Generated by repomod" footer, so don't add your own.                           |
| `reviewers`      | no       | Reviewers requested on the PR.                                                                            |
| `sparse_paths`   | no       | Check out only these paths, for example `[.github]`.                                                       |
| `skip`           | no       | Repos to skip. Give each entry a YAML comment that says why.                                               |

Other keys are passed through to your fix in `config`.

### `main.py`

```python
def accept_repo(repo: dict, config: dict) -> bool:
    """repo keys: name, default_branch, topics. config: parsed config.yaml."""


def run(repo: Path, config: dict) -> bool:
    """Modify files in the cloned repo. Return True if changes were made."""
```

`run` must be idempotent: a second run changes nothing.

You can also define `validate(repo: Path, config: dict) -> list[str]`. It runs after `run`
makes changes. Any returned error fails the repo with status `failed` and details in
`logs-{hash}/`, and repomod neither shows nor pushes the diff.

repomod always runs `actionlint` on changed workflow files. Only errors the fix introduced fail
the repo.
