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

- [ ] joshuarli/shadowdriver (103): gitignore `grab/node_modules/`, then triage the rest
- [ ] laputa-systems/python-build (106)
- [ ] joshuarli/discover (51)
- [ ] laputa-systems/node-build (32)
- [ ] joshuarli/obsidian-ferrisearch (22)
- [ ] joshuarli/toolbox (16): on a `repomod/fix/*` branch; resolve first
- [ ] laputa-systems/chromium-build (16)
- [ ] laputa-systems/h12tiny (11)
- [ ] joshuarli/tansu (9)
- [ ] laputa-systems/zzz (8)
- [ ] joshuarli/ish (6)
- [ ] joshuarli/pytest-parallel (5)
- [ ] laputa-systems/deser (5)
- [ ] laputa-systems/git-build (4)
- [ ] joshuarli/mactop (3)
- [ ] laputa-systems/fz (3)
- [ ] laputa-systems/jaq (3)
- [ ] laputa-systems/ptytest (3)
- [ ] laputa-systems/fd (2)
- [ ] laputa-systems/rust-brotli (2)
- [ ] laputa-systems/srv (2)
- [ ] laputa-systems/tm (2): dirty checkout, leave until clean
- [ ] laputa-systems/vault (2)
- [ ] joshuarli/defuddle-rs (1)
- [ ] laputa-systems/bbtree (1)
- [ ] laputa-systems/fdu (1)
- [ ] laputa-systems/ntpd-rs (1)
- [ ] laputa-systems/rustybench (1)

## Lane 2: repomod noise

- [x] Drop mdtool C001, C002, C003 in `rules/mdtool.py` (advisory: code reads or embeds a doc)

## Lane 3: reported, not zeroed

- Fork drift: gh, bbtree, deser, fd, rawzip, rg, rust-brotli
- TODO headings: tm 4, h12tiny 5, fastermod 2, ish 2, gh, obsidian-ferrisearch, tansu, ntpd-rs,
  rawzip, rg, rustybench 1 each
- Non-lint CI failures: discover, ish, tansu, mactop, h12tiny, node-build, toolbox
