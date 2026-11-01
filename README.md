# stuff

One repository, three registries, one serving platform.

`stuff` consolidates the `mycrates` (Cargo), `mynpm` (npm), and `mypypi`
(Python wheel) static registries onto shared Rust infrastructure for HTTP
serving, storage, and generation management. All first-party registry
logic is Rust; external ecosystems are orchestrated, not reimplemented
(npm/pnpm resolve and install, uv/CPython build and install wheels).
Package semantics stay per-ecosystem; only the serving layer is shared.

## Registries

Each registry publishes immutable generations activated through a `current`
pointer (`current -> generations/<id>`). One registry's publication never
rebuilds or republishes another's output.

Build and test commands are in [AGENTS.md](AGENTS.md).

## Exit codes

All three publish CLIs share one exit-code contract: `2` usage/input,
`3` integrity, `4` conflict, `5` limit, `6` network, `1` other
operational failures (`mynpm` adds `130` for a cancelled run; `stuff
doctor` exits `1` on a failed check and `2` on its own error).

## Release

Run the `release` workflow from the default branch to publish the `stuff`
CLI. It builds Apple Silicon and Linux musl binaries, verifies them, and
attaches `stuff-<target>.zip` plus `SHA256SUMS` to a `release-<sha>`
prerelease. Install a release with `sh dl stuff` from `~/usr`.

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
`/index.html`, `/catalog.json`, `/llms.txt`, and `/simple/...` also alias
through `current` at the registry root, like the Worker.
`/llms.txt` serves the generation's LLM-readable inventory
(`current/llms.txt`, also pinned under `generations/<id>/llms.txt`).
Both adapters share `stuff-http`'s route plan, so slash-less directory
requests, shared wheel/tarball aliases that require an existing generation,
and cache kinds agree. Content-addressed tarballs and wheels, including
`-/tarballs/` and `tarballs/`, are immutable; other `current/` paths keep
the 60-second generation pointer.

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
per-platform commands. The container uses the same
`$STUFF_STATE_HOME/mypypi/{packages.toml,pyproject.toml,state,site}` paths as
`deploy/publish mypypi` and mounts them, so every platform must use that one
state home.

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
cargo run -p stuff -- doctor --sample-artifacts
```

`ls` works against the live registries by default (no credentials), and
against local output with `--cargo-dir/--npm-dir/--pypi-dir` (site,
generation, or `catalog.json`). Supplying any local dir limits `doctor` to
the supplied registries: it prints a skip check for each omitted registry
and refuses `--expected-generation` unless every selected registry is
checked. `--json` emits machine-readable inventory, `--versions` lists every
published version, and `render --output index.html` aggregates the three
catalogs into one page (`render --output llms.txt` renders the aggregate
`llms.txt` instead). Health checks cover the active generations, pinned
catalog objects, essential endpoints (including `current/llms.txt`), and one
fully hash-verified sample artifact per registry.

Catalog schema 3 records each package's dependencies as `runtime`, `peer`,
`build`, and `dev` name lists: the sorted, unique packages in the same
registry that its latest version depends on. Cargo supplies normal, build, and
dev kinds by real crate name. npm supplies `dependencies`, `peerDependencies`,
and `devDependencies`. PyPI supplies `Requires-Dist` without extras,
canonicalized, as runtime only, because wheel metadata has no dev, build, or
peer kinds. Both HTML pages link each package to its dependencies and
dependents through in-page `#registry/name` views, with dev, build, and peer
lists collapsed. Dependencies never cross registries. Schema 1 and 2 catalogs
still load; schema 2 has runtime names only and schema 1 has none, and the
pages say so instead of showing empty lists. Readers that predate schema 3
reject schema 3 catalogs, so deploy the Worker and `stuff` CLI before
publishing.

Both pages carry a package search box. It is a vendored MiniSearch (with its
bounded top-k selector, from `~/d/discover`) in `crates/stuff-catalog/web/`,
bundled into `crates/stuff-catalog/assets/search.js` and inlined in each page.
It indexes the table rows already on the page (name, registry, source), so the
pages carry no extra data, and it stays hidden without scripts. After editing
`web/`, run `make search-bundle` (needs `npm ci` in `crates/stuff-worker`) and
commit the bundle; `make test-search` runs its tests.

`stuff.17166969.xyz` serves that aggregate page from the Worker, rendered
with the same Rust catalog code from the three current snapshots (cached
60 s at the edge). The machine-readable packuments, sparse indexes,
`/simple/` pages, and per-registry `llms.txt` files are unchanged; the
aggregate host also serves the combined inventory at `/llms.txt`.
If one registry's catalog cannot be read, for example because it uses a newer
schema than the deployed Worker understands, the aggregate page and
`/llms.txt` list the other registries and say which one is missing. They only
fail when no catalog is readable. `stuff render` stays strict and fails.

## Deploy

See [docs/PUBLISHING.md](docs/PUBLISHING.md) for the publish wrapper, prerequisites, optional R2 credentials, and verification.

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

Real deploys are manual: `make deploy` builds the Worker bundle once, tests
it, deploys the same bundle with `cf deploy --prebuilt`, republishes the three
registries in parallel, and runs the live check. A bare `cf deploy` still
works (it rebuilds first); use `--provision` once to create the
`stuff-mycrates`, `stuff-mynpm`, and `stuff-mypypi` buckets. The
`[[routes]]` entries in `wrangler.toml` attach the `crates`, `npm`,
`pypi`, and `stuff` custom domains, each serving its registry from `/`
(see `STUFF_ROUTES` hosts). Registry roots for clients include the
generation pointer:
`https://crates.17166969.xyz/current/registry/` (cargo sparse),
`https://npm.17166969.xyz/current/` (npm), and
`https://pypi.17166969.xyz/current/simple/` (pip). Publish through
`deploy/publish REGISTRY`, which checks `cf` auth, adopts state that is not
marked complete, and runs the matching helper. The helpers keep
state in `$STUFF_STATE_HOME/<registry>` (default
`${XDG_STATE_HOME:-$HOME/.local/state}/stuff`), not in the calling checkout;
that directory is the publication source of truth and should be backed up.
A directory lock serializes the whole transaction across source repositories.
Sync a published site to its bucket per registry with
`deploy/r2-sync.sh SITE_DIR BUCKET [--expected ACTIVE_GEN] [--repair] [--dry-run] [--promote=cf|s3]`,
which translates the `current` symlink to a marker object holding the
generation id (verified against the local R2 simulation). Sync is incremental
and content-aware: remote objects whose size and content hash match are
skipped, so re-syncing an unchanged generation uploads nothing. Without
`R2_S3_*`, the sync mints short-lived bucket-scoped credentials from `cf`
(see [Fast path credentials](#fast-path-credentials)). Remote objects then
come from the S3 API; uploads run `R2_PUT_JOBS` in parallel (default 8). If
minting fails, prefix listing through `cf` (`deploy/r2-plan.py`) and a
cross-process pacer at roughly four requests a second remain the fallback.
The local simulation stays single-threaded. Every sync writes a
`manifests/<id>.json` for each local generation, verifies every listed object
remotely, then replaces `current`. Promotion defaults to `cf` for a single
publisher: it writes `current` with a plain put after the same verification
and needs no S3 credentials. `--promote=s3` uses `If-Match` (or
`If-None-Match: *` on first publish); a competing publisher gets 412 instead
of silently winning. `--expected` additionally pins the generation that must
already be active. Existing immutable objects with different bytes are
conflicts; `--repair` is the separate, explicit overwrite path. `--dry-run` lists the
bucket read-only and prints the object and byte counts it would upload, then stops
before any upload or promotion (it needs no `R2_S3_*` variables). During a
sync, S3 promotion uses the temporary credentials minted from `cf` when
`R2_S3_*` is not set; long-lived credentials can be supplied with
`R2_S3_ENDPOINT`, `R2_S3_ACCESS_KEY_ID`, and
`R2_S3_SECRET_ACCESS_KEY`. Roll back with
`deploy/r2-rollback.sh BUCKET GEN_ID`, which verifies the target generation's
manifest remotely before moving `current` and defaults to the same `cf`
promotion. Generation-scoped
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
After a successful sync, the publish helpers keep only the active local
generation and its manifest; the bucket keeps every generation's manifest, so
rollback does not need the pruned local copy. `deploy/test-r2-sync.sh [COUNT]`
(`make test-sync`) checks pagination and idempotency against the local
simulation only. `make test-r2` checks the multipart SigV4 canonicalization,
listing past one page, conditional promotion, and generation pruning against
local fakes, and `make worker-test` builds the actual Worker bundle and runs
it under workerd (Miniflare) for cold-HEAD cache, root-link, and pinned
artifact-alias coverage. `make test-r2-live`
(`R2_ACCEPTANCE_BUCKET=...` plus `R2_S3_*`) is the opt-in real-R2 multipart
acceptance check; it is not run by CI.

### Fast path credentials

The fast upload path uses R2's S3 API with:

```sh
export R2_S3_ENDPOINT="https://<ACCOUNT_ID>.r2.cloudflarestorage.com"
export R2_S3_ACCESS_KEY_ID="<ACCESS_KEY_ID>"
export R2_S3_SECRET_ACCESS_KEY="<SECRET_ACCESS_KEY>"
```

When those are absent, `r2-sync.sh` mints short-lived credentials for the
bucket through the authenticated `cf` CLI. To do it by hand:

```sh
eval "$(python3 deploy/r2-temp-credentials.py stuff-mycrates)"
```

That helper selects the first active account token whose policies mention R2,
calls `cf r2 temporary-credentials create` with `object-read-write` scoped to
the bucket, and exports the four variables the sync needs (the three above
plus `R2_S3_SESSION_TOKEN`). The credentials are never written to disk.
`R2_PARENT_ACCESS_KEY_ID` picks a specific parent token, `R2_ACCOUNT_ID`
overrides the account used for the endpoint, and `R2_TEMP_CREDENTIALS_TTL`
sets the lifetime in seconds (default 3600).

The equivalent manual command, using the parent token ID from
`cf accounts tokens list`, is:

```sh
cf r2 temporary-credentials create \
  --parent-access-key-id <R2_TOKEN_ID> \
  --bucket stuff-mycrates \
  --permission object-read-write \
  --ttl-seconds 3600
```

Repeat for `stuff-mynpm` and `stuff-mypypi`; the helper does this
automatically for each registry sync. `make deploy` uses the fast path by
default and runs the three registry republishes in parallel. Set
`DEPLOY_JOBS=1` to serialize them, or export `R2_S3_*` to use long-lived
credentials instead of minting.

## Adopt a live publication

`deploy/publish` runs this automatically until the mycrates state carries an
`.adopt-complete` marker. The manual sequence is below.

When `$STUFF_STATE_HOME` is missing (new machine, lost disk) the next publish
would replace the live registry with the caller's subset. Rebuild the state
from the buckets first. Pulling only reads the bucket; each object is checked
against the listing ETag and only the current generation plus the shared
stores are fetched. With `R2_S3_*` (or credentials minted through the
authenticated `cf` CLI) the pull lists through the S3 API and downloads
`R2_PULL_JOBS` objects in parallel; set `R2_PULL_CF_ONLY=1` to force the
paced `cf` transport:

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

A manual mycrates adoption leaves `.adopt-complete` absent, so the next
`deploy/publish mycrates` repeats the idempotent adopt in place and writes the
marker once it verifies. That is safe: existing local records are kept.

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

It runs `stuff doctor --sample-artifacts` plus `npm view` and
`uv pip install --dry-run` probes against the deployed origins.

## License

Apache-2.0 for stuff files; imported crates keep their own license fields.
