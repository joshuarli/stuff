Prefix: RPMD
Next: 38

### RPMD-37 · P3 · Detect language tags without matching source

- **Area:** Policy
- **Where:** `src/repomod/rules/languages.py:521`, `src/repomod/rules/languages.py:596`, `tests/test_consistency.py:69`
- **Problem:** `_language_tag_findings` reports detected languages missing from catalog tags, but never reports language tags without matching first-party source. For example, a `typescript` tag with a `package.json` and no TypeScript source is accepted. The test suite covers the missing-tag direction only.
- **Recommendation:** Compare detected source languages with catalog language tags in both directions, retaining the binary-prerelease exception.
- **Fix with:** none
- **Effort:** S
