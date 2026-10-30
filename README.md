# stuff

One repository, three registries, one serving platform.

`stuff` consolidates the `mycrates` (Cargo), `mynpm` (npm), and `mypypi`
(Python wheel) static registries onto shared Rust infrastructure for HTTP
serving, storage, and generation management. All first-party registry
logic is Rust; external ecosystems are orchestrated, not reimplemented
(npm/pnpm resolve and install, uv/CPython build and install wheels).
Package semantics stay per-ecosystem; only the serving layer is shared.

## Layout

| Path | What |
| --- | --- |
| `crates/mycrates` | Cargo sparse registry + archive (Rust) |
| `crates/mynpm` | npm archive (Rust; `mynpm` binary) |
| `crates/mypypi` | Wheel index (Rust; `mypypi` binary) |
| `crates/h12tiny-*` | TLS client crates mycrates builds on, imported with history |
| `crates/stuff-store` | Generation activation, immutable blob sync, durable writes |
| `crates/stuff-http` | Content types, cache policy, path resolution, conditionals, ranges |
| `crates/stuff-server` | Native static file server over the shared semantics |
| `crates/stuff-worker` | Cloudflare Worker adapter (same semantics over R2) |
| `deploy/` | Pipeline checks and ecosystem smoke; the worker project lives in `crates/stuff-worker` |
| `tests/fixtures` | Shared registry fixtures (lockfiles, configs, tarballs, wheels) |

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

Ecosystem smoke (needs Node 26 + npm 12.2.1, uv + CPython 3.15, network):

```sh
deploy/ecosystem-smoke.sh
```

Pipeline independence (also runs in CI):

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
R2. URL routing and response planning live in the crate's Rust code
(`src/map.rs`, `src/api.rs`, shared `stuff-http` semantics, `deser` for
config parsing — no `worker` SDK, no async runtime in the dependency tree);
routes match on the request hostname, so each registry serves from its own
domain root. The thin fetch handler is `src/entry.ts`, which initializes the
compiled wasm once (`initSync`) and talks to R2 directly. Its project files
(`wrangler.toml`, generated `cloudflare.config.ts`) live next to the crate.
Build the wasm and validate without credentials:

```sh
cd crates/stuff-worker
npm run build:wasm
cf migrate --dry-run ./wrangler.toml
cf deploy --dry-run
```

`npm run build:wasm` compiles the crate for `wasm32-unknown-unknown` and
runs `wasm-bindgen --target web` into `build/` (explicit init: the bundler
target's namespace import does not resolve to the wasm exports under
wrangler); keep the installed `wasm-bindgen` CLI at the version pinned in
`Cargo.lock`. `wrangler deploy` (and `cf deploy`) runs the same build
automatically via the `[build]` command before bundling `src/entry.ts`.

Real deploys are manual: `cf deploy` (use `--provision` once to create the
`stuff-mycrates`, `stuff-mynpm`, and `stuff-mypypi` buckets). The
`[[routes]]` entries in `wrangler.toml` attach the `crates`, `npm`, and
`pypi` custom domains, each serving its registry from `/` (see `STUFF_ROUTES`
hosts). Sync a published site to its bucket per registry with
`deploy/r2-sync.sh`, which translates the `current` symlink to a marker
object holding the generation id (verified against the local R2 simulation). Generation-scoped
immutable aliases (`wheels/`, `tarballs/`, `-/tarballs/`) are not uploaded;
the worker resolves them to the shared store instead. Files over the 300 MB
single-PUT cap fail loudly and need an out-of-band multipart upload.

## License

Apache-2.0 for stuff files; imported crates keep their own license fields.
