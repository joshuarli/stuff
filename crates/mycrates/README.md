# mycrates

Lean, self-hosted Cargo sparse registry and dependency archive, written in standard-library-only Python 3.11.

Eliminates reliance on crates.io for publishing private packages and preserves exact upstream dependencies for reproducible, offline builds.

## Architecture

The system provides two logically separate registry sparse indexes over a content-addressed artifact store:

1. **Native Registry (`current/registry/`)**: An independent Cargo registry for privately published packages (`cargo publish --registry mycrates`).
2. **Crates.io Archive (`current/crates-io/`)**: An exact-byte mirror for locked upstream releases consumed via Cargo source replacement.

```
site/
  blobs/
    <sha256>.crate
  generations/
    <generation-id>/
      registry/
        config.json
        <shard>/<name>
      crates-io/
        config.json
        <shard>/<name>
  current -> generations/<generation-id>
```

- **Static Read Path**: All downloads and index lookups are static HTTP GET requests. No runtime database or application server is needed on the read path.
- **Atomic Generations**: Publications create a deterministic generation ID from a canonical manifest of index files, link blobs, and atomically update the `current` symlink.
- **Configured Origins**: The generated `llms.txt` names the configured `public_base_url` in its Cargo instructions and origin, so a deployment at another origin does not point clients at a different registry. Published generations use renderer 4; generations rendered by earlier versions stay readable.
- **Strict Verification**: Lockfile checksum equals upstream sparse index checksum equals downloaded archive SHA-256.

## Installation

Nothing to build. Python 3.11 or newer runs it from a checkout; `cargo` is needed only for the `cargo publish` step of publishing and for `validate`:

```sh
ln -s "$PWD/crates/mycrates/bin/mycrates" ~/.local/bin/mycrates
```

or `PYTHONPATH=crates/mycrates python3 -m mycrates ...`. Formats are specified in [docs/mycrates-contracts.md](../../docs/mycrates-contracts.md); `make test-mycrates` runs the tests.

### Upgrading a state directory from the Rust tool

The Rust tool kept native records and pins as TOML. Back up the state directory, then convert it once:

```sh
mycrates migrate-state --state-dir STATE
```

Every other command refuses a state directory that still holds TOML records. The migrated state publishes the same index bytes, so the generation id does not change.

## Configuration

Configuration lives in `config.json` inside the state directory. `mycrates init` creates it, and every command that reads it refuses a state directory without one:

```json
{
  "version": 1,
  "public_base_url": "https://deps.example.net/mycrates",
  "upstream": {
    "index_url": "https://index.crates.io/"
  },
  "limits": {
    "max_compressed": 1073741824,
    "max_expanded": 4294967296,
    "max_entries": 200000,
    "max_ratio": 1000,
    "concurrent_requests": 4,
    "fetch_bytes": 10737418240,
    "fetch_timeout_secs": 3600,
    "request_timeout_secs": 300,
    "redirects": 5,
    "serve_connections": 64,
    "serve_writes": 4,
    "upload_timeout_secs": 300
  },
  "auth_required": false
}
```

`auth_required` controls reads. When true, `current/registry/config.json` advertises `"auth-required":true` and `serve-publish` requires the token for GET and HEAD. Writes always require the token when `serve-publish` has one. Keep it false for the public Worker and static servers, which do not enforce read auth.

## Quickstart

Initialize the registry directories:

```sh verified
mycrates init
```

### Importing Upstream Dependencies

Import exact crate versions and checksums from an existing `Cargo.lock`:

```sh verified
mycrates import-lock Cargo.lock --project myproject
```

To archive many lockfiles into one state directory, import each under its own project name with
`deploy/import-cargo-locks.sh STATE_DIR LOCKFILE...`. The project name is the lockfile's directory
path with `/` replaced by `-`, so nested lockfiles do not collide, and re-running rewrites the same
pin files.

Inspect the planned downloads:

```sh verified
mycrates plan
```

Fetch and verify all pinned upstream artifacts:

```sh verified
mycrates fetch
```

`fetch` downloads up to `limits.concurrent_requests` crates at once and stops with a limit error once
`limits.fetch_timeout_secs` has elapsed. The first failure aborts the run; crates already stored are
kept and skipped on the next run.

Verify storage integrity:

```sh verified
mycrates verify
```

Run doctor checks to detect unsupported sources or missing blobs:

```sh verified
mycrates doctor
```

### Publishing the Static Registry

Generate the static sparse index and expose it via the `current` generation:

```sh verified
mycrates publish
```

### Publishing Private Crates

Start the temporary write service for a publication:

```sh
mycrates serve-publish --bind 127.0.0.1:8080 --token-file ~/.local/state/stuff/mycrates/token
```

`serve-publish` binds loopback addresses only; it refuses any other host. It is the whole local registry for the duration of a publication: besides the write API it serves `current/`, `generations/`, and `blobs/` from the site directory, so Cargo can read the sparse index and a dependent native crate can download its dependency during `cargo publish`. The public registry is the static site on R2 behind the Worker. The server accepts up to `limits.serve_connections` connections, admits up to `limits.serve_writes` authenticated writes at once, and bounds each upload by `limits.upload_timeout_secs`.

Point Cargo at the registry in `~/.cargo/config.toml`. For a publication, use the loopback address; consumers use the public one:

```toml
[registries.mycrates]
index = "sparse+https://crates.17166969.xyz/current/registry/"
```

Export the publish authentication token:

```sh
export CARGO_REGISTRIES_MYCRATES_TOKEN="secret-operator-token"
```

In your private crate's `Cargo.toml`, set the publish guard to prevent accidental release to crates.io:

```toml
[package]
name = "my-private-crate"
version = "0.1.0"
edition = "2021"
publish = ["mycrates"]
```

Publish the crate with native Cargo:

```sh
cargo publish --registry mycrates
```

### Consuming Private Crates

In consumer projects, specify the registry:

```toml
[dependencies]
my-private-crate = { version = "0.1.0", registry = "mycrates" }
```

### Cargo Source Replacement for crates.io

To resolve public dependencies entirely from your self-hosted mirror without external crates.io access:

```toml
[source.crates-io]
replace-with = "mycrates-mirror"

[source.mycrates-mirror]
registry = "sparse+https://crates.17166969.xyz/current/crates-io/"
```

Existing `Cargo.lock` files retain their original `crates.io` source identity and checksums.

### Yanking Releases

Yank and unyank versions using native Cargo against a local `serve-publish` whose state and site are the publication's, with `CARGO_REGISTRIES_MYCRATES_INDEX` pointing at it; then restore the public base URL, publish, and sync:

```sh
cargo yank --registry mycrates --version 0.1.0 my-private-crate
cargo yank --undo --registry mycrates --version 0.1.0 my-private-crate
```

### Validating the Installation

Validate that all archived packages resolve and fetch under proxy denial:

```sh verified
mycrates validate --project-dir .
```

## Web Server Deployment

The site directory is plain static files, so any static server can host the read path. The write API is not meant to be exposed; it exists for the loopback publication flow above.

### Nginx

```nginx
server {
    listen 443 ssl http2;
    server_name deps.example.net;

    ssl_certificate /etc/ssl/certs/deps.crt;
    ssl_certificate_key /etc/ssl/private/deps.key;

    # Static registry files and blobs
    location /mycrates/ {
        alias /var/lib/mycrates/site/;
        autoindex off;
        etag on;
        add_header Cache-Control "public, max-age=60";
    }

    location /mycrates/blobs/ {
        alias /var/lib/mycrates/site/blobs/;
        etag on;
        add_header Cache-Control "public, max-age=31536000, immutable";
    }
}
```

### Caddy

```caddy
deps.example.net {
    handle /mycrates/blobs/* {
        root * /var/lib/mycrates/site
        file_server
        header Cache-Control "public, max-age=31536000, immutable"
    }

    handle /mycrates/* {
        root * /var/lib/mycrates/site
        file_server
        header Cache-Control "public, max-age=60"
    }
}
```

## Cache and ETag Semantics

- **Blobs (`/blobs/<sha256>.crate`)**: Completely immutable. Serve with `Cache-Control: public, max-age=31536000, immutable`.
- **Index (`/current/...`)**: Mutable generation pointer. Serve with `ETag` and short cache lifetime (`max-age=60`). When new versions are published, generation switching generates new ETags and atomic index updates.

## Backup, Restore, and Recovery

- **Backup**:
  - `state/`: Contains records (`native/`, `upstream/`, `pins/`, `config.json`), content-addressed `blobs/`, and `uploads/` for in-flight publish spools (reclaimed after an hour under the store lock).
  - `site/blobs/`: Content-addressed `.crate` archives.
- **Restore**:
  1. Restore `state/` and `site/blobs/`.
  2. Run `mycrates publish` to regenerate the static generation index from state and link blobs.
  3. Run `mycrates verify` to recheck all blob SHA-256 hashes against stored records.
- **Adopt a published site** (no `state/` left, only a pulled or served `site/`): run
  `mycrates init`, `mycrates config set-public-base-url URL`, then
  `mycrates adopt --from-site SITE --state-dir STATE`. It reads the index lines of the site's
  `current` generation (`registry/` becomes native records, `crates-io/` upstream records), ingests
  each referenced blob from `SITE/blobs/` and rejects any blob whose SHA-256 differs from its index
  checksum, so a following `mycrates publish` renders the identical generation. Blobs the current
  generation no longer references are not adopted. Upstream records get `acquired_at` 0 because
  the published index does not carry it.
- **Crash Recovery**: If a publish process was interrupted, running `mycrates publish` automatically cleans up staging leftovers (`.staging.*`) and recovers cleanly. If it instead refuses to replace an immutable generation, `site/generations/<id>` exists with content that fails verification; remove that directory and publish again.

## Security and Limitations

- **Partial Archive Limitation**: An archive populated from a specific project's `Cargo.lock` contains only the exact versions locked by that project. It is not an arbitrary SemVer solver mirror for unpinned versions.
- **Non-Registry Dependencies**: Git repositories and path dependencies cannot be served via sparse crate indexes; `mycrates import-lock` and `doctor` explicitly report these as unsupported.
- **Single Token Model**: The write service uses single-operator bearer token authentication comparing SHA-256 digests in constant time. Tokens must be stored in mode `0600` files, and the service binds loopback only.
