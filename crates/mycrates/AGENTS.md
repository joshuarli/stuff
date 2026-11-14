# AGENTS.md

`mycrates` is a self-hosted Cargo sparse registry and dependency archive in standard-library-only Python 3.11. Run it as `crates/mycrates/bin/mycrates` or `python3 -m mycrates` with `crates/mycrates` on `PYTHONPATH`. Tests are `unittest`: `make test-mycrates` (from `crates/mycrates`: `python3 -m unittest discover -t . -s tests`). No third-party packages, no formatter. Wire and on-disk formats are in [docs/mycrates-contracts.md](../../docs/mycrates-contracts.md) and pinned by `tests/golden/`; change a format, its contract text, and its golden together.

`stuff publish cargo` is the only Cargo publishing entry point. The Python publish path (`publish.py`, and `server.py` behind `serve-publish`) is the legacy parity oracle for the golden tests and a dependency of the kept maintenance commands; it is not a publishing entry point.

## Module map

- `names.py`, `semver.py`, `requirements.py`: crate name rules and shard paths, SemVer parsing and precedence, and requirement normalization. Requirements are compared by version interval only; Cargo resolves.
- `model.py`: `IndexEntry`, `IndexDep`, `PublishBody` with strict field typing and the byte-exact index line serializer.
- `wirejson.py`: JSON escaping that matches the index contract, and strict loading.
- `config.py`: `config.json` in the state directory, including `limits`.
- `errors.py`: the error kinds, exit codes, and HTTP statuses.
- `index.py`: sparse `config.json` documents, `publish_to_index`, and reconciliation of upload metadata against the packaged manifest.
- `crate_file.py`: `scan_crate_staged`, the single archive gate for uploads and upstream downloads (gzip/tar integrity, entry types, path traversal, `name-version/` prefix, `Cargo.toml` parity, size and ratio limits), without extracting to disk; and the packaged manifest parser.
- `lockfile.py`, `pins.py`: Cargo.lock (v3/v4) parsing and source classification; exact registry pins in `pins/<project>.json`.
- `store.py`: content-addressed blob store, native and upstream records, the exclusive store lock (`flock`), name collision and build-metadata identity rules, and SHA-256 verification.
- `migrate.py`: one-way conversion of the Rust tool's TOML records to JSON (`migrate-state`).
- `fsutil.py`, `site.py`: durable writes, locks, and atomic generation activation with immutable blob sync.
- `catalog.py`: catalog JSON, HTML, `llms.txt`, and fingerprint rendering, byte-compatible with `crates/stuff-catalog`; the search script and stylesheets are read from `crates/stuff-catalog/assets/`.
- `publish.py`: generation rendering, the manifest-derived generation id, and activation.
- `adopt.py`: rebuild records and blobs from a published site.
- `wire.py`, `server.py`: framing for `PUT /api/v1/crates/new`, and the loopback-only `serve-publish` server (write API, yank/unyank, sparse index and blob reads).
- `upstream.py`, `acquire.py`: crates.io client with `dl` template expansion, bounded downloads and the SHA-256 checks; `plan`, `fetch`, `verify` with the trust chain (lockfile checksum equals index `cksum` equals downloaded bytes).
- `validate.py`: offline validation of locked projects through Cargo proxy denial and source replacement.
- `cli.py`: one handler per subcommand.
