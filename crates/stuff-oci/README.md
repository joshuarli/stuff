# stuff-oci

The OCI Distribution 1.1 protocol core shared by `myoci` and the Cloudflare
Worker. It performs no I/O, contains no HTTP client, is `wasm`-safe, and is
`#![forbid(unsafe_code)]`.

## Scope

- Digest and reference parsing (`sha256` only), repository and tag grammar.
- Media-type policy for OCI and Docker schema 2 images, indexes, configs,
  layers, artifacts, and referrers.
- Manifest and index parsing, validation, descriptor rendering, and
  referrer descriptor construction.
- Storage keys and repository-view JSON (`repository.json`, `blobs.json`,
  referrers indexes).
- Read and write request parsing, pagination, conditional and range logic,
  OCI error JSON, and response plans.
- HMAC upload-session tokens and Basic authentication parsing.

The crate deliberately does not implement a generic `Package` abstraction or
ecosystem policy; `myoci`, `stuff-catalog`, and the Worker supply storage,
rendering, and HTTP.

## Modules

| Module | Responsibility |
| --- | --- |
| `digest`, `reference`, `media` | Digest, reference, and media-type grammar |
| `manifest`, `referrers` | Manifest/index parsing, validation, descriptors |
| `view`, `referrers` | Repository views, tags, blobs, referrer indexes |
| `request`, `response` | OCI read/write request parsing and response plans |
| `session` | Upload-session tokens and Basic authentication parsing |
| `generation` | Content-derived generation identity |
| `error` | OCI error codes and JSON |

## Tests

```sh
cargo test -q -p stuff-oci
cargo clippy -q -p stuff-oci --all-targets -- -D warnings
```

The parity table is exercised by `deploy/oci-conformance.sh` through `myoci`
and the Worker; the checked-in fixture is `tests/fixtures/oci/layout`.
