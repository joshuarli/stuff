# Publish

`stuff publish cargo|npm|pypi|oci` is the normal entry point. `stuff publish --help` is authoritative for its grammar, options, and exit codes. A publish reads the live generation and manifest, prepares the requested packages against them, validates the complete result, uploads only missing objects, promotes `current` with a conditional write, reads back, and reports; a request whose bytes are already published reports `unchanged` and promotes nothing.

## Check prerequisites

| Requirement | Use | Check |
| --- | --- | --- |
| `cf` signed in | List and upload R2 objects, promote `current` | `cf auth whoami` |
| Publication state | Extend the live registry instead of replacing it | A publish reads the live generation; local state is optional |
| Python 3.11+ | Run the kept R2 maintenance scripts and the `mycrates` oracle | `python3 --version` |
| Rust nightly | `cargo publish` for crates | `cargo --version` |
| Node and npm | `mynpm` fetches npm packages | `node --version` |
| uv and CPython 3.15 | `mypypi` resolves and builds wheels | `uv --version` |
| `auditwheel` or `delocate` | `mypypi` repairs native wheels | `deploy/provision-mypypi-repair.sh` installs the pin |
| Network | Fetch crates, npm tarballs, and Python wheels | |
| R2 S3 token (optional) | Long-lived credentials, conditional promotion, and multipart uploads over 300 MB | `R2_S3_*` is set, or `cf` mints temporary credentials |
| OCI writer host (optional) | Large anonymous `docker push`; reads/writes `stuff-oci-blobs` | `deploy/myoci-writer.sh` runs with the shared secret and R2 credentials |

`stuff` mints R2 credentials through the `cf` profile named by `cf_profile` in its configuration (default `stuff`) and passes `--profile` on every `cf` call, so `make publish-*` works from any directory without a directory binding. Create the profile once with `cf auth create stuff`; check it with `cf --profile stuff auth whoami`. The kept maintenance scripts (`r2-pull.py`, `r2-gc.py`) mint through `stuff credentials`, so they use the same profile.

`stuff` keeps configuration (`cf_profile` and the per-registry `bucket` and `public_base_url`) in `${XDG_CONFIG_HOME:-$HOME/.config}/stuff/config.json`, a disposable cache in `${XDG_CACHE_HOME:-$HOME/.cache}/stuff/<registry>`, and durable state in `${XDG_STATE_HOME:-$HOME/.local/state}/stuff/<registry>`; `STUFF_CONFIG_HOME`, `STUFF_CACHE_HOME`, and `STUFF_STATE_HOME` override each. The kept maintenance scripts use the same state root. Back up the state directories. The live generation, not local state, is the publication source of truth.

## Publish a registry

Run `stuff publish` from the repository that owns the packages:

```sh
stuff publish cargo -p crate-a -p crate-b

stuff publish npm --lockfile pnpm-lock.yaml --project my-project

stuff publish npm --tarball joshuarli-ui-0.1.0.tgz

stuff publish pypi six==1.16.0
```

`--dry-run` reports the planned change without uploading or promoting. `stuff` prepares the change against the live generation, so a machine with no local state publishes safely: it never rebuilds the published set from a local subset, and a non-additive or byte-conflicting request exits 3 instead of overwriting a competing publisher.

A publish holds the registry's lock, a `flock` on `$STUFF_STATE_HOME/<registry>/publish.lock`. `stuff` and the kept maintenance scripts (`make gc`, `deploy/r2-gc-blobs.py`, and `mypypi-container.sh`) take the same lock. A failure stops the run before any R2 write.

For `mypypi`, Linux wheels use `deploy/mypypi-container.sh` for build and validation (see [crates/mypypi/README.md](../crates/mypypi/README.md)).

To ingest a whole fleet locally, `deploy/fleet-ingest.sh OUT_DIR --cargo LIST --npm LIST --pypi LIST` (`make fleet-ingest`) runs each registry's import, fetch, verify or validate, and publish into `OUT_DIR/{mycrates,mynpm,mypypi}/site`. Lists hold Cargo.lock paths, pnpm-lock.yaml paths, and `name==version` requirements. It never writes to R2; publish through `stuff publish`.

## Publish a first-party npm package

`stuff publish npm --tarball PATH.tgz` hosts a package that exists nowhere upstream beside the mirror. Pack it first with `npm pack` or `pnpm pack` (`--tarball` may repeat), and run the pack's `prepack` build yourself; `stuff` reads the tarball and never runs package scripts. `--tarball` and `--lockfile`/`--project` are separate modes and do not combine.

```sh
(cd packages/ui && npm pack --pack-destination /tmp/packs)
stuff publish npm --tarball /tmp/packs/joshuarli-ui-0.1.0.tgz --dry-run
stuff publish npm --tarball /tmp/packs/joshuarli-ui-0.1.0.tgz
```

The publish extends the live generation exactly as a lockfile publish does, so every mirrored package and version stays. The tarball is stored in the shared tarball store and served at `current/-/tarballs/<sha512>.tgz`; the packument carries `dist.integrity` (sha512), `dist.shasum`, `dist-tags.latest` (the newest publish), `time`, and these `package.json` fields: `description`, `license`, `type`, `main`, `module`, `types`, `exports`, `sideEffects`, `bin`, `engines`, `os`, `cpu`, `libc`, and the dependency maps (`dependencies`, `optionalDependencies`, `peerDependencies`, `peerDependenciesMeta`, `bundleDependencies`).

Rules, all exit 3 on violation:

- A published `name@version` is immutable. The same bytes report `unchanged`; different bytes conflict.
- A first-party name cannot reuse a name the mirror hosts (any mirrored version or metadata-only stub).
- A later lockfile publish cannot mirror another version of a first-party name from upstream.

First-party records live in the generation manifest as artifacts marked `"firstParty": true`, with no local state of their own, so later mirror publishes and `stuff republish npm` carry them forward and a machine with no state sees them. Consumers install by version:

```ini
@joshuarli:registry=https://npm.17166969.xyz/current/
```

A lockfile that names a first-party `name@version` resolves from the generation, not from npmjs.

## Publish the OCI registry

The protocol, storage, and activation design is in
[oci-design.md](oci-design.md).

`stuff publish oci` imports every image into its state, renders one OCI
generation, syncs blobs before metadata, and flips `current`. The metadata
bucket is `stuff-oci`; blob bytes live in `stuff-oci-blobs` under
`sha256/<hex>`. `oci.bucket` and `oci.blobs_bucket` in
`$XDG_CONFIG_HOME/stuff/config.json` override both names.

```sh
stuff publish oci --image example/app=ghcr.io/example/app:v1

stuff publish oci --layout tests/fixtures/oci/layout --repository example/app \
  --tag stable

stuff republish oci
```

An empty `stuff-oci` bucket makes the first publication.

`--image` imports through pinned `oras`; `--layout` imports a local OCI image
layout. Import verifies every digest and size, walks indexes and referrers,
and rejects sparse indexes, missing blobs, foreign layers, and schema 1. A tag
move or push creates a new content-derived generation; a rollback is
`stuff rollback oci <generation>`. With no local state, a publish rebuilds what
it needs from the published generation.

Blob garbage collection is separate from generation GC:
`deploy/r2-gc-blobs.py [--dry-run]` unions every retained generation's
`blobs.json` and deletes unreferenced `sha256/*` objects. `myoci verify`
exits 3 on any digest, size, tag, or referrer mismatch.

The registry is sha256-only. Limits: repository name 255 characters, tag 128
characters, manifest 4 MiB, upload session one hour, one upload 20 GiB.
Reads are anonymous. Writes require `Authorization: Basic` with the registry
push token; delete endpoints are not implemented.

### Push writer

Production does not run a writer. The Worker has no `STUFF_OCI_PUSH_TOKEN`, `STUFF_OCI_PUSH_SECRET`, or `STUFF_OCI_WRITER_ORIGIN`, so every write returns the OCI `405 UNSUPPORTED` error, and images are published with `stuff publish oci`. Enabling network push means starting the writer below on a TLS host and setting those three on the Worker.

The front (`myoci serve --push`) owns authentication, manifest validation,
generation creation, and the `current` flip. A separate `myoci writer` owns
only blob bytes: it accepts upload `PATCH`/`PUT`s on an HMAC-signed,
repo-bound, one-hour `Location`, streams and verifies the digest, then writes
`sha256/<hex>` to the blob bucket through `deploy/r2-put.py` and
`deploy/r2-multipart.py`. The writer never sees registry credentials.

```sh
MYOCI_WRITER_SECRET_FILE=/run/secrets/oci-push-secret \
  MYOCI_WRITER_BUCKET=stuff-oci-blobs deploy/myoci-writer.sh

target/debug/myoci serve --site-dir SITE --state-dir STATE --push \
  --writer-url https://oci-writer.example \
  --push-token-file /run/secrets/oci-push-token \
  --push-secret-file /run/secrets/oci-push-secret \
  --bind 0.0.0.0:5000
```

Worker bindings: `OCI` and `OCI_BLOBS`, `STUFF_OCI_HOST`,
`STUFF_OCI_PUSH_TOKEN`, `STUFF_OCI_PUSH_SECRET`, and
`STUFF_OCI_WRITER_ORIGIN`. `STUFF_OCI_BLOB_ORIGIN` is set in `cloudflare.config.ts` to
`https://oci-blobs.17166969.xyz`, so blob GET/HEAD returns a `307` to
`<origin>/sha256/<hex>`; the `oci-blobs` bucket domain needs the Cache Rule
for `/sha256/*`. Layers over 300 MB upload through S3 multipart. The writer
host needs `R2_S3_*` for `stuff-oci-blobs`; mint them with:

```sh
eval "$(stuff credentials stuff-oci-blobs)"
```

Offline proofs: `make test-oci-pull`, `make test-oci-push`,
`make test-oci-publish`, `make test-oci-conformance`,
`make test-oci-conformance-push`, `make oci-bench`, and
`R2_S3_* make test-r2-multipart`.

## Republish without new packages

A renderer or catalog change reaches a registry's pages only when that registry publishes again. `stuff republish cargo|npm|pypi|oci` does that without package inputs: it reads the live generation, renders it again under the current renderer, and promotes it. Unchanged output reports no new generation. The bucket keeps older manifests until `make gc` prunes generations outside its retention window.

```sh
stuff republish cargo
stuff republish npm
stuff republish pypi
```

`make deploy` runs the whole rollout in order. It builds one Worker bundle, tests it, and deploys that bundle with `cf deploy --prebuilt`. It then runs the three republishes in parallel (and `republish-oci` with `OCI_DEPLOY=1`), then `make smoke-deployed`. Each publish mints its own temporary S3 credentials through `cf` when `R2_S3_*` is absent. Set `DEPLOY_JOBS=1` to serialize them. A failure stops the rollout before the live check.

## Shared stores

Generation-scoped aliases (`wheels/`, `tarballs/`, `-/tarballs/`, listed in `deploy/shared-store-paths.txt`) are not uploaded. The Worker resolves them to the shared store, including direct `generations/<id>/...` requests. Files over the 300 MB single-PUT cap upload through S3 multipart and are skipped when the remote multipart ETag already matches.

## Upload and promote with S3

Promotion is a conditional write, so publishing needs these credentials. `stuff` uses `R2_S3_*` when the three variables are set, and otherwise mints short-lived credentials through `cf` before it writes; it fails before writing if minting fails. Minting needs a logged-in `cf_profile` profile (`cf auth create stuff`, check with `cf --profile stuff auth whoami`) and an active account token whose policies mention R2. The working directory does not matter.

To mint credentials by hand, for the bucket you will write (`stuff-mycrates`, `stuff-mynpm`, or `stuff-mypypi`):

```sh
eval "$(stuff credentials stuff-mycrates)"
```

The variables exist only in that shell and expire after an hour, so mint again when a long session outlives them. If minting fails, the message names the failing `cf` call. Typical causes are a missing login, no token that mentions R2, or an account `cf auth whoami` cannot resolve. Set `R2_PARENT_ACCESS_KEY_ID` or `R2_ACCOUNT_ID` to pick them explicitly.

`stuff credentials` tries active account tokens whose policies mention R2 first and calls `cf r2 temporary-credentials create` with `object-read-write` scoped to the bucket. It prints exports for `R2_S3_ENDPOINT`, `R2_S3_ACCESS_KEY_ID`, `R2_S3_SECRET_ACCESS_KEY`, and `R2_S3_SESSION_TOKEN`, and never writes them to disk. `R2_PARENT_ACCESS_KEY_ID` picks a specific parent token, and `R2_ACCOUNT_ID` overrides the account used for the endpoint. `R2_TEMP_CREDENTIALS_TTL` sets the lifetime in seconds (default 3600).

To use long-lived credentials instead, export them before publishing:

```sh
export R2_S3_ENDPOINT="https://<ACCOUNT_ID>.r2.cloudflarestorage.com"
export R2_S3_ACCESS_KEY_ID="<ACCESS_KEY_ID>"
export R2_S3_SECRET_ACCESS_KEY="<SECRET_ACCESS_KEY>"
```

`R2_S3_REGION` defaults to `auto`. Long-lived tokens should allow object read and write on the three registry buckets and nothing else.

A publish reads `current` immediately before promoting and writes it with `If-Match` on the ETag it just read, or `If-None-Match: *` on a first publication, so a publisher that moves `current` during verification gets `412`. `stuff` re-reads the live snapshot and re-plans the additive change up to three times; a non-additive request exits 3 and never falls back to a plain put. The publish lock is local to one machine, and nothing fences two machines publishing the same registry at the same moment; publish each registry from one machine at a time.

## Publish from a new machine

A machine with no local state is safe to publish from: `stuff` reads the live generation and manifest, verifies each object it uses against its listing size and ETag, and prepares the change against that snapshot. A failure stops before any R2 write, and the published set is never rebuilt from a local subset.

`deploy/r2-pull.py BUCKET DEST` only reads the bucket. It fetches the current generation and the shared stores and checks each object against its listing ETag, using the multipart ETag algorithm for objects uploaded in parts. After the download it checks every entry of the generation's manifest (key, size, ETag) against the pulled tree; a generation published before manifests existed is pulled without that check. With `R2_S3_*`, or with credentials minted through `cf`, it lists through the S3 API and downloads `R2_PULL_JOBS` objects in parallel. `R2_PULL_CF_ONLY=1` forces the paced `cf` transport.

The Python `mycrates` adoption sequence (`mycrates init`, `adopt`, and `verify`) belongs to the legacy oracle and its golden tests; it is not a publishing entry point.

## Upgrading mycrates state from the Rust tool

The Rust `mycrates` stored native records and pins as TOML; the Python tool stores JSON and refuses a state directory with TOML records left in it. Once, per state directory, with a backup in hand (`cp -a "$H/mycrates" "$H/mycrates.bak"`), run:

```sh
crates/mycrates/bin/mycrates migrate-state --state-dir "$H/mycrates/state"
```

`stuff publish cargo` does not run this migration; it reads its own state directory. The migrated state republishes the generation the live site already serves: check it by publishing into a scratch site (`crates/mycrates/bin/mycrates publish --state-dir "$H/mycrates/state" --site-dir "$H/mycrates/check"`) and comparing the `current` link there with the live `current` marker.

## mycrates auth

This describes the legacy Python write path, kept as the parity oracle. `mycrates` writes require the publish token, whether `--token-file` supplies it or you set `MYCRATES_TOKEN`. Reads follow `auth_required` in `config.json`, and the generated `current/registry/config.json` advertises the same value as `auth-required`.

Keep `auth_required` false for the public Worker and static servers, which do not enforce read auth. The publish server serves `blobs/` for Cargo verification, so a dependent native crate publishes without `--no-verify`.

## Deploy the Worker

```sh
cd crates/stuff-worker
pnpm install --frozen-lockfile
pnpm run build
cf deploy --prebuilt --dry-run
cf deploy --prebuilt
```

`pnpm run build` writes the Cloudflare Build Output Specification under `.cloudflare/output/v0`, and `cf deploy --prebuilt` uploads it without rebuilding (see [crates/stuff-worker/README.md](../crates/stuff-worker/README.md)). The three buckets already exist; use `--provision` only when you add one. Custom domains come from `domains` in `cloudflare.config.ts`.

The Worker uses Workers Caching. The default entrypoint is an uncached gateway. The `Registry` entrypoint caches responses under a key that includes the host, so the four domains cannot share cache entries. Immutable artifacts carry a one-year TTL. Mutable generation views carry 60 seconds, so a promotion becomes visible within that window without a cache purge. The gateway costs one extra worker-to-worker request per request; a cache hit skips `Registry` but still runs the gateway. Two requests to the same URL should show `cf-cache-status: HIT` on the second. Range requests are sliced from the cached full object, so a cold range fetch downloads the whole artifact.

## Edge protection

The registry hosts serve package clients, not browsers, so they must never receive an
interactive challenge. Keep these zone settings for `17166969.xyz`:

- Bot Fight Mode and Browser Integrity Check off.
- Security Level `medium`, with the Cloudflare managed WAF ruleset enabled.
- The `/ingest/` rate limit at 20 requests per 10 seconds per source IP and colo; do not
  rate-limit or challenge the read paths.
- No Managed Challenge, JS Challenge, or Turnstile rule on `crates.`, `npm.`, `pypi.`,
  `oci.`, or `stuff.`.

A challenge breaks `cargo` with `403` and a `Just a moment...` body from
`https://crates.17166969.xyz/current/crates-io/config.json`. Verify a fetch returns `200`:

```sh
curl -fsS -o /dev/null -w '%{http_code}\n' \
  https://crates.17166969.xyz/current/crates-io/config.json
```

## Verify the publication

```sh
make smoke-deployed
```

This builds the `stuff` binary and runs `deploy/deployed-smoke.sh`. The check is read-only and needs no Cloudflare credentials. It runs `stuff doctor --sample-artifacts` against the active generations, catalog objects, and sample artifact hashes, plus `npm view`, `uv pip install --dry-run`, and `oras manifest fetch` probes.

## Roll back a generation

```sh
stuff rollback cargo <generation-id>
```

`stuff rollback` verifies every object in the target generation's manifest before moving `current`, and records the target as this machine's promoted generation. `--dry-run` performs the verification without promoting. To roll forward, run the same command with the newer generation id; gc keeps generations newer than `current` by default, so the target survives.

## Reclaim storage

`make gc` deletes R2 generations outside its retention window and shared-store objects no retained manifest references, then prints per-bucket and total storage before and after. It keeps `current` plus `GC_KEEP_PREVIOUS` older generations (default 1) and `GC_KEEP_NEWER` newer generations (default `all`). Preview with `GC_DRY_RUN=1`, and restrict buckets with `GC_BUCKETS`.

The command mints bucket-scoped credentials per bucket, so the usual `cf` login is enough. Because manifests are deleted with their generations, rollback and roll-forward only reach retained generations.

gc holds each bucket's publish lock while it plans and deletes, so it waits for a publish on the same machine and a publish waits for it. It deletes manifests first, then generation trees, then shared objects, and before every batch it re-reads `current` and the manifest set and stops if either changed. It also defers shared-store deletion while a recent generation directory has no manifest.

## Fix common failures

| Message | Cause | Fix |
| --- | --- | --- |
| `cf is not authenticated` | No active cf session | Run `cf auth login --use-keyring` |
| `cannot determine the R2 account` or `no active account token can mint R2 credentials` | `stuff` could not mint R2 credentials through `cf` | Run `cf auth create stuff` (or the configured `cf_profile`), or export `R2_S3_*` |
| `current changed during promotion` | Another publisher moved `current` | Pull the new generation, merge your changes, and publish again |
| `immutable object differs` | A local immutable file differs from R2 | Find which side is wrong; immutable objects are never overwritten |
| `exceeds the 300 MB single-PUT cap` | Large object without S3 credentials | Set `R2_S3_*` |
| `differs from its manifest ETag` | A pulled object does not match the generation's manifest | Rerun the pull; if it repeats, the bucket is inconsistent |
| `publish: waiting for active writer` | Another publish, rollback, or gc holds the lock | Wait for it to finish |
| `remote current is '...', expected '...'; refusing to overwrite a competing publication` | Another machine published, or rolled back, since this machine's last promotion | Publish again; `stuff` prepares the change against the new live generation |
| `generation ... drops N versions served by live generation ...` | The new catalog lacks versions the live catalog serves, usually because this machine's state predates another publisher's | Re-adopt the live publication, or pass `--allow-drops` to remove them on purpose |
