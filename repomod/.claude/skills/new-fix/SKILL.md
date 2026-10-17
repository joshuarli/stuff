---
name: new-fix
description: Use when asked to add a new repomod / fix that mass-applies a change across repos (e.g. "upgrade X to Y in all CI", "add a new repomod for ..."). Covers scaffolding, testing, and dry-running a fix in this repo.
---

# Adding a new fix

Read AGENTS.md first for conventions. [Write a fix](../../../README.md#write-a-fix) in the
README documents the `config.yaml` keys and the `main.py` interface.

## 1. Scaffold `src/repomod/fixes/<name>/`

Model the fix on `src/repomod/fixes/macos-15/` (simple regex rewrite) or
`src/repomod/fixes/pin-gha/` (needs network).

- Set `sparse_paths`, for example `[.github]`, so clones stay small.
- List only extra people in `reviewers`. Don't invent reviewers or copy another fix's `skip`
  list. Ask if unsure.
- Make rewrites precise (regex with boundaries) and test the near-misses.
- Add `validate` when output can break in a way actionlint doesn't catch, for example a YAML
  file that no longer parses.

## 2. Test it

Add `tests/test_fix_<name_with_underscores>.py`. The directory name has a hyphen, so load
the module with `importlib.util.spec_from_file_location` like `tests/test_fix_macos_15.py`.
Cover the change, a no-op when already applied, near-miss strings left alone, and missing
directories. Use realistic names (`acme/backend`). Tests are offline.

Then run `make test`, `uv run ruff check src tests`, and `uv run ty check`.

## 3. Dry-run

A dry run pushes nothing, but it clones real repos and writes `status.yaml` and `logs-*`, so
go in stages:

1. `uv run repomod <name> --plan`
2. `uv run repomod <name> --limit 5`, then inspect the diffs
3. `uv run repomod <name>` for the full run, then `uv run repomod <name> --status`

Repos that fail actionlint validation show up as `failed` with the findings in
`logs-{hash}/`. Treat those as fix bugs, not noise.

Editing `main.py` or the config (other than `skip` and comments) after a dry run changes the
fix hash, and every repo runs again.

## 4. Never without explicit approval

`--apply` pushes branches and opens PRs on many repos. Only run it when the user
explicitly asks for it for this fix, and never as part of "add a fix" or "dry run".
Report dry-run results (counts per status, notable diffs, failures) and stop there.
