# Handoff — fleet release standardization + all-green round (2026-10-25)

## Landed in repomod
- `21d86c5` binary release standard: musl-only 3-target matrix, ish prerelease format
  (`binary-release-standard`), `rust-musl-image` Dockerfile checks, platform tags
  (`mactop=[tool,macos,swift]`, `zzz=[tool,linux]`, `fd=[tool]`).
- `0c87a22` + `700b236` (revert): all workflows use generic concurrency; new
  `github-protection` lint (0 approvals required). Carve-out was tried and reverted
  per owner decision.
- `cbb1907` `gha-triggers` rule+fixer: bare `push:` + `pull_request:` gets scoped to
  the git-resolved default branch. Fleet clean except where noted.
- `a8a132d` `status` shows open PR merge state; `--failed` includes conflicts.
- `28026f1` new `.agents/skills/all-green/` skill (survey → confirm → fan out).
- RPMD-43/49 closed; RPMD-51/52/53 open (format/Dockerfile/check-name convergence).

## Fleet state: all fix PRs merged except red-product ones below
- Merged green: e, fdu, h12tiny, ish, pypi, vault, zzz#7, pytest-parallel, toolbox#3,
  fd, srv, tm, diff-pretty, and the settings/pins wave.
- toolbox#3 needed a manual rebase (setup-uv pin conflict); fdu trigger fix went
  direct to main (checkout had been left on the merged fix branch; moved back).

## Owner actions needed (nothing code-side unblocks these)
1. **GitHub billing failing** — jobs won't start on discover, shadowdriver, toolbox-main.
   Settings → Billing & plans, then rerun. Shadowdriver's pnpm-cache fix is pushed
   but unproven until then.
2. **tansu CI** clones private defuddle-rs → access denied. Needs a token/visibility call.
3. **First real zzz release** must be dispatched manually (workflow fixed, never published).
4. **Unapproved pins**: `setup-rust` (fd), pnpm actions (discover/shadowdriver) need
   trust calls before entering `actions.toml`.
5. e/toolbox/pypi/h12tiny/fdu/ish product-test fixes all landed via lanes; details in
   the session (no separate tickets filed).

## Resume pointers
- `uv run repomod sync && uv run repomod status --failed` for fresh state.
- Private free-tier repos cannot auto-merge; merge green PRs by hand.
- `fdu/.work/` has leftover lane scratch (untracked); stale local `repomod/fix/*`
  branches exist in several checkouts.
