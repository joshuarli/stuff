Prefix: RPMD
Next: 72

### RPMD-53 · P2 · Enforce required check names match produced checks

- **Area:** Release policy
- **Where:** `src/repomod/github.py` (`protection_many`), branch protection on `joshuarli/pytest-parallel`
- **Problem:** pytest-parallel protection required `test (..., 3.13)` and `test (..., 3.14)` while CI produces `test (..., 3.15)`, so auto-merge stalled with green checks the same way the missing vault `CI` job did. The `github-protection` lint only checks the approval count today.
- **Recommendation:** Compare required status check contexts against the check names CI workflows produce, accounting for matrix expansion, and report stale requirements.
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

### RPMD-68 · P2 · Fix the shadowdriver-py build

- **Area:** Build
- **Where:** `joshuarli/shadowdriver/py/`, `Pool` in the shadowdriver crate
- **Problem:** `cargo test -p shadowdriver-py` fails on `origin/master` with two E0599 errors: `Pool` has no `open_interactive_tab` or `cookies_json`. The Python binding calls methods the Rust crate no longer has, so `make rust-test` cannot finish and the Python package cannot be built.
- **Recommendation:** Restore the two methods on `Pool` or update the binding to the current API, and keep `make rust-test` in the repository's verification.
- **Fix with:** root-cause
- **Effort:** S (hours)

### RPMD-71 · P3 · Check smolith-qualify for parallel test flakes

- **Area:** Tests
- **Where:** `laputa-systems/smolith/tests/qualify`
- **Problem:** `unproven`: in two parallel runs of `smolith-qualify`, a different fd-reuse test failed each time; it passed 22 of 22 twice with `--test-threads=1`. Not yet confirmed against the commit before the example and bench clean-up, so it may predate it.
- **Recommendation:** Reproduce on the earlier commit with parallel threads; if it reproduces, isolate the shared file descriptors per test rather than serializing the suite.
- **Fix with:** root-cause
- **Effort:** S (hours)

### RPMD-63 · P3 · Move owned repositories from serde to deser

- **Area:** Dependencies
- **Where:** `Cargo.toml` in `joshuarli/defuddle-rs`, `joshuarli/ish`, `joshuarli/kache`, `joshuarli/obsidian-ferrisearch`, `joshuarli/shadowdriver`, `joshuarli/soulseek-rs`, `joshuarli/tansu`, `laputa-systems/h12tiny`, `laputa-systems/python-build`, `laputa-systems/rustybench`
- **Problem:** deser is the preferred serialization stack, but these ten owned repositories still declare serde (fork repositories jaq, oxish, rg, rustls, and webpki also do). serde brings `serde_derive`, `syn`, and `proc-macro2` with it.
- **Recommendation:** Migrate one repository at a time, starting with those where serde is only JSON (`deser-json`), and confirm on the Bloat page that the derive stack leaves the lockfile.
- **Fix with:** none
- **Effort:** L (weeks)

### RPMD-70 · P3 · Prefer the Python standard library over third-party packages

- **Area:** Dependencies
- **Where:** `dependency-roles.toml`, `src/repomod/dependency_roles.py`, pyproject.toml in `joshuarli/toolbox`, `joshuarli/agent-memory`, `joshuarli/pytest-parallel`
- **Problem:** Python repositories pull in third-party packages for jobs the standard library covers: HTTP (`httpx2`, `urllib3`, `truststore`), and possibly `idna` and `websockets`. The preference is to stay within the standard library, but roles can only name packages, so "use the standard library" cannot be preferred or flagged.
- **Recommendation:** Let a role option stand for the standard library (an option with no packages, such as `stdlib = []`), add a Python HTTP role preferring it, then move each repository to `urllib.request` and `ssl` where it does not need HTTP/2 or connection pooling, and record exceptions with reasons where it does.
- **Fix with:** none
- **Effort:** M (days)
