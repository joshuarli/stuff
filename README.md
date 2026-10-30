# stuff

One repository, three registries, one serving platform.

`stuff` consolidates the `mycrates` (Cargo), `mynpm` (npm), and `mypypi`
(Python wheel) static registries onto shared Rust infrastructure for HTTP
serving, storage, and generation management. Package semantics stay
per-ecosystem; only the serving layer is shared.

## Layout

| Path | What |
| --- | --- |
| `crates/mycrates` | Cargo sparse registry + archive (Rust) |
| `tools/mynpm` | npm archive (TypeScript, Node 26, pnpm 12.15.0) |
| `tools/mypypi` | Wheel index (CPython 3.15, uv) |
| `crates/h12tiny-*` | TLS client crates mycrates builds on, imported with history |
| `crates/stuff-store` | Generation activation, immutable blob sync, durable writes |
| `crates/stuff-http` | Content types, cache policy, path resolution, conditionals, ranges |
| `crates/stuff-server` | Native static file server over the shared semantics |
| `crates/stuff-worker` | Cloudflare Worker adapter (same semantics over R2) |
| `deploy/` | Pipeline checks; the worker project lives in `crates/stuff-worker` |

Each registry publishes immutable generations activated through a `current`
pointer (`current -> generations/<id>`). One registry's publication never
rebuilds or republishes another's output.

## Develop

Rust (nightly-2026-09-15, see `rust-toolchain.toml`):

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --locked --all-targets -- -D warnings
```

npm archive:

```sh
cd tools/mynpm
pnpm install --frozen-lockfile --ignore-scripts
pnpm check
```

Wheel index:

```sh
cd tools/mypypi
uv run --locked --python 3.15 pytest -q -m "not network" tests
```

Pipeline independence (offline; also runs in CI):

```sh
deploy/check-pipeline-independence.sh
```

## Serve

Serve a published site directory (or a mounted registry prefix) locally:

```sh
cargo run -p stuff-server -- --route /=path/to/site
cargo run -p stuff-server -- --route /mirror=path/to/mirror --max-connections 64
```

The server resolves `current/` through the generation symlink, serves
`generations/` paths as immutable, supports conditional and range requests,
and serves `index.html` inside directories (PEP 503 simple pages).

## Deploy

The Cloudflare Worker in `crates/stuff-worker` serves the same layouts from
R2. Its project files (`wrangler.toml`, generated `cloudflare.config.ts`)
live next to the crate. Validate without credentials:

```sh
cd crates/stuff-worker
cf migrate --dry-run ./wrangler.toml
cf deploy --dry-run
```

Real deploys are manual: `cf deploy` (use `--provision` once to create the
`stuff-mycrates`, `stuff-mynpm`, and `stuff-mypypi` buckets). Sync a
published site to its bucket per registry with `deploy/r2-sync.sh`, which
translates the `current` symlink to a marker object holding the generation
id (verified against the local R2 simulation).

## License

Apache-2.0 for stuff files; imported crates keep their own license fields.
