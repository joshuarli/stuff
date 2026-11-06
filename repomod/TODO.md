Prefix: RPMD
Next: 74

### RPMD-73 · P2 · Repair branch protections that require checks no workflow produces

- **Area:** Release policy
- **Where:** branch protection on `laputa-systems/diff-pretty` (`test (macos-26)`), `laputa-systems/fdu` (`Build and test (macos-26)`), `laputa-systems/fz` (`CI (macos-26)`), `laputa-systems/tm` (`CI`)
- **Problem:** `github-protection` reports required status checks that no workflow produces, so pull requests on these repositories wait forever for green checks and auto-merge stalls. The macos-26 contexts look like a runner rename to macos-latest; tm's job is now named `CI (${{ matrix.os }})`.
- **Recommendation:** Point each protection at the check names the workflows produce now; repomod has no command that writes branch protection, so do it in GitHub settings or with `gh api`, then confirm the lint is clean.
- **Fix with:** none
- **Effort:** S (hours)

### RPMD-63 · P3 · Finish moving owned repositories from serde to deser

- **Area:** Dependencies
- **Where:** `laputa-systems/python-build/rust-cpython/overlay/Modules/_pickle_rs/src/lib.rs`; `miniserde` in `laputa-systems/rustybench`, `laputa-systems/h12tiny` (`json` and `query` features), `joshuarli/defuddle-rs`, `joshuarli/obsidian-ferrisearch`
- **Problem:** ish, tansu, shadowdriver, kache, and h12tiny's benchmark app are on deser, and soulseek-rs is an accepted exception. `_pickle_rs` still uses `serde` and `serde-pickle` because `deser-pickle` 0.10.2 cannot emit the module's current bytes: it writes small ints as `K`/`M` instead of `J`, sizes `LONG1` minimally instead of always 8 bytes, and encodes empty lists and dicts as `](e` and `}(u`. Its output is valid pickle and closer to CPython's own, and a deser-based loader decodes every valid pickle identically. Four repositories use `miniserde`, which is not serde but is also not the preferred stack; in defuddle-rs and obsidian-ferrisearch serde stays in the lockfile through html5ever and tantivy regardless, and ferrisearch moved to miniserde on purpose to slim its wasm build.
- **Recommendation:** Decide whether `_pickle_rs` must keep its exact bytes. If CPython-equivalent loading is enough, port both directions to `deser-pickle` and run CPython's `test_pickle`; otherwise leave it until `deser-pickle` has a compatible mode. Decide separately whether `miniserde` should move to deser, starting with rustybench behind report and baseline round-trip tests, and measure wasm size before touching ferrisearch.
- **Fix with:** none
- **Effort:** M (days)
