Prefix: RPMD
Next: 74

### RPMD-63 · P3 · Finish moving owned repositories from serde to deser

- **Area:** Dependencies
- **Where:** `laputa-systems/python-build/rust-cpython/overlay/Modules/_pickle_rs/src/lib.rs`; `miniserde` in `laputa-systems/rustybench`, `laputa-systems/h12tiny` (`json` and `query` features), `joshuarli/defuddle-rs`, `joshuarli/obsidian-ferrisearch`
- **Problem:** ish, tansu, shadowdriver, kache, and h12tiny's benchmark app are on deser, and soulseek-rs is an accepted exception. `_pickle_rs` still uses `serde` and `serde-pickle` because `deser-pickle` 0.10.2 cannot emit the module's current bytes: it writes small ints as `K`/`M` instead of `J`, sizes `LONG1` minimally instead of always 8 bytes, and encodes empty lists and dicts as `](e` and `}(u`. Its output is valid pickle and closer to CPython's own, and a deser-based loader decodes every valid pickle identically. Four repositories use `miniserde`, which is not serde but is also not the preferred stack; in defuddle-rs and obsidian-ferrisearch serde stays in the lockfile through html5ever and tantivy regardless, and ferrisearch moved to miniserde on purpose to slim its wasm build.
- **Recommendation:** Decide whether `_pickle_rs` must keep its exact bytes. If CPython-equivalent loading is enough, port both directions to `deser-pickle` and run CPython's `test_pickle`; otherwise leave it until `deser-pickle` has a compatible mode. Decide separately whether `miniserde` should move to deser, starting with rustybench behind report and baseline round-trip tests, and measure wasm size before touching ferrisearch.
- **Fix with:** none
- **Effort:** M (days)
