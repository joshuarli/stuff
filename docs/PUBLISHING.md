# Publish

The publish helpers build a registry site, upload missing objects through the authenticated `cf` CLI, and move the bucket's `current` marker to the new generation. `cf` auth is required, but not sufficient: the normal path also needs R2 S3 credentials for the conditional promotion.

## Check prerequisites

| Requirement | Use | Check |
| --- | --- | --- |
| `cf` signed in | List and upload R2 objects, deploy the Worker | `cf auth whoami` |
| R2 S3 API token | Conditional `current` promotion and multipart uploads | `R2_S3_*` is set |
| Existing publication state | Extend the live registry instead of replacing it | `$STUFF_STATE_HOME` exists |
| Rust nightly | Build the registry binaries | `cargo --version` |
| Node and npm | `mynpm` fetches npm packages | `node --version` |
| uv and CPython 3.15 | `mypypi` resolves and builds wheels | `uv --version` |
| Network | Fetch crates, npm tarballs, and Python wheels | |

If `cf auth whoami` reports no active session, run `cf auth login`.

Use `cf` for uploads and listings. Use the R2 S3 token for verification and conditional promotion. The fallback below replaces the token with an unconditional `cf` write.

The default state root is `${XDG_STATE_HOME:-$HOME/.local/state}/stuff`. Set `STUFF_STATE_HOME` if your state lives elsewhere. Back it up. This directory is the publication source of truth.

## Set R2 S3 credentials

Create an R2 API token in the Cloudflare dashboard and export its S3 credentials:

```sh
export R2_S3_ENDPOINT="https://<ACCOUNT_ID>.r2.cloudflarestorage.com"
export R2_S3_ACCESS_KEY_ID="<ACCESS_KEY_ID>"
export R2_S3_SECRET_ACCESS_KEY="<SECRET_ACCESS_KEY>"
```

`R2_S3_REGION` defaults to `auto`. Temporary credentials also need `R2_S3_SESSION_TOKEN`.

These credentials verify the uploaded generation and move `current` with `If-Match`. A competing publisher gets `412` instead of overwriting the winner.

If you cannot issue S3 credentials, promote through `cf` instead:

```sh
export R2_PROMOTE_UNCONDITIONAL=1
```

This writes `current` without a conditional check, so it cannot detect a competing publisher. Objects over the 300 MB single-PUT cap still need the S3 credentials.

## Publish a registry

Run the helper from the repository that owns the packages:

```sh
CRATES="crate-a crate-b" ../stuff/deploy/publish-mycrates.sh

LOCKFILE=pnpm-lock.yaml PROJECT=my-project ../stuff/deploy/publish-mynpm.sh

PACKAGES="six==1.16.0" ../stuff/deploy/publish-mypypi.sh
```

If `stuff` is not at `../stuff`, set `STUFF_DIR` to its path and invoke the helper by its full path:

```sh
STUFF_DIR=/path/to/stuff CRATES="crate-a" /path/to/stuff/deploy/publish-mycrates.sh
```

For `mynpm`, `TARGETS` adds platform-specific lockfile entries. For `mypypi`, Linux wheels use `deploy/mypypi-container.sh` for build, validate, and publish. See the root README for those options.

Each helper holds a per-registry lock, builds the site, uploads it, and promotes it. A source or build failure stops the run before any R2 write.

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

Add `--expected <generation>` to require the bucket to be on that generation before promotion. Without `--expected`, the conditional write still rejects another publisher.

`r2-sync.sh` requires `SITE_DIR/current` to be a symlink. For `mynpm`, pass `site/mirror`, not `site`.

## Publish from a new machine

If `$STUFF_STATE_HOME` is missing or empty, stop. A publish helper would extend an empty site and move `current` to a generation that contains only your local packages.

Adopt the live bucket first:

- Pull the bucket with `deploy/r2-pull.py`.
- Rebuild `mycrates` state from the pulled site with `mycrates adopt`.
- Run `mynpm adopt` against the pulled site.
- Keep the pulled `mypypi` site as its state.

Follow [Adopt a live publication](../README.md#adopt-a-live-publication) for the exact commands. After adoption, the next publish carries every live package forward.

## Deploy the Worker

```sh
cd crates/stuff-worker
npm ci
cf deploy --dry-run
cf deploy
```

`cf deploy` runs `npm run build:wasm` before bundling `src/entry.ts`. In this deployment, the three buckets already exist. Use `--provision` only when you add a bucket. Custom domains come from the `[[routes]]` entries in `wrangler.toml`.

## Verify the publication

```sh
make smoke-deployed
```

This builds the `stuff` binary and runs `deploy/deployed-smoke.sh`. The check is read-only and needs no Cloudflare credentials. It checks the active generations, catalog objects, sample artifact hashes, and real package-manager probes.

## Roll back a generation

```sh
deploy/r2-rollback.sh stuff-mycrates <generation-id>
```

The rollback verifies every object in the target generation's manifest before moving `current`. It needs the same promotion credentials as a publish. To roll forward, run the same command with the newer generation id.

## Fix common failures

| Message | Cause | Fix |
| --- | --- | --- |
| `FAIL: R2_S3_ENDPOINT is not set` | No S3 credentials | Set `R2_S3_*` or `R2_PROMOTE_UNCONDITIONAL=1` |
| `current changed during promotion` | Another publisher moved `current` | Pull the new generation, merge your changes, and publish again |
| `FAIL: immutable object differs` | A local immutable file differs from R2 | Verify the local site. Use `--repair` only to overwrite intentionally |
| `FAIL: .../current is not a symlink` | Wrong site directory | For `mynpm`, use `site/mirror` |
| `exceeds the 300 MB single-PUT cap` | Large object without S3 credentials | Set `R2_S3_*` |
| `publish: waiting for active writer` | Another publish holds the lock | Wait for it to finish |
