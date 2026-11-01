# Publish

`deploy/publish` is the normal entry point. It checks `cf` auth, adopts the live publication until local state is marked complete, and runs the matching publish helper. Promotion defaults to `cf`, which is safe for a single publisher and needs no R2 S3 credentials.

## Check prerequisites

| Requirement | Use | Check |
| --- | --- | --- |
| `cf` signed in | List and upload R2 objects, promote `current` | `cf auth whoami` |
| Publication state | Extend the live registry instead of replacing it | The wrapper adopts if missing |
| Rust nightly | Build the registry binaries | `cargo --version` |
| Node and npm | `mynpm` fetches npm packages | `node --version` |
| uv and CPython 3.15 | `mypypi` resolves and builds wheels | `uv --version` |
| Network | Fetch crates, npm tarballs, and Python wheels | |
| R2 S3 token (optional) | Long-lived credentials, parallel republishes, conditional promotion, and multipart uploads over 300 MB | `R2_S3_*` is set, or `cf` mints temporary credentials |

If `cf auth whoami` reports no active session, run `cf auth login --use-keyring`. To keep publishing credentials separate, create a profile and bind it to the source repository with `cf auth activate stuff .`.

The default state root is `${XDG_STATE_HOME:-$HOME/.local/state}/stuff`. Set `STUFF_STATE_HOME` if your state lives elsewhere. Back it up. This directory is the publication source of truth.

## Publish a registry

Run the wrapper from the repository that owns the packages:

```sh
CRATES="crate-a crate-b" /path/to/stuff/deploy/publish mycrates

LOCKFILE=pnpm-lock.yaml PROJECT=my-project /path/to/stuff/deploy/publish mynpm

PACKAGES="six==1.16.0" /path/to/stuff/deploy/publish mypypi
```

If `stuff` is at `../stuff`, use `../stuff/deploy/publish` instead. The wrapper sets `STUFF_DIR` to its own checkout, so the helper builds the right binaries.

The wrapper does three things before it publishes:

- It verifies `cf auth whoami`.
- It adopts the live bucket until the local state is marked complete.
- It selects `cf` promotion unless you pass `--promote=s3`.

The helper then holds the per-registry lock, builds the site, uploads it, and promotes it. A source or build failure stops the run before any R2 write. The mycrates helper restores the public base URL and rebuilds the site from its exit trap, so a failure cannot leave `http://127.0.0.1:8080` in the site for a later manual `r2-sync.sh` to promote.

For `mynpm`, `TARGETS` adds platform-specific lockfile entries. For `mypypi`, Linux wheels use `deploy/mypypi-container.sh` for build, validate, and publish. See the root README for those options.

## Republish without new packages

A renderer or catalog change reaches a registry's pages only when that registry publishes again. `--republish` does that without package inputs. It adopts the live publication if needed, renders it again under the current renderer, and promotes it:

```sh
deploy/publish mycrates --republish
deploy/publish mynpm --republish
deploy/publish mypypi --republish
```

`mycrates` rebuilds its site from state, `mynpm` runs `mynpm adopt` on the site, and `mypypi` runs `mypypi republish`, which refuses an index with nothing published. Unchanged output reports no new generation.

`make deploy` runs the whole rollout in order: the Worker first, so it can read the newest catalog schema, then the three republishes, then `make smoke-deployed`. The three republishes run in parallel; each mints its own temporary S3 credentials when `R2_S3_*` is absent. Set `DEPLOY_JOBS=1` to serialize them. A failed mint falls back to the paced `cf` API. A failure stops the rollout before the live check.

## Upload and promote with S3

The `cf` API allows roughly four requests a second per account. A republish after a catalog change uploads a new generation's whole index tree, so listing and uploading through `cf` costs several minutes even though the files are small.

Before a sync, `r2-sync.sh` mints short-lived S3 credentials from the authenticated `cf` CLI with `cf r2 temporary-credentials create`. They are scoped to the bucket with object read and write and expire after `R2_TEMP_CREDENTIALS_TTL` seconds (default 3600). The parent access key ID is the first active account token whose policies mention R2; set `R2_PARENT_ACCESS_KEY_ID` to choose one. The credentials are exported into the sync process and never written to disk. If minting fails, the sync falls back to the paced `cf` API.

When `R2_S3_*` is already set, the sync uses those credentials instead. Listing and uploads then use the S3 API: uploads run `R2_PUT_JOBS` in parallel (default 8), the remote plan comes from one paginated list, and the generation is verified against that list. The upload cap is gone, so a republish is bounded by local rendering and hashing instead of minutes of uploads.

The `cf` promotion writes `current` without a conditional check. That is safe for a single publisher because no other writer can race it. Using it also avoids needing conditional-write credentials.

Use `--promote=s3` when CI or another machine can publish at the same time:

```sh
CRATES="crate-a" /path/to/stuff/deploy/publish mycrates --promote=s3
```

This uses the temporary credentials minted by the sync when `R2_S3_*` is not set. To use long-lived credentials instead:

```sh
export R2_S3_ENDPOINT="https://<ACCOUNT_ID>.r2.cloudflarestorage.com"
export R2_S3_ACCESS_KEY_ID="<ACCESS_KEY_ID>"
export R2_S3_SECRET_ACCESS_KEY="<SECRET_ACCESS_KEY>"
```

`R2_S3_REGION` defaults to `auto`. Long-lived tokens should allow object read and write on the three registry buckets and nothing else.

S3 promotion verifies the uploaded generation and moves `current` with `If-Match`. A competing publisher gets `412` instead of overwriting the winner. Objects over the 300 MB single-PUT cap need S3 credentials, either minted or long-lived.

## Publish from a new machine

The wrapper adopts automatically until the mycrates state carries an
`.adopt-complete` marker. It pulls the current generation and the shared
stores, then rebuilds the local publication state before running the helper.

For an absent or empty state, adoption runs `mycrates init`, `mycrates adopt`,
and `mycrates verify` in a sibling staging directory next to the state. It
writes the marker there and renames the whole state into place, so a failed or
interrupted run leaves no live state behind; the next publish retries because
the marker is still absent.

An existing state without the marker is never replaced. That includes state
left by an interrupted older adopt and state this wrapper has not adopted
before. The wrapper runs the idempotent adopt into it, verifies it, and only
then writes the marker. Local records that are not on the live site stay in
place, and an interrupted upgrade is retried next time.

For `mynpm` and `mypypi`, adoption pulls and verifies into a sibling staging
directory and renames it into the live site only after that step succeeds. A
failed pull leaves no live site behind; the wrapper prints the staging path and
the `rm -rf` command that removes it. After inspecting it, run the publish
again to start a fresh adoption.

If the wrapper refuses because a site directory exists without `current`, do
not delete it. Inspect it, or restore the state backup, before publishing. A
publish against an empty or partial state would replace the live registry with
only the local packages. This refusal covers genuinely ambiguous live
directories; the staging paths above are only for this wrapper's own failed
adoptions.

For the manual adopt sequence, see [Adopt a live publication](../README.md#adopt-a-live-publication).

## mycrates auth

`mycrates` writes require the publish token, whether the helper generates it or you set `MYCRATES_TOKEN`. Reads follow `auth_required` in `config.json`, and the generated `current/registry/config.json` advertises the same value as `auth-required`.

Keep `auth_required` false for the public Worker and static servers, which do not enforce read auth. The publish server serves `blobs/` for Cargo verification, so a dependent native crate publishes without `--no-verify`.

## Sync a site you already built

Use `deploy/r2-sync.sh` when the site is already on disk:

```sh
H=${STUFF_STATE_HOME:-${XDG_STATE_HOME:-$HOME/.local/state}/stuff}

deploy/r2-sync.sh "$H/mycrates/site" stuff-mycrates
deploy/r2-sync.sh "$H/mynpm/site/mirror" stuff-mynpm
deploy/r2-sync.sh "$H/mypypi/site" stuff-mypypi
```

| Registry | Site directory | Bucket |
| --- | --- | --- |
| mycrates | `$H/mycrates/site` | `stuff-mycrates` |
| mynpm | `$H/mynpm/site/mirror` | `stuff-mynpm` |
| mypypi | `$H/mypypi/site` | `stuff-mypypi` |

Add `--dry-run` to list what would change. It writes manifests into the site, needs no S3 credentials, and stops before any upload or promotion.

Add `--expected <generation>` to require the bucket to be on that generation before promotion. Without `--expected`, a conditional S3 promotion still rejects another publisher.

`r2-sync.sh` requires `SITE_DIR/current` to be a symlink. For `mynpm`, pass `site/mirror`, not `site`.

## Deploy the Worker

```sh
cd crates/stuff-worker
npm ci
cf deploy --dry-run
cf deploy
```

`cf deploy` runs `npm run build:wasm` before bundling `src/entry.ts`. In this deployment, the three buckets already exist. Use `--provision` only when you add a bucket. Custom domains come from the `[[routes]]` entries in `wrangler.toml`.

`cf deploy` delegates the bundle step to the project-local Wrangler, and the runtime integration test uses Miniflare, so both remain development dependencies.

## Verify the publication

```sh
make smoke-deployed
```

This builds the `stuff` binary and runs `deploy/deployed-smoke.sh`. The check is read-only and needs no Cloudflare credentials. It checks the active generations, catalog objects, sample artifact hashes, and real package-manager probes.

## Roll back a generation

```sh
deploy/r2-rollback.sh stuff-mycrates <generation-id>
```

The rollback verifies every object in the target generation's manifest before moving `current`. It defaults to `cf` promotion and accepts `--promote=s3` for the conditional write. To roll forward, run the same command with the newer generation id.

## Fix common failures

| Message | Cause | Fix |
| --- | --- | --- |
| `cf is not authenticated` | No active cf session | Run `cf auth login --use-keyring` |
| `R2_S3_ENDPOINT is not set` | `--promote=s3` without S3 credentials | Set `R2_S3_*` or use the default `cf` promotion |
| `current changed during promotion` | Another publisher moved `current` | Pull the new generation, merge your changes, and publish again |
| `FAIL: immutable object differs` | A local immutable file differs from R2 | Verify the local site. Use `--repair` only to overwrite intentionally |
| `FAIL: .../current is not a symlink` | Wrong site directory | For `mynpm`, use `site/mirror` |
| `exceeds the 300 MB single-PUT cap` | Large object without S3 credentials | Set `R2_S3_*` |
| `publish: waiting for active writer` | Another publish holds the lock | Wait for it to finish |
| `publish: adoption failed; staging is at ...` | A `mynpm` or `mypypi` adopt failed and left its staging directory | Inspect it, then run the printed `rm -rf` command and publish again |
