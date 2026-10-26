Prefix: RPMD
Next: 54

### RPMD-53 · P2 · Enforce required check names match produced checks

- **Area:** Release policy
- **Where:** `src/repomod/github.py` (`protection_many`), branch protection on `joshuarli/pytest-parallel`
- **Problem:** pytest-parallel protection required `test (..., 3.13)` and `test (..., 3.14)` while CI produces `test (..., 3.15)`, so auto-merge stalled with green checks the same way the missing vault `CI` job did. The `github-protection` lint only checks the approval count today.
- **Recommendation:** Compare required status check contexts against the check names CI workflows produce, accounting for matrix expansion, and report stale requirements.
- **Fix with:** none
- **Effort:** M (days)

### RPMD-51 · P2 · Converge tool prerelease workflows on the ish format

- **Area:** Release policy
- **Where:** `.github/workflows/release.yml` in `joshuarli/mactop`, `laputa-systems/diff-pretty`, `e`, `fastermod`, `fd`, `fdu`, `fz`, `srv`, `tm`, and `zzz`
- **Problem:** The `binary-release-standard` lint reports the remaining ish format gaps: no verified `SHA256SUMS` packaging, no two-phase GitHub API tag creation with `--verify-tag`, and no preflight default-branch guard. Target matrices are already conforming.
- **Recommendation:** Migrate each workflow to the ish release job flow through reviewed repo fixers or PRs, keeping single-OS scopes for mactop and zzz.
- **Fix with:** none
- **Effort:** M (days)

### RPMD-52 · P2 · Converge Rust builder images on the ish Dockerfile

- **Area:** Release policy
- **Where:** `Dockerfile` in `laputa-systems/diff-pretty`, `fastermod`, `fz`, `srv`, `tm`, and `vault`
- **Problem:** The `rust-musl-image` lint reports unpinned `rustup/dist` installers and, for vault, the missing verified LLVM prebuilt, musl-cargo wrapper, and CRT stubs. Unpinned installers track latest instead of a checksummed archive.
- **Recommendation:** Rebuild each image from the ish Dockerfile contract through reviewed repo fixers or PRs.
- **Fix with:** none
- **Effort:** M (days)

### RPMD-39 · P2 · Limit the Markdown audit to maintained documentation

- **Area:** Lint accuracy
- **Where:** `src/repomod/rules/mdtool.py:23`, `joshuarli/defuddle-rs/tests/upstream/defuddle/expected/`, `joshuarli/gh/internal/attachments/testdata/`, repository `TODO.md` files
- **Problem:** The repo-wide `mdtool check --untracked` call reports 322 findings in extracted Defuddle output snapshots and 90 findings in gh attachment fixtures. It also treats intentionally missing paths quoted in TODO tickets as new untracked-path warnings. These files are not maintained user documentation, so they add noise to the actionable results.
- **Recommendation:** Exclude generated snapshots, fixture data, and TODO tracking files from the repomod mdtool audit while continuing to check maintained documentation.
- **Fix with:** none
- **Effort:** M (days)

### RPMD-40 · P2 · Preserve Rustdoc links in Markdown lint results

- **Area:** Lint accuracy
- **Where:** `src/repomod/rules/mdtool.py:23`, `laputa-systems/rawzip/docs/01-reading.md:5`
- **Problem:** `mdtool` treats Rustdoc intra-doc links such as `[crate::ziparchive]` as undefined Markdown reference links. Rawzip's technical guides contain many valid Rustdoc links reported as errors.
- **Recommendation:** Teach the Markdown audit to recognize Rustdoc intra-doc link syntax or suppress only that supported link class.
- **Fix with:** none
- **Effort:** M (days)

### RPMD-42 · P2 · Check Rust lockfiles at executable workspace roots

- **Area:** Lint accuracy
- **Where:** `src/repomod/rules/languages.py:204`, `src/repomod/rules/languages.py:249`, `laputa-systems/rawzip/compare/Cargo.toml`, `laputa-systems/rawzip/compare/src/main.rs`
- **Problem:** Rawzip's library root has no executable, but its nested `compare` workspace has `src/main.rs`. The Rust lint sees that nested binary and asks for a repository-root `Cargo.lock`, even though the lockfile belongs at the nested workspace root.
- **Recommendation:** Associate each executable with its owning Cargo workspace and check for a lockfile there; do not attribute nested development executables to the library root.
- **Fix with:** none
- **Effort:** M (days)

### RPMD-44 · P2 · Align language detection with managed project ecosystems

- **Area:** Catalog policy
- **Where:** `src/repomod/rules/languages.py:509`, `catalog.toml`, `joshuarli/shadowdriver/ts/src/`, `joshuarli/gh/go.mod`, `laputa-systems/jaq/Cargo.toml`
- **Problem:** The detector searches TypeScript under `src`, `web`, `app`, `packages`, and `extensions`, but misses the tracked `shadowdriver/ts/src/*.ts` project. It also treats helper scripts in otherwise Rust repositories as project languages, while primary Go and Rust projects such as gh, ish, and jaq lack corresponding catalog tags.
- **Recommendation:** Base language tags on managed project manifests and first-party source roots, include the `ts/` layout, and reconcile primary language tags in the catalog.
- **Fix with:** none
- **Effort:** M (days)

### RPMD-45 · P3 · Blacklist intentional README omissions per repository

- **Area:** Catalog policy
- **Where:** `catalog.toml` (`joshuarli.gh`), `joshuarli/gh/AGENTS.md`
- **Problem:** The repo-template lint requires a human-written README for gh even though its repository instructions explicitly say the upstream README and documentation are intentionally removed from this fork.
- **Recommendation:** Add a repository-specific `repo-template` lint blacklist entry for gh.
- **Fix with:** none
- **Effort:** S (hours)

### RPMD-47 · P2 · Reconcile GitHub topics with the curated catalog

- **Area:** Repository metadata
- **Where:** `catalog.toml` and the GitHub repository topics for all 27 catalog repositories
- **Problem:** The read-only scan found catalog topic differences on all 27 repositories, including topics to add and a few existing topics to remove. The repositories therefore present different classifications locally and on GitHub.
- **Recommendation:** Review the proposed topic changes and run `repomod topics apply` for the catalog once approved.
- **Fix with:** none
- **Effort:** S (hours)

### RPMD-48 · P2 · Autofix required workflow concurrency and shell defaults

- **Area:** GitHub Actions
- **Where:** `.github/workflows/*.yml` and `.yaml` in catalog repositories with `gha-settings` findings
- **Problem:** The new lint identifies workflows that do not use the required concurrency group, cancel setting, or `bash -euo pipefail {0}` shell default. The linter reports these, but the fixer has no validated file plan for them, leaving repeated policy edits manual across repositories.
- **Recommendation:** Add a workflow fixer that plans these exact root workflow edits, validates the resulting workflow, then apply it through the existing per-repository PR flow.
- **Fix with:** none
- **Effort:** M (days)

### RPMD-50 · P2 · Recognize manifest readme declarations in generated-data audit

- **Area:** Lint accuracy
- **Where:** `src/repomod/rules/mdtool.py`, package-manifest readme lines (e.g. `joshuarli/pytest-parallel/pyproject.toml:5`, `laputa-systems/rg/Cargo.toml:114`), template sources (e.g. `joshuarli/discover/AGENTS.md.in:1`)
- **Problem:** The Markdown audit reports C003 "literal consumer may affect generated data" for 85 locations where the consumer only declares the package readme (`readme = "README.md"`) or is a template source (`.in`) that generates the target. Readme declarations point at hand-maintained sources, and template outputs are reviewed through the template, so neither is an unreviewed generated-data hazard.
- **Recommendation:** Skip C003 when the consuming line is a package-manifest readme declaration or the consumer is a template source that generates the target.
- **Fix with:** none
- **Effort:** S (hours)
