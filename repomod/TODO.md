Prefix: RPMD
Next: 72

### RPMD-68 · P2 · Fix the shadowdriver-py build

- **Area:** Build
- **Where:** `joshuarli/shadowdriver/py/`, `Pool` in the shadowdriver crate
- **Problem:** `cargo test -p shadowdriver-py` fails on `origin/master` with two E0599 errors: `Pool` has no `open_interactive_tab` or `cookies_json`. The Python binding calls methods the Rust crate no longer has, so `make rust-test` cannot finish and the Python package cannot be built.
- **Recommendation:** Restore the two methods on `Pool` or update the binding to the current API, and keep `make rust-test` in the repository's verification.
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
