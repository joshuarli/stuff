Prefix: STUF
Next: 27

### STUF-25 · P3 · stuff accepts flags no command reads and advertises positionals it rejects

- **Area:** Types
- **Where:** `crates/stuff/src/cli.rs:102,202-206` (`--remote` parsed and never read), `crates/stuff/src/main.rs:257-286` (`render` ignores `--json`), `crates/stuff/src/cli.rs:219,222-223` (help promises `[sources...]`), `:175-177` (`diff` accepts and ignores the base flags).
- **Problem:** The flat `Parsed` struct is handed to every command and each arm hand-rejects a subset of flags. `doctor --remote` is a no-op that the README and `deploy/deployed-smoke.sh` present as meaningful; `render --output x.html --json` writes HTML and exits 0; `diff` accepts base-URL flags it never uses. The help advertises `search ... [sources...]` and `render ... [sources...]` even though the parser rejects every positional except the single search query. Callers cannot tell from a signature which flags a command honors.
- **Recommendation:** Give each command its own argument type so unsupported flags cannot be parsed, or add the missing rejections per arm. Remove `--remote` or make it select remote mode explicitly, and fix the help text.
- **Fix with:** `api-surface-and-module-boundaries`
- **Effort:** M

