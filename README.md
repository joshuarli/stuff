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

High-level admin tasks are wrapped in the top-level `Makefile` (`make help`
lists them: build, test, smoke, worker deploy, R2 sync, local serve).

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

All three publish CLIs share one exit-code contract: `2` usage/input,
`3` integrity, `4` conflict, `5` limit, `6` network, `1` other
operational failures (`mynpm` adds `130` for a cancelled run; `stuff
doctor` exits `1` on a failed check and `2` on its own error).

## Serve

Serve a published site directory (or a mounted registry prefix) locally:

```sh
cargo run -p stuff-server -- --route /=path/to/site
cargo run -p stuff-server -- --route /mirror=path/to/mirror --max-connections 64
```

The server resolves `current/` through the generation symlink, serves
`generations/` paths as immutable, supports conditional and range requests,
and serves `index.html` inside directories (PEP 503 simple pages).
`/` serves the generation's catalog landing page (`current/index.html`).
`/llms.txt` serves the generation's LLM-readable inventory
(`current/llms.txt`, also pinned under `generations/<id>/llms.txt`).

## Ingest Python pins from lockfiles

`mypypi add` takes exact `name==version` pins and resolves their
dependencies with `uv pip compile` for every configured target. Pins can come
from the registry packages of `uv.lock` files and from requirements files
(comments and `--hash` continuations are ignored):

```sh
mypypi --catalog packages.toml --config pyproject.toml --state-dir state add \
  --uv-lock ../app/uv.lock --requirements pins.txt extra==1.0
```

Only `[[package]]` entries whose `source` is a registry are read; virtual,
editable, path, and git packages are skipped. Run `add` once per lockfile:
one invocation rejects two versions of the same package, and lockfiles in a
fleet usually disagree. The lockfile hashes are not carried into the catalog;
compare them with the published wheel hashes after publishing.

Wheels are validated on a native runner, so one host publishes only its own
platform. Packages that exist only for other operating systems (for example
`pyobjc-*` on Linux) need `platforms = [...]` in their catalog entry, and
wheels that fail validation by design need `exclude_platforms` or
`validate_skip_imports` entries with a reason.

One generation serves every platform: run build, validate, and publish once
per platform against the same catalog, state directory, and site. Each
`publish` adds that platform's wheels to the current generation and keeps the
rest. Linux platforms run in a container of the matching architecture, which
is a native runner for mypypi's check (the binary is compiled inside the
container, so `current_platform()` reports the container's architecture):

```sh
deploy/mypypi-container.sh arm64 linux-aarch64 build
deploy/mypypi-container.sh arm64 linux-aarch64 validate
deploy/mypypi-container.sh arm64 linux-aarch64 publish
deploy/mypypi-container.sh amd64 linux-x86_64 validate   # Rosetta/QEMU on an arm64 host
```

`linux-x86_64` on an arm64 host runs under emulation; the check accepts it
because the userspace and binary are x86_64, not because the code runs on x86
silicon. Publish the Linux wheels from a real x86_64 runner when that matters.
`add` takes no `--platform` and rewrites the shared catalog; run it before the
per-platform commands.

## Catalog

Every publish renders a normalized package catalog from the registry's
authoritative published state (never from input lockfiles), plus a
human-readable landing page and an LLM-readable `llms.txt`, into each
generation:

```sh
cargo run -p stuff -- ls
cargo run -p stuff -- ls --registry npm --versions
cargo run -p stuff -- search h12tiny
cargo run -p stuff -- diff before/catalog.json after/catalog.json
cargo run -p stuff -- doctor --remote --sample-artifacts
```

`ls` works against the live registries by default (no credentials), and
against local output with `--cargo-dir/--npm-dir/--pypi-dir` (site,
generation, or `catalog.json`). `--json` emits machine-readable inventory,
`--versions` lists every published version, and `render --output index.html`
aggregates the three catalogs into one page (`render --output llms.txt`
renders the aggregate `llms.txt` instead). Health checks cover the
active generations, pinned catalog objects, essential endpoints (including
`current/llms.txt`), and one
fully hash-verified sample artifact per registry.

`stuff.17166969.xyz` serves that aggregate page from the Worker, rendered
with the same Rust catalog code from the three current snapshots (cached
60 s at the edge). The machine-readable packuments, sparse indexes,
`/simple/` pages, and per-registry `llms.txt` files are unchanged; the
aggregate host also serves the combined inventory at `/llms.txt`.

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
`[[routes]]` entries in `wrangler.toml` attach the `crates`, `npm`,
`pypi`, and `stuff` custom domains, each serving its registry from `/`
(see `STUFF_ROUTES` hosts). Registry roots for clients include the
generation pointer:
`https://crates.17166969.xyz/current/registry/` (cargo sparse),
`https://npm.17166969.xyz/current/` (npm), and
`https://pypi.17166969.xyz/current/simple/` (pip). The publish helpers keep
state in `$STUFF_STATE_HOME/<registry>` (default
`${XDG_STATE_HOME:-$HOME/.local/state}/stuff`), not in the calling checkout;
that directory is the publication source of truth and should be backed up.
A directory lock serializes the whole transaction across source repositories.
Sync a published site to its bucket per registry with
`deploy/r2-sync.sh SITE_DIR BUCKET [--expected ACTIVE_GEN] [--repair] [--dry-run]`,
which translates the `current` symlink to a marker object holding the
generation id (verified against the local R2 simulation). Sync is incremental
and content-aware: remote objects whose size and content hash match are
skipped, so re-syncing an unchanged generation uploads nothing. Remote
objects are found by prefix listing (`deploy/r2-plan.py`), which works for
any object count without pagination cursors; uploads run `R2_PUT_JOBS`
at a time (default 8; 1 against the local simulation, which deadlocked at
8 concurrent writers). Every sync writes a
`manifests/<id>.json` for each local generation, verifies every listed object
remotely, then replaces `current` with an `If-Match` write (or
`If-None-Match: *` on first publish); a competing publisher gets 412 instead
of silently winning. `--expected` additionally pins the generation that must
already be active. Existing immutable objects with different bytes are
conflicts; `--repair` is the separate, explicit overwrite path. `--dry-run` lists the
bucket read-only and prints the object and byte counts it would upload, then stops
before any upload or promotion (it needs no `R2_S3_*` variables). Conditional
promotion needs `R2_S3_ENDPOINT`, `R2_S3_ACCESS_KEY_ID`, and
`R2_S3_SECRET_ACCESS_KEY`. Without S3 credentials,
`R2_PROMOTE_UNCONDITIONAL=1` verifies the generation remotely and then moves
`current` with a plain `cf` put, which does not detect a competing publisher.
Roll back with
`deploy/r2-rollback.sh BUCKET GEN_ID`, which verifies the target generation's
manifest remotely before moving `current`. Generation-scoped
immutable aliases (`wheels/`, `tarballs/`, `-/tarballs/`) are not uploaded;
the worker resolves them to the shared store instead, including direct
`generations/<id>/...` requests. Both `stuff-server`
and the worker also serve the standard npm tarball path
`current/<name>/-/<basename>-<version>.tgz` (pnpm requests it from
integrity-only lockfiles): the version's `dist.tarball` in the current
packument names the digest object, served with immutable caching; unknown
versions are 404. pnpm 12 also checks the publish time of every lockfile
entry, including platform packages mynpm does not host; those are published as
metadata-only versions (see `crates/mynpm/README.md`), so default pnpm settings
install from such lockfiles. Files over the 300 MB
single-PUT cap upload via S3 multipart (`deploy/r2-multipart.py`, needs
`R2_S3_ENDPOINT`, `R2_S3_ACCESS_KEY_ID`, `R2_S3_SECRET_ACCESS_KEY`) and are
skipped when the remote multipart ETag already matches.
`deploy/test-r2-sync.sh [COUNT]` (`make test-sync`) checks pagination and
idempotency against the local simulation only. `make test-r2` checks the
multipart SigV4 canonicalization, listing past one page, and conditional
promotion against local fakes, and `make worker-test` builds the actual
Worker bundle and runs it under workerd (Miniflare) for cold-HEAD cache,
root-link, and pinned artifact-alias coverage. `make test-r2-live`
(`R2_ACCEPTANCE_BUCKET=...` plus `R2_S3_*`) is the opt-in real-R2 multipart
acceptance check; it is not run by CI.

## Adopt a live publication

When `$STUFF_STATE_HOME` is missing (new machine, lost disk) the next publish
would replace the live registry with the caller's subset. Rebuild the state
from the buckets first. Pulling only reads the bucket (`cf` authenticated);
each object is checked against the listing ETag and only the current
generation plus the shared stores are fetched:

```sh
H=${STUFF_STATE_HOME:-${XDG_STATE_HOME:-$HOME/.local/state}/stuff}
mkdir -p "$H/mycrates" "$H/mynpm" "$H/mypypi"
deploy/r2-pull.py stuff-mycrates "$H/pull-mycrates"
mycrates init --state-dir "$H/mycrates/state" --site-dir "$H/mycrates/site"
mycrates config set-public-base-url https://crates.17166969.xyz --state-dir "$H/mycrates/state"
mycrates adopt --from-site "$H/pull-mycrates" --state-dir "$H/mycrates/state"
deploy/r2-pull.py stuff-mynpm "$H/mynpm/site/mirror"
mynpm adopt --site-dir "$H/mynpm/site"
deploy/r2-pull.py stuff-mypypi "$H/mypypi/site"
```

`mycrates` publishes from `state/` alone, so `adopt` rebuilds its records from
the pulled index lines and blobs (blob SHA-256 must equal the index checksum).
`mynpm` and `mypypi` extend the `current` generation of their site, so the
pulled site is their state; `mynpm adopt` verifies it and republishes it
under the current renderer, and `mypypi` needs nothing beyond the pull. Delete
`$H/pull-mycrates` once adopted. The next publish then carries every live
package forward.

To ingest a whole fleet locally, `deploy/fleet-ingest.sh OUT_DIR --cargo
LIST --npm LIST --pypi LIST` (`make fleet-ingest`) runs each registry's
import, fetch, verify or validate, and publish into
`OUT_DIR/{mycrates,mynpm,mypypi}/site`. Lists hold Cargo.lock paths,
pnpm-lock.yaml paths, and `name==version` requirements. It never writes to R2;
sync each site afterwards (`make sync LOCAL=1` to rehearse).

After deploying, run the read-only live check (no credentials):

```sh
deploy/deployed-smoke.sh
```

It runs `stuff doctor --remote --sample-artifacts` plus `npm view` and
`uv pip install --dry-run` probes against the deployed origins.

## License

Apache-2.0 for stuff files; imported crates keep their own license fields.
