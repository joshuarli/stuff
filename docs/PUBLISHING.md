# Publish

`deploy/publish REGISTRY` is the normal entry point. It checks `cf` auth, adopts the live publication when local state is missing, and runs the matching publish helper. Promotion defaults to `s3`, the conditional write, and mints `R2_S3_*` from `cf`; `--promote=cf` is the plain-put opt-out.

## Check prerequisites

| Requirement | Use | Check |
| --- | --- | --- |
| `cf` signed in | List and upload R2 objects, promote `current` | `cf auth whoami` |
| Publication state | Extend the live registry instead of replacing it | The wrapper adopts if missing |
| Python 3.11+ | Run `mycrates` and the R2 scripts | `python3 --version` |
| Rust nightly | `cargo publish` for crates, and the `mynpm`, `mypypi`, and `myoci` binaries | `cargo --version` |
| Node and npm | `mynpm` fetches npm packages | `node --version` |
| uv and CPython 3.15 | `mypypi` resolves and builds wheels | `uv --version` |
| `auditwheel` or `delocate` | `mypypi` repairs native wheels | `deploy/provision-mypypi-repair.sh` installs the pin |
| Network | Fetch crates, npm tarballs, and Python wheels | |
| R2 S3 token (optional) | Long-lived credentials, conditional promotion, and multipart uploads over 300 MB | `R2_S3_*` is set, or `cf` mints temporary credentials |
| OCI writer host (optional) | Large anonymous `docker push`; reads/writes `stuff-oci-blobs` | `deploy/myoci-writer.sh` runs with the shared secret and R2 credentials |

If `cf auth whoami` reports no active session, run `cf auth login --use-keyring`. To keep publishing credentials separate, create a profile and bind it to the source repository with `cf auth activate stuff .`.

The helpers keep state in `$STUFF_STATE_HOME/<registry>`, default `${XDG_STATE_HOME:-$HOME/.local/state}/stuff`, not in the calling checkout. Back it up. This directory is the publication source of truth.

## Publish a registry

Run the wrapper from the repository that owns the packages:

```sh
CRATES="crate-a crate-b" /path/to/stuff/deploy/publish mycrates

LOCKFILE=pnpm-lock.yaml PROJECT=my-project /path/to/stuff/deploy/publish mynpm

PACKAGES="six==1.16.0" /path/to/stuff/deploy/publish mypypi
```

The wrapper sets `STUFF_DIR` to its own checkout unless it is already set, so the helper builds the right binaries. Before it publishes, the wrapper:

- verifies `cf auth whoami`;
- adopts the live bucket: for mycrates until the state carries an `.adopt-complete` marker, and again whenever the live generation is not the one this machine last promoted or adopted (the adopt extends the state in place, so another machine's records join this machine's); for mynpm and mypypi when the site has no `current`;
- selects `s3` promotion, the conditional write, unless you pass `--promote=cf`. It mints `R2_S3_*` from `cf` when they are unset and stops before building if it cannot.

The helper then holds the registry's publish lock, builds the site, uploads it, and promotes it. The lock is a `flock` on `$STUFF_STATE_HOME/<registry>/publish.lock`, and every writer of that registry's bucket on the machine takes it: the helpers, adoption, `make sync`, `r2-rollback.sh`, and `make gc`. `deploy/with-publish-lock.sh BUCKET COMMAND...` runs any other command under it. A source or build failure stops the run before any R2 write. The mycrates helper compiles nothing itself: one loopback `mycrates serve-publish` on `PUBLISH_PORT` (default 8080) takes the Cargo writes and serves the index and blobs while `cargo publish` runs. It restores the public base URL and rebuilds the site from its exit trap, so a failure cannot leave `http://127.0.0.1:8080` in the site for a later manual `r2-sync.sh` to promote.

For `mynpm`, `TARGETS` adds platform-specific lockfile entries. For `mypypi`, Linux wheels use `deploy/mypypi-container.sh` for build, validate, and publish (see [crates/mypypi/README.md](../crates/mypypi/README.md)).

To ingest a whole fleet locally, `deploy/fleet-ingest.sh OUT_DIR --cargo LIST --npm LIST --pypi LIST` (`make fleet-ingest`) runs each registry's import, fetch, verify or validate, and publish into `OUT_DIR/{mycrates,mynpm,mypypi}/site`. Lists hold Cargo.lock paths, pnpm-lock.yaml paths, and `name==version` requirements. It never writes to R2; sync each site afterwards (`make sync LOCAL=1` to rehearse).

## Publish the OCI registry

The protocol, storage, and activation design is in
[oci-design.md](oci-design.md).

`deploy/publish oci` imports every image into `$STUFF_STATE_HOME/oci/state`,
renders one OCI generation, syncs blobs before metadata, and flips `current`.
The metadata bucket is `stuff-oci`; blob bytes live in `stuff-oci-blobs`
under `sha256/<hex>`. `BUCKET_META` and `BUCKET_BLOBS` override both names.

```sh
OCI_IMAGES="example/app=ghcr.io/example/app:v1" deploy/publish oci

OCI_LAYOUT=tests/fixtures/oci/layout OCI_REPOSITORY=example/app \
  OCI_TAG=stable deploy/publish oci

deploy/publish oci --republish
```

An empty `stuff-oci` bucket makes the first publication: the wrapper skips
adoption and builds from the inputs, and `--republish` refuses with nothing to
republish.

`OCI_IMAGES` imports through pinned `oras`; `OCI_LAYOUT` imports a local OCI
image layout. Import verifies every digest and size, walks indexes and
referrers, and rejects sparse indexes, missing blobs, foreign layers, and
schema 1. A tag move or push creates a new content-derived generation; a
rollback is `deploy/r2-rollback.sh stuff-oci <generation>`. Adoption rebuilds
local state from the published generation through `r2-pull.py`.

Blob garbage collection is separate from generation GC:
`deploy/r2-gc-blobs.py [--dry-run]` unions every retained generation's
`blobs.json` and deletes unreferenced `sha256/*` objects. `myoci verify`
exits 3 on any digest, size, tag, or referrer mismatch.

The registry is sha256-only. Limits: repository name 255 characters, tag 128
characters, manifest 4 MiB, upload session one hour, one upload 20 GiB.
Reads are anonymous. Writes require `Authorization: Basic` with the registry
push token; delete endpoints are not implemented.

### Push writer

Production does not run a writer. The Worker has no `STUFF_OCI_PUSH_TOKEN`, `STUFF_OCI_PUSH_SECRET`, or `STUFF_OCI_WRITER_ORIGIN`, so every write returns the OCI `405 UNSUPPORTED` error, and images are published with `deploy/publish oci`. Enabling network push means starting the writer below on a TLS host and setting those three on the Worker.

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
for `/sha256/*`. Layers over 300 MB upload through S3 multipart;
`deploy/r2-sync-blobs.sh` mints `R2_S3_*` for `stuff-oci-blobs` when they are
unset. To mint them by hand:

```sh
eval "$(python3 deploy/r2-temp-credentials.py stuff-oci-blobs)"
```

Offline proofs: `make test-oci-pull`, `make test-oci-push`,
`make test-oci-publish`, `make test-oci-conformance`,
`make test-oci-conformance-push`, `make oci-bench`, and
`R2_S3_* make test-r2-multipart`.

## Republish without new packages

A renderer or catalog change reaches a registry's pages only when that registry publishes again. `--republish` does that without package inputs. It adopts the live publication if needed, renders it again under the current renderer, and promotes it:

```sh
deploy/publish mycrates --republish
deploy/publish mynpm --republish
deploy/publish mypypi --republish
```

`mycrates` rebuilds its site from state, `mynpm` runs `mynpm adopt` on the site, and `mypypi` runs `mypypi republish`, which refuses an index with nothing published. Unchanged output reports no new generation. Each helper prunes local generations other than the active one: mycrates before its sync, mynpm and mypypi after it. The bucket keeps older manifests until `make gc` prunes generations outside its retention window.

`make deploy` runs the whole rollout in order. It builds one Worker bundle, tests it, and deploys that bundle with `cf deploy --prebuilt`. It then runs the three republishes in parallel, then `make smoke-deployed`. Each republish mints its own temporary S3 credentials when `R2_S3_*` is absent. Set `DEPLOY_JOBS=1` to serialize them. A failed mint falls back to the paced `cf` API. A failure stops the rollout before the live check.

## Sync a site you already built

Use `deploy/r2-sync.sh SITE_DIR BUCKET [--expected GEN] [--repair] [--dry-run] [--promote=cf|s3] [--allow-drops]` when the site is already on disk:

```sh
H=${STUFF_STATE_HOME:-${XDG_STATE_HOME:-$HOME/.local/state}/stuff}

deploy/r2-sync.sh "$H/mycrates/site" stuff-mycrates
deploy/r2-sync.sh "$H/mynpm/site/mirror" stuff-mynpm
deploy/r2-sync.sh "$H/mypypi/site" stuff-mypypi
```

`r2-sync.sh` requires `SITE_DIR/current` to be a symlink, so for `mynpm` pass `site/mirror`, not `site`. It translates `current` into a marker object holding the generation id. The sync is incremental and content-aware: remote objects whose size and content hash match are skipped, so re-syncing an unchanged generation uploads nothing. It writes `manifests/<id>.json` for each local generation and uploads it after every object it lists, so a generation directory without a manifest is always a sync in flight. It then verifies every listed object remotely and replaces `current`.

After each promotion the sync records the generation in `$STUFF_STATE_HOME/<registry>/promoted-generation`. Adoption and rollback write the same record. The next sync expects the bucket to still be on that generation and refuses to promote otherwise, so a site built from state that another publisher has since superseded cannot replace the live generation. Without a record, the first sync promotes unconditionally and writes one.

- `--dry-run` lists the bucket read-only and prints the object and byte counts it would upload. It writes manifests into the site, needs no S3 credentials, and stops before any upload or promotion.
- Before uploading, `deploy/r2-catalog-guard.py` compares the new generation's `catalog.json` with the live one and refuses the sync when any (source, package, version) the live catalog serves is missing from the new one. The recorded base generation only proves the bucket has not moved; the guard also catches state that never held another machine's records. `--allow-drops` (or `R2_ALLOW_DROPS=1`, which `deploy/publish --allow-drops` sets) is the explicit way to remove versions. Rollback does not run the guard. A live generation without a `catalog.json` is refused the same way.
- `--expected <generation>` requires the bucket to be on that generation before promotion, in place of the recorded one.
- An existing immutable object with different bytes is a conflict. `--repair` is the separate, explicit overwrite path.

Generation-scoped aliases (`wheels/`, `tarballs/`, `-/tarballs/`, listed in `deploy/shared-store-paths.txt`) are not uploaded. The Worker resolves them to the shared store, including direct `generations/<id>/...` requests. Files over the 300 MB single-PUT cap upload through S3 multipart (`deploy/r2-multipart.py`, needs `R2_S3_*`). They are skipped when the remote multipart ETag already matches.

## Upload and promote with S3

The `cf` API allows roughly four requests a second per account. A republish after a catalog change uploads a new generation's whole index tree, so listing and uploading through `cf` costs several minutes even though the files are small.

Before a sync, `r2-sync.sh` mints short-lived S3 credentials from the authenticated `cf` CLI. Listing and uploads then use the S3 API: the remote plan comes from one paginated list, uploads run `R2_PUT_JOBS` in parallel (default 8), and the generation is verified against that list. If minting fails, listing through `cf` (`deploy/r2-plan.py`) and a cross-process pacer remain the fallback. The local simulation (`CF_LOCAL_STATE`) stays single-threaded.

Promotion defaults to `s3`, so a publish, sync, or rollback needs these credentials. When `R2_S3_*` is unset the scripts mint them themselves, and a publish stops before building if minting fails. Minting needs an authenticated `cf` (`cf auth login --use-keyring`, check with `cf auth whoami`) and an active account token whose policies mention R2.

To mint credentials by hand, for the bucket you will write (`stuff-mycrates`, `stuff-mynpm`, or `stuff-mypypi`):

```sh
eval "$(python3 deploy/r2-temp-credentials.py stuff-mycrates)"
```

The variables exist only in that shell and expire after an hour, so mint again when a long session outlives them. If minting fails, the message names the failing `cf` call. Typical causes are a missing login, no token that mentions R2, or an account `cf auth whoami` cannot resolve. Set `R2_PARENT_ACCESS_KEY_ID` or `R2_ACCOUNT_ID` to pick them explicitly.

The helper picks the first active account token whose policies mention R2 and calls `cf r2 temporary-credentials create` with `object-read-write` scoped to the bucket. It exports `R2_S3_ENDPOINT`, `R2_S3_ACCESS_KEY_ID`, `R2_S3_SECRET_ACCESS_KEY`, and `R2_S3_SESSION_TOKEN`, and never writes them to disk. `R2_PARENT_ACCESS_KEY_ID` picks a specific parent token, and `R2_ACCOUNT_ID` overrides the account used for the endpoint. `R2_TEMP_CREDENTIALS_TTL` sets the lifetime in seconds (default 3600).

To use long-lived credentials instead, export them before publishing:

```sh
export R2_S3_ENDPOINT="https://<ACCOUNT_ID>.r2.cloudflarestorage.com"
export R2_S3_ACCESS_KEY_ID="<ACCESS_KEY_ID>"
export R2_S3_SECRET_ACCESS_KEY="<SECRET_ACCESS_KEY>"
```

`R2_S3_REGION` defaults to `auto`. Long-lived tokens should allow object read and write on the three registry buckets and nothing else.

Both promotion modes check the recorded generation first. `--promote=s3`, the default, writes `current` with `If-Match` on the ETag read just before the write, or `If-None-Match: *` on first publish, so a publisher that moves `current` during verification gets `412`. `--promote=cf` is the explicit opt-out: it writes `current` with a plain put and does not detect a competing publisher. The publish lock is local to one machine, and nothing fences two machines publishing the same registry at the same moment; publish each registry from one machine at a time.

```sh
CRATES="crate-a" /path/to/stuff/deploy/publish mycrates --promote=cf
```

## Publish from a new machine

When `$STUFF_STATE_HOME` is missing (new machine, lost disk), a plain publish would replace the live registry with the caller's subset. The wrapper prevents that by adopting first: it pulls the current generation and the shared stores, then rebuilds the local publication state before running the helper.

`deploy/r2-pull.py BUCKET DEST` only reads the bucket. It fetches the current generation and the shared stores and checks each object against its listing ETag, using the multipart ETag algorithm for objects uploaded in parts. After the download it checks every entry of the generation's manifest (key, size, ETag) against the pulled tree; a generation published before manifests existed is pulled without that check. With `R2_S3_*`, or with credentials minted through `cf`, it lists through the S3 API and downloads `R2_PULL_JOBS` objects in parallel. `R2_PULL_CF_ONLY=1` forces the paced `cf` transport.

For mycrates with an absent or empty state, adoption runs `mycrates init`, `mycrates adopt`, and `mycrates verify` in a sibling staging directory next to the state. It writes the marker there and renames the whole state into place, so an interrupted run leaves no live state behind and the next publish retries. An existing state without the marker is never replaced. The wrapper runs the idempotent adopt into it, verifies it, and only then writes the marker. Local records that are not on the live site stay in place.

For mynpm, adoption pulls into a sibling staging directory, runs `mynpm adopt` there, and renames the result into the live site only after it succeeds. mypypi needs no step beyond the pull, so its pulled site is renamed into place as fetched. A failed adoption leaves no live site behind. The wrapper prints the staging path and the `rm -rf` command that removes it. After inspecting it, publish again to start a fresh adoption.

If the wrapper refuses because a site directory exists without `current`, do not delete it. Inspect it, or restore the state backup, before publishing. A publish against an empty or partial state would replace the live registry with only the local packages.

The manual sequence the wrapper automates:

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

`mycrates` publishes from `state/` alone, so `adopt` rebuilds its records from the pulled index lines and blobs. `mynpm` and `mypypi` extend the `current` generation of their site, so the pulled site is their state. Delete `$H/pull-mycrates` once adopted. A manual mycrates adoption leaves `.adopt-complete` absent, so the next `deploy/publish mycrates` repeats the idempotent adopt in place and writes the marker once it verifies.

## Upgrading mycrates state from the Rust tool

The Rust `mycrates` stored native records and pins as TOML; the Python tool stores JSON and refuses a state directory with TOML records left in it. Once, per state directory, with a backup in hand (`cp -a "$H/mycrates" "$H/mycrates.bak"`), run:

```sh
crates/mycrates/bin/mycrates migrate-state --state-dir "$H/mycrates/state"
```

The wrapper does not run this itself. The migrated state republishes the generation the live site already serves: check it by publishing into a scratch site (`crates/mycrates/bin/mycrates publish --state-dir "$H/mycrates/state" --site-dir "$H/mycrates/check"`) and comparing the `current` link there with the live `current` marker.

## mycrates auth

`mycrates` writes require the publish token, whether the helper generates it or you set `MYCRATES_TOKEN`. Reads follow `auth_required` in `config.json`, and the generated `current/registry/config.json` advertises the same value as `auth-required`.

Keep `auth_required` false for the public Worker and static servers, which do not enforce read auth. The publish server serves `blobs/` for Cargo verification, so a dependent native crate publishes without `--no-verify`.

## Deploy the Worker

```sh
cd crates/stuff-worker
npm ci
npm run build
cf deploy --prebuilt --dry-run
cf deploy --prebuilt
```

`npm run build` writes the Cloudflare Build Output Specification under `.cloudflare/output/v0`, and `cf deploy --prebuilt` uploads it without rebuilding (see [crates/stuff-worker/README.md](../crates/stuff-worker/README.md)). The three buckets already exist; use `--provision` only when you add one. Custom domains come from `domains` in `cloudflare.config.ts`.

The Worker uses Workers Caching. The default entrypoint is an uncached gateway. The `Registry` entrypoint caches responses under a key that includes the host, so the four domains cannot share cache entries. Immutable artifacts carry a one-year TTL. Mutable generation views carry 60 seconds, so a promotion becomes visible within that window without a cache purge. The gateway costs one extra worker-to-worker request per request; a cache hit skips `Registry` but still runs the gateway. Two requests to the same URL should show `cf-cache-status: HIT` on the second. Range requests are sliced from the cached full object, so a cold range fetch downloads the whole artifact.

## Verify the publication

```sh
make smoke-deployed
```

This builds the `stuff` binary and runs `deploy/deployed-smoke.sh`. The check is read-only and needs no Cloudflare credentials. It runs `stuff doctor --sample-artifacts` against the active generations, catalog objects, and sample artifact hashes, plus `npm view`, `uv pip install --dry-run`, and `oras manifest fetch` probes.

## Roll back a generation

```sh
deploy/r2-rollback.sh stuff-mycrates <generation-id>
```

The rollback verifies every object in the target generation's manifest before moving `current`, and records the target as this machine's promoted generation. It defaults to the conditional `s3` promotion, minting `R2_S3_*` when they are unset, and accepts `--promote=cf` for a plain put. To roll forward, run the same command with the newer generation id; gc keeps generations newer than `current` by default, so the target survives.

## Reclaim storage

`make gc` deletes R2 generations outside its retention window and shared-store objects no retained manifest references, then prints per-bucket and total storage before and after. It keeps `current` plus `GC_KEEP_PREVIOUS` older generations (default 1) and `GC_KEEP_NEWER` newer generations (default `all`). Preview with `GC_DRY_RUN=1`, and restrict buckets with `GC_BUCKETS`.

The command mints bucket-scoped credentials per bucket, so the usual `cf` login is enough. Because manifests are deleted with their generations, rollback and roll-forward only reach retained generations.

gc holds each bucket's publish lock while it plans and deletes, so it waits for a publish on the same machine and a publish waits for it. It deletes manifests first, then generation trees, then shared objects, and before every batch it re-reads `current` and the manifest set and stops if either changed. It also defers shared-store deletion while a recent generation directory has no manifest.

## Fix common failures

| Message | Cause | Fix |
| --- | --- | --- |
| `cf is not authenticated` | No active cf session | Run `cf auth login --use-keyring` |
| `--promote=s3 needs R2_S3_* and minting them failed` | The default `s3` promotion has no credentials and `cf` could not mint them | Run `cf auth login --use-keyring`, or mint by hand with `eval "$(python3 deploy/r2-temp-credentials.py BUCKET)"`. Pass `--promote=cf` only when no other machine can publish |
| `current changed during promotion` | Another publisher moved `current` | Pull the new generation, merge your changes, and publish again |
| `FAIL: immutable object differs` | A local immutable file differs from R2 | Verify the local site. Use `--repair` only to overwrite intentionally |
| `FAIL: .../current is not a symlink` | Wrong site directory | For `mynpm`, use `site/mirror` |
| `exceeds the 300 MB single-PUT cap` | Large object without S3 credentials | Set `R2_S3_*` |
| `differs from its manifest ETag` | A pulled object does not match the generation's manifest | Rerun the pull; if it repeats, the bucket is inconsistent |
| `publish: waiting for active writer` | Another publish, sync, rollback, or gc holds the lock | Wait for it to finish |
| `remote current is '...', expected '...'; refusing to overwrite a competing publication` | Another machine published, or rolled back, since this machine's last promotion | Adopt again: for mynpm and mypypi, move the site aside and publish; `deploy/publish mycrates` re-adopts by itself before building |
| `generation ... drops N versions served by live generation ...` | The new catalog lacks versions the live catalog serves, usually because this machine's state predates another publisher's | Re-adopt the live publication, or pass `--allow-drops` to remove them on purpose |
| `publish: adoption failed; staging is at ...` | A `mynpm` or `mypypi` adopt failed and left its staging directory | Inspect it, then run the printed `rm -rf` command and publish again |
