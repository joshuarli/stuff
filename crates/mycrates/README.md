# mycrates

Lean, self-hosted Cargo sparse registry and dependency archive.

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
- **Strict Verification**: Lockfile checksum equals upstream sparse index checksum equals downloaded archive SHA-256.

## Installation

Build and install `mycrates` from source using Rust stable:

```sh
cargo build --release
cp target/release/mycrates /usr/local/bin/
```

## Configuration

Configuration lives in `config.json` inside the state directory:

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
    "redirects": 5
  },
  "auth_required": false
}
```

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

Inspect the planned downloads:

```sh verified
mycrates plan
```

Fetch and verify all pinned upstream artifacts:

```sh verified
mycrates fetch
```

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

Start the minimal write service:

```sh
mycrates serve-publish --bind 127.0.0.1:8080 --token-file /etc/mycrates/token
```

Configure Cargo client credentials and endpoint in `~/.cargo/config.toml`:

```toml
[registries.mycrates]
index = "sparse+https://deps.example.net/mycrates/current/registry/"
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
registry = "sparse+https://deps.example.net/mycrates/current/crates-io/"
```

Existing `Cargo.lock` files retain their original `crates.io` source identity and checksums.

### Yanking Releases

Yank and unyank versions using native Cargo:

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

    # Write API
    location /mycrates/api/v1/crates/ {
        proxy_pass http://127.0.0.1:8080/api/v1/crates/;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        client_max_body_size 1024M;
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

    handle /mycrates/api/v1/crates/* {
        reverse_proxy http://127.0.0.1:8080
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
  - `state/`: Contains records (`native/`, `upstream/`, `pins/`, `config.json`).
  - `site/blobs/`: Content-addressed `.crate` archives.
- **Restore**:
  1. Restore `state/` and `site/blobs/`.
  2. Run `mycrates publish` to regenerate the static generation index from state and link blobs.
  3. Run `mycrates verify` to recheck all blob SHA-256 hashes against stored records.
- **Crash Recovery**: If a publish process was interrupted, running `mycrates publish` automatically cleans up staging leftovers (`.staging.*`) and recovers cleanly.

## Security and Limitations

- **Partial Archive Limitation**: An archive populated from a specific project's `Cargo.lock` contains only the exact versions locked by that project. It is not an arbitrary SemVer solver mirror for unpinned versions.
- **Non-Registry Dependencies**: Git repositories and path dependencies cannot be served via sparse crate indexes; `mycrates import-lock` and `doctor` explicitly report these as unsupported.
- **Single Token Model**: The write service uses single-operator bearer token authentication comparing SHA-256 digests in constant time. Tokens must be stored in mode `0600` files.
