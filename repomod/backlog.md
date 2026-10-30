# Backlog: status to zero

Goal: `uv run repomod status` reports 0 lints. Only lints count; fork drift, TODO headings, and
CI failures that are not lint findings are reported, not zeroed.

Landing rule: each repo is fixed in a worktree under its own `.work/wt/`, committed on a branch,
and fast-forwarded into the clean canonical default branch. Nothing is pushed.

## Decisions

- Drift, TODOs, and non-lint CI failures do not count toward zero.
- mdtool C001-C003 are consumer advisories (code reads or embeds a doc); repomod stops reporting them.

## Lane 1: repo fixes

Baseline lint counts from the first run. Mark `[x]` when the repo shows 0 lints.

- [x] joshuarli/shadowdriver (103 -> 0, 5 local commits)
- [ ] laputa-systems/python-build (106 -> 55): see Open decisions
- [x] joshuarli/discover (51 -> 0, 2 local commits)
- [x] laputa-systems/node-build (32 -> 0; binary-release-tags ignored via override)
- [x] joshuarli/obsidian-ferrisearch (22 -> 0; binary-release-tags ignored via override)
- [x] joshuarli/toolbox (16 -> 0, on fix branch)
- [ ] laputa-systems/chromium-build (16 -> 6): R005 x6 are legit external paths; see Open decisions
- [x] laputa-systems/h12tiny (11 -> 0)
- [ ] joshuarli/tansu (9 -> 3): typescript-stack Solid rules; see Open decisions
- [x] laputa-systems/zzz (8 -> 0; release asset names changed)
- [x] joshuarli/ish (6 -> 0)
- [x] joshuarli/pytest-parallel (5 -> 0)
- [x] laputa-systems/deser (5 -> 0)
- [x] laputa-systems/git-build (4 -> 0)
- [x] joshuarli/mactop (3 -> 0)
- [x] laputa-systems/fz (3 -> 0)
- [x] laputa-systems/jaq (3 -> 0)
- [ ] laputa-systems/ptytest (3 -> 1): R003 quotes another repo's cargo package
- [x] laputa-systems/fd (2 -> 0)
- [x] laputa-systems/rust-brotli (2 -> 0)
- [x] laputa-systems/srv (2 -> 0)
- [x] laputa-systems/tm (2 -> 0; checkout still has the owner's uncommitted changes)
- [x] laputa-systems/vault (2 -> 0)
- [x] joshuarli/defuddle-rs (1 -> 0)
- [ ] laputa-systems/bbtree (1): rust-toolchain pin conflicts with Verus nightly; owner decision
- [x] laputa-systems/fdu (1 -> 0)
- [ ] laputa-systems/ntpd-rs (1): rust-toolchain.toml symlink to stable channel; owner decision
- [x] laputa-systems/rustybench (1 -> 0)

## Open decisions

- Per-code mdtool ignore (`mdtool:R005`) in catalog overrides: needed for chromium-build R005 x6 and
  python-build D001/P002/R005/R001 (vendored and mirrored docs).
- tansu: narrow `_is_frontend` to framework dependencies, or ignore `typescript-stack`.
- python-build: approve `cachix/install-nix-action` in `actions.toml`; AGENTS.md T002 needs owner restructuring.
- ntpd-rs, bbtree: rust-toolchain rule vs project nightly/stable pins.
- ptytest R003: command quotes another repo's package.

## Lane 2: repomod noise

- [x] Drop mdtool C001, C002, C003 in `rules/mdtool.py` (advisory: code reads or embeds a doc)

## Lane 3: reported, not zeroed

- Fork drift: gh, bbtree, deser, fd, rawzip, rg, rust-brotli
- TODO headings: tm 4, h12tiny 5, fastermod 2, ish 2, gh, obsidian-ferrisearch, tansu, ntpd-rs,
  rawzip, rg, rustybench 1 each
- Non-lint CI failures: discover, ish, tansu, mactop, h12tiny, node-build, toolbox
