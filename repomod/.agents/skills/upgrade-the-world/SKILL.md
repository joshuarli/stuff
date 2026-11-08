---
name: upgrade-the-world
description: "Use for /upgrade-the-world: upgrade every public, non-fork catalogued repository to the latest versions of the dependencies and tool pins it already uses, loop until CI is green, and file hard blockers as deferred TODO.md tickets."
disable-model-invocation: true
---

# Upgrade the world

Invoking this skill approves the whole run. Open pull requests, merge green ones, refresh pins, and publish mirror updates without asking. Stop before starting only if the repomod working tree has uncommitted changes, because `fix` runs the working tree's rules against the live catalog.

## Scope

Upgrade what each repository already depends on:

- Direct dependencies, including major versions, and the lockfiles behind them.
- Known vulnerabilities that an upgrade of an existing dependency clears.
- Approved pins in `actions.toml`, `python-tools.toml`, `typescript-tools.toml`, and `musl-toolchain.toml`.
- Red CI on the default branch, whatever caused it.

Leave these alone, and do not file tickets for them:

- Private repositories.
- Dependency upgrades in repositories tagged `fork`, which keep upstream's choices. Repair red CI in a fork only when the fix stays out of upstream's code.
- Porting. Do not replace a dependency, add one, or act on `audit-banned`, `audit-registry`, or the dashboard's Choices page.
- The pinned Rust nightly. Do not run the `rust-toolchain` fixer.
- Catalog tags, `ignore` entries, and `dependency-roles.toml`.

## Use judgment in each repository

Repositories differ, and "latest" is the goal rather than a command. Read the repository's `AGENTS.md` and the code around a dependency before moving it.

Skip an upgrade when the repository has a real reason to stay put. Examples are a commented pin, an MSRV or platform floor, a version a sibling repository must match, or a major release that drops something the repository relies on. Name the reason in the report. A skip needs no ticket and is not retried in this run.

## Run the loop

Repeat these steps until the exit condition holds. Run them from the repomod checkout.

### 1. Survey

```sh
mkdir -p .work/tmp
uv run repomod sync
uv run repomod status --refresh --json > .work/tmp/status.json
jaq --from toml -r '.repos | to_entries[] | .key as $owner | .value | to_entries[] | select(.value | index("fork")) | "\($owner)/\(.key)"' catalog.toml | sort > .work/tmp/forks.txt
jaq -r '.[].repository' .work/tmp/status.json | sort | comm -23 - .work/tmp/forks.txt > .work/tmp/lanes.txt
uv run repomod lint --rule audit-upgrades --rule audit-vuln --json $(sed 's/^/--repo /' .work/tmp/lanes.txt) > .work/tmp/upgrades.json
```

`.work/tmp/lanes.txt` is the upgrade set. `fix` and `merge` include private repositories and forks unless you pass `--repo`, so always pass this list. Read each lane's open pull requests with `gh pr list` and its `TODO.md`. Do not rework an item that already has a ticket from this run.

### 2. Refresh pins

Skip this step after the first round.

1. Run `uv run repomod pins refresh`.
2. If a pinned version is missing from a registry that repositories use, publish it with the `stuff-registries` skill and verify the index carries it.
3. If a pin file changed, run `make test`, then commit and push the change in repomod before running `fix`.

### 3. Run the fixers

```sh
uv run repomod fix --rule audit-upgrades --rule python-standard --rule typescript-stack --rule linux-musl-toolchain --rule gha-syntax $(sed 's/^/--repo /' .work/tmp/lanes.txt)
```

`fix` opens one pull request per repository and enables auto-merge where the repository allows it. Treat a `failed` or `skipped` line as work for a lane, and do not rerun `fix` blindly.

### 4. Send a lane to each remaining repository

Start one subagent per repository that has manual upgrades, a failed fixer, or red CI, and route it through `orchestrate`. Do not batch repositories. Give each subagent a self-contained brief with these rules:

- Name the repository path, default branch, target packages and versions, and failing checks with log excerpts.
- Apply "Scope" and "Use judgment in each repository" from this skill, copied into the brief.
- Read the release notes of every major version crossed. Fix breakage at its cause.
- Work in a detached worktree under the repository's `.work/wt/`, on a new `upgrade-the-world/<short-name>` branch from the default tip. Add `.work/` to `.git/info/exclude` first.
- Never reset, stash, clean, switch, or discard changes in the canonical checkout. A dirty checkout does not block a worktree.
- Never use a `repomod/fix/*` branch.
- Do not weaken or delete a test unless the test is wrong, and say so when it is.
- Verify with the commands CI runs, then push and open or update a pull request with `gh`.
- Do not write `TODO.md`. Report each blocker to the parent with its evidence.
- Stop after three rounds without progress, and report what was tried.

If the same workflow failure appears in several lanes, run the matching `fix --rule` once. If the cure needs a repomod rule change, file a ticket in repomod's `TODO.md` and do not change repomod during the run.

### 5. Merge and wait

1. Wait on checks with `gh pr checks <number> --watch` in the background.
2. Rerun a failed job once with `gh run rerun --failed`. A second identical failure is a real failure, so send it back to the lane.
3. Merge green lane pull requests with `gh pr merge --auto --rebase`. If the repository refuses auto-merge, wait for green and run `gh pr merge --rebase`.
4. Never merge a red pull request. Do not use `repomod merge`, which does not check CI itself.
5. Wait for default-branch CI after each merge, because a pull request can pass and the push build can still fail.

### Exit condition

Stop looping when all of these hold for every repository in `.work/tmp/lanes.txt`:

- `status --refresh --json` shows no failed or pending CI on the default branch.
- No pull request from this run is open.
- Every remaining `audit-upgrades` and `audit-vuln` finding is either a reasoned skip or a ticket.

An item that makes no progress for three rounds is a hard blocker. Every round must merge something, clear a finding, or file a ticket. If a round does none of these, stop looping.

## File hard blockers as deferred tickets

A hard blocker is in-scope work that cannot finish in this run:

- The target version is unpublished, yanked, or broken upstream.
- A vulnerability has no clean upgrade, so the fix needs an override or an owner decision.
- The work needs credentials, repository settings, or branch protection changes that are out of reach.
- The red check is a product bug that needs a design decision.
- Three rounds passed without progress.

Only the parent files tickets, after the lanes report. Follow the `todo` skill for IDs, priorities, and format, in the affected repository's root `TODO.md`. Extend a matching ticket instead of adding another. Name the blocked package and version, the evidence, and the condition that unblocks the work. Set `Fix with` to `upgrade-the-world`.

Commit tickets on an `upgrade-the-world/todos` branch in a worktree, open a pull request, and merge it when green. At the start of a new run, retry each open `upgrade-the-world` ticket once. Delete a ticket when its blocker clears, and end that commit's body with `Closes ABC-123`.

## Finish

1. Run `uv run repomod deps snapshot`, then commit and push the inventory in repomod.
2. Run `uv run repomod deps enrich --refresh` so `make deps` shows the result.
3. Report per repository: merged, skipped with the reason, and tickets filed. List anything published to a registry. List what is still outdated and why.
