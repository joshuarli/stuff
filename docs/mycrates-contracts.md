# mycrates contracts

The wire and on-disk formats that `mycrates` produces and consumes. The Cloudflare Worker, Cargo, the `stuff` CLI, and `deploy/r2-*.py` read these artifacts, so the implementation is free to change but the bytes are not.

`stuff publish cargo` is the only Cargo publishing entry point. The upload API and `serve-publish` described below are the legacy Python oracle (`publish.py`, `server.py`), kept for the golden tests and the kept maintenance commands; they are not a publishing entry point.

Executable form: `crates/mycrates/tests/golden/` holds inputs and the exact outputs the original Rust implementation produced for them. `tests/test_golden.py` fails on any byte of difference. `tests/test_contracts.py` checks the structural rules below against generated artifacts.

## State directory

```
state/
  config.json          configuration, version 1
  lock                 exclusive flock for every state mutation
  blobs/<sha256>.crate immutable archives, content addressed
  native/<name>/<version>.json    one record per privately published version
  upstream/<name>/<version>.json  one record per mirrored upstream version
  pins/<project>.json             one file per imported lockfile
  uploads/                        spill files for in-flight uploads, reclaimed after one hour
```

`<name>` is the lowercased crate name. `<version>` is the full SemVer text including build metadata. Names and versions are never URL-decoded or normalized further, and a second version that differs only in build metadata is a conflict.

### config.json

Pretty JSON, two-space indent, no trailing newline, fields in this order. Unknown fields are rejected.

| Field | Type | Default |
| --- | --- | --- |
| `version` | 1 | 1 |
| `public_base_url` | string | `http://localhost:8080` |
| `upstream.index_url` | string | `https://index.crates.io/` |
| `limits.*` | integers: `max_compressed`, `max_expanded`, `max_entries`, `max_ratio`, `concurrent_requests`, `fetch_bytes`, `fetch_timeout_secs`, `request_timeout_secs`, `redirects`, `serve_connections`, `serve_writes`, `upload_timeout_secs` | see README |
| `auth_required` | bool | false |

### Native record

```json
{"name": "alpha", "vers": "0.2.0", "checksum": "<sha256>", "yanked": false, "entry": { ...index entry object... }}
```

`name`, `vers`, `checksum` (= `entry.cksum`), and `yanked` must equal the entry's fields; a record that disagrees is an integrity error on load.

### Upstream record

```json
{"name": "serde", "vers": "1.0.0", "checksum": "<sha256>", "raw_index_line": "<crates.io line, verbatim>", "acquired_at": 0}
```

`raw_index_line` is republished byte for byte. `acquired_at` is Unix seconds, `0` when the record came from `adopt`.

### Pins

```json
{"project": "...", "lock_path": "...", "lock_content": "<Cargo.lock text>", "registry": [{"name": "", "version": "", "checksum": "", "source": ""}], "unsupported": ["git: name version"]}
```

`project`, `lock_path`, and `lock_content` are omitted when absent.

### Legacy TOML records

The Rust implementation wrote native records and pins as TOML (`native/<name>/<version>.toml`, `pins/<project>.toml`). Every command except `migrate-state` refuses a state directory that still holds them. `mycrates migrate-state` rewrites each as JSON with the same fields and removes the TOML file. The index lines a migrated state publishes are byte-identical to the ones the TOML state published.

## Sparse index entry

One JSON object per line, one file per crate at the Cargo sparse shard path of the lowercased name (`1/a`, `2/ab`, `3/a/abc`, `ab/cd/abcd`). Lines are ordered by SemVer precedence ascending (text order if either side does not parse), the file ends with a newline, and native files carry only versions of that crate.

Native entries are serialized with this exact key order and no whitespace:

`name`, `vers`, `deps`, `cksum`, `features`, `yanked`, then when present `links`, `rust_version`, `v`, then any unknown fields sorted by key.

Each dependency: `name`, `req`, `features`, `optional`, `default_features`, then when present `target`, `kind`, then `registry` (always; `null` for the same registry), then `package` when present.

- `name` is the name the depending crate uses (the rename), `package` the real crate name when renamed.
- `features` object keys are sorted; `v` is 2 when any feature value contains `dep:` or `?`, else 1.
- Strings escape only `"`, `\`, `\n`, `\r`, `\t`, and other control characters below 0x20 as `\u00xx` with lowercase hex. Everything else is emitted as UTF-8.
- Upstream lines are the crates.io lines, trimmed, unchanged.

## Upload and yank API

`PUT /api/v1/crates/new` carries `u32le(json length) json u32le(crate length) crate`, with `Content-Length` equal to the total. Chunked bodies get 411.

Metadata fields: `name`, `vers`, `deps` (required); `features`, `links`, `rust_version`, `checksum`, `yanked` (optional); other fields such as `description` are ignored. A dependency has `name`, `version_req`, `features`, `optional`, `default_features`, `target`, `kind`, `registry`, `package`, `explicit_name_in_toml`; `default_features` defaults to false when absent, as in the Rust implementation. For a renamed dependency `explicit_name_in_toml` is the alias and `name` the real crate, which becomes the entry's `package`.

The archive is the authority. The packaged `Cargo.toml` and the metadata must agree on:

- name, version, `links`, `rust-version`;
- features, compared as sets per feature;
- every dependency, keyed by (alias, real crate, kind, whitespace-free target): requirement, feature set, `optional`, `default_features`, registry, and `package`;
- the requirement is equal when the text matches or when both denote the same version interval (`1.0` equals `^1.0` equals `>=1.0.0, <2.0.0`). This is a normalization, not resolution; Cargo resolves.

A disagreement is HTTP 422 with an `integrity:` detail. Registry `https://github.com/rust-lang/crates.io-index` is the only accepted foreign registry.

Responses are JSON. Success is 200 with `{"warnings":{"invalid_categories":[],"invalid_badges":[],"other":[]}}`; failures are `{"errors":[{"detail":"<kind>: <message>"}]}`. Status by kind: input 400, integrity 422, conflict 409, limit 413, network 502, other 500, plus 401 (token), 404, 408 (timeout), 411 (length), 503 (admission).

`DELETE /api/v1/crates/<name>/<version>/yank` and `PUT .../unyank` flip `yanked` in the native record and republish; success is `{"ok":true}`, an unknown version is 404.

All writes need the token (`Authorization: <token>` or `Bearer <token>`, compared as SHA-256 digests in constant time). Reads need it only when `auth_required` is true.

## Site layout

```
site/
  blobs/<sha256>.crate               hard links to state/blobs, never overwritten
  generations/<id>/
    registry/config.json             {"dl":"<base>/blobs/{sha256-checksum}.crate","api":"<base>","auth-required":false}
    registry/<shard>                 native index files
    crates-io/config.json            {"dl":"<base>/blobs/{sha256-checksum}.crate"}
    crates-io/<shard>                upstream index files
    catalog.json  index.html  llms.txt
  current -> generations/<id>        relative symlink, replaced atomically
```

`<base>` is `public_base_url` without a trailing slash (`api` keeps it as configured). This is the Python oracle's layout. `stuff publish cargo` puts `dl` on `cargo.download_base_url` instead, which equals `public_base_url` unless it is configured separately (see [PUBLISHING.md](PUBLISHING.md#edge-protection)).

## Generation identity

`<id>` is the first 24 hex characters of the SHA-256 of

```
v:4\n
catalog:<fingerprint>\n
<view>|<path>|<sha256 of file>\n   for every config.json and index file, sorted by (view, path)
```

with `view` one of `registry` or `crates-io`. The fingerprint is the SHA-256 of `catalog.json`, `index.html`, and `llms.txt` rendered with generation `fingerprint` and origin `public_base_url`, joined by NUL. The id therefore covers every byte served except the three catalog files stamped with the id itself. Renderer version 4 is the value above.

A generation directory is immutable: publishing an id that exists with different content fails with an integrity error and changes nothing. A noop publish leaves `current` alone.

## catalog.json (schema 4)

Schema 4 is what `crates/stuff-catalog` writes and reads; schema 3 added the per-kind dependency lists and schema 4 added the `oci` registry. Cargo catalogs are structurally unchanged between them.

```
{
  "schema_version": 4,
  "registry": "cargo",
  "generation": "<id>",
  "packages": [
    {
      "name": "...", "source": "native"|"upstream", "latest": "<version>",
      "versions": [{"version": "...", "yanked": false}],
      "dependencies": {"runtime": [], "peer": [], "build": [], "dev": []}
    }
  ]
}
```

Packages are sorted by (name, source), versions by SemVer precedence, dependency names sorted and unique. `latest` is the highest stable non-yanked version, falling back to the highest non-yanked, then the highest yanked. Dependencies are those of the latest version, by real crate name (`package` when renamed); `normal` is `runtime`. Formatting is fixed: two-space indent, one version per line, a trailing newline.

The shared catalog also represents npm, PyPI, and OCI. Dependency names refer
only to the same registry: npm contributes runtime, peer, and dev kinds;
PyPI contributes canonicalized `Requires-Dist` names without extras as runtime
dependencies. OCI uses tags as versions and has no dependency lists.
Readers accept schemas 1 through 4: schema 1 has no dependency data and schema
2 has runtime names only. Pages distinguish missing dependency data from an
empty set. Readers predating schema 4 reject it, so deploy readers before
publishing the newer format. See `crates/stuff-catalog/src/model.rs`.

The promotion guard in `crates/stuff/src/publish/r2/guard.rs` reads `packages[].versions[].version` and `source` from this file to refuse promotions that drop a served version.

## index.html and llms.txt

Both are templates over the catalog and `public_base_url`; the golden generation holds their bytes. `index.html` embeds `crates/stuff-catalog/assets/search.js` and the stylesheet. The stylesheet contains a literal backslash-n sequence between two rules inherited from the Rust renderer; it is kept because the page is part of the fingerprint, and removing it is a coordinated change with `stuff-catalog`.

## Known differences from the Rust implementation

- Unknown index fields are kept as parsed, including `null` members; the Rust implementation silently dropped null members nested inside them. Only adopted foreign lines can carry such fields.
- Archive paths with an empty or `.` component in the middle (`a//b`, `a/./b`) are rejected; the Rust path parser collapsed them.
- Error details for a mismatched dependency with a target print the target as a quoted string rather than Rust's `Some("...")`.
