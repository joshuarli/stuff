# OpenOrange shared host stack (`deploy/openorange/`)

Runbook for the stack that hosts Stats, Stuff, Discover and fronts Tansu on
`lisa` (Alpine 3.25, musl, x86_64, OpenRC, Docker, tailnet address
`100.68.104.43`). Campaign context: `stats/docs/CAMPAIGN-OPENORANGE.md`, slice S7.
Rehearsed on `lisa` on 2026-11-13; production DNS and the existing services
were not touched.

## Components and pins

Everything is pinned by multi-arch **index digest** in `images.lock.json`; the
`linux/amd64` manifest digest is recorded next to it and `bin/pin-verify`
re-checks both against the registry (and the local image platform).

| Role | Image | Index digest | linux/amd64 manifest |
| --- | --- | --- | --- |
| Rivet Engine | `rivetdev/engine:3.1.2-rc.1` (slim) | `sha256:6e585002b700dd4d3630808eb67884aa15d1a602d83781984edac47023b6d40f` | `sha256:66de9f77cde47be3cd7af2c001e8083784f6b91ca58b0ab5d7878677cff5bd5d` |
| S3 store | `versity/versitygw:v1.11.0` (posix backend) | `sha256:8319a530d08d537426e2f855bae5280709795dc73a3f0c347973ef915d6343c5` | `sha256:5da5e2cd887eec7ae6ab1cd6f5099207c754964358ab6351da2a9d6b80507e4e` |
| Caddy builder | `caddy:2.11.9-builder-alpine` | `sha256:646fee578e680989ca5c7fd1ce6f1de574d19fd71ef2221810cc0510a9f40c86` | `sha256:29f98311e17e583811355fa62cc080f71d6f3aa19cf09a9e1494d30579f8bebf` |
| Caddy runtime | `caddy:2.11.9-alpine` | `sha256:003a97808e781aa9f7e75cc2f43d34d461099031faf5e20faefb9f61126836f0` | `sha256:fc9cbc100585871d18d4e451cca7021785c90a7006087427ae5ceb3e4efe6424` |
| Identity base | `python:3.14-alpine` | `sha256:5862d971d1f0ae26b258cefa4e893556b1d3aba81bd2ff30888704ad378bab67` | `sha256:76c65fc8c52c4d375e958298d9f66198b8fb6b06416c0e5500bf710c19cbaa77` |

Locally built (never pulled): `openorange/caddy:2.11.9-cf0.2.4` is
`xcaddy build v2.11.9 --with github.com/caddy-dns/cloudflare@v0.2.4` (module
list contains `dns.providers.cloudflare`; binary sha256
`d46ee78e53e3362c084968808c6868c6631ead385d626c0be61afeb88624c8e4`, file cap
`cap_net_bind_service=ep`) and `openorange/identity:1` (`identity/identity.py`).
`ingress/Dockerfile` and `identity/Dockerfile` carry the base digests as ARG
defaults; `pin-verify` fails if they drift from the lock.

**Engine version.** The apps pin open-orange 1.0.2 / rivetkit 3.1.2-rc.1. The
Engine image `3.1.2-rc.1` exists on Docker Hub and, like the
`@rivetkit/engine-cli` 3.1.2-rc.1 binary, reports `Rivet 3.1.1`
(`rivet-engine --version`, `/health` -> `"version":"3.1.1"`). `3.1.1` reports
the same string, so the version string cannot tell them apart; the matching
release candidate was chosen. A release candidate is in production position:
re-evaluate when `3.1.2` ships. Rivet's `--runtime-info` and `/health` report
`architecture: v3`. rivet-services ships GNU binaries only; the Engine image
and every Orange app image are Debian/glibc containers, and nothing glibc runs
on the Alpine host itself.

## S3 store selection (empirical)

`bin/s3-conformance` (standard-library SigV4) probes path-style addressing,
multipart completion and ETag format, `If-None-Match: *` / `If-Match`
conditional PUT, an 8-way concurrent create race, ListObjectsV2 pagination and
ranged GET. Raw results: `tests/s3-backend-results.json`.

| Check | Garage v2.5.0 | versitygw v1.11.0 (and the deployed stack) |
| --- | --- | --- |
| Path-style PUT/GET | pass | pass |
| Multipart 3 parts, completes; ETag `md5(md5s)-3` | pass | pass |
| `If-None-Match: *` on existing key -> 412 | **fail (200, value overwritten)** | pass (412, value unchanged) |
| `If-Match` stale ETag -> 412 | **fail (200)** | pass |
| `If-Match` on absent key | **fail (200)** | pass (404) |
| 8 concurrent `If-None-Match: *` creates | **fail (8 x 200)** | pass (1 x 200, 7 x 412) |
| `If-None-Match: *` on CompleteMultipartUpload | fail (info) | pass (412) |
| ListObjectsV2 pages (max-keys 10 over 25), delimiter, start-after | pass | pass |
| Ranged GET `a-b`, `a-`, `-n`; unsatisfiable -> 416 | pass | pass |
| `GET ?partNumber=N` | fail (info) | fail (info, 400) |

Garage silently ignores the conditional headers, so it cannot back registries
that publish with conditional writes. The unreleased Garage `main` image
(`99923a87`) answered 500 on a freshly initialised cluster (`Unable to decode
entry of bucket_v2`) and was not pursued. SeaweedFS was not tested because
versitygw passed. **versitygw** was chosen: posix backend (plain files and
xattrs on ext4), `--fsync`, bucket versioning directory enabled, per-app IAM
users that own their buckets. The deployed stack run (key of the `demo` app)
also proved isolation: the same key gets `403` on PUT, GET and LIST of
`stuff-oci`. Open-orange maps each R2 binding onto this store through
`ORANGE_R2_<BUCKET>_S3_*` variables, which `oo secrets` writes; an R2 `onlyIf`
create through Orange against this store returned `stored:false` on the second
create.

## Topology

```
tailnet client --443--> [host net] oo-ingress (Caddy, 100.68.104.43:443)
                          |  unix socket /run/openorange/identity/identity.sock
                          |      oo-identity (python, --network none) -> tailscaled LocalAPI whois
                          |  http://172.29.80.2x:8080
cloudflared (pending) --> 127.0.0.1:18480 (public listener, same Caddy)
                          v
        docker network `openorange` 172.29.80.0/24, --internal (no route off the host)
        oo-store .10:7070   oo-engine .11:6420   oo-app-<name> .20+:8080
```

* One private bridge `oo0`, `--internal`: containers cannot reach the internet
  or the LAN. No container publishes a port (`docker ps` shows none; `bin/health`
  asserts it). Engine API, store S3 and app listeners exist only on
  `172.29.80.0/24`; the store admin API is a unix socket in
  `/run/openorange/store-admin`.
* **Why Caddy uses host networking.** Measured on `lisa`: a container publishing
  `100.68.104.43:18999` saw the Mac's request (100.107.2.101) as source
  `172.17.0.1`; Docker's published-port path hides the tailnet peer, so
  `whois` could not work. Host networking gives Caddy the true peer address,
  lets it bind exactly `100.68.104.43:443` (no `:80` listener at all) and reach
  Tansu on `127.0.0.1:3001`; it reaches app containers by their static
  addresses (the host routes to bridge addresses, including `--internal`
  networks). Caddy and the sidecar run as uid 10001 with every capability
  dropped except `NET_BIND_SERVICE` on Caddy.
* Engine storage (`/var/lib/openorange/engine/{log,objects}`, Rivet
  `file_system` tier, its own token) is separate from application buckets
  (`/var/lib/openorange/store/...`, per-app S3 keys). Rivet's S3 tier on this
  store was tried and failed twice identically (see Deviations).
* **Apps**: each `[apps.NAME]` in `apps.toml` gets its own Rivet namespace, S3
  key and bucket set, gateway secret, container and OpenRC instance
  (`oo-app.NAME`). Adding or restarting one never restarts another.
  Pool names are not configurable: open-orange 1.0.2 hard-codes `default`
  (Durable Objects) and `services`; both are created in every namespace and
  isolation is the namespace.
* Admin token: `/etc/openorange/engine.env` (0600 root, never printed). Every
  Orange app container needs it to connect (`RIVET_TOKEN`); Rivet 3.1.x has no
  static namespace-scoped node credential (scoped JWTs expire within a day and
  were not tested for node connections). A compromised app container can
  therefore act on other namespaces; the container boundary, not the token, is
  the isolation.

### Ingress contract

For every enabled hostname in `apps.toml` (`bin/oo render --stdout` prints the
whole Caddyfile) Caddy:

1. deletes client `Tailscale-*`, `Stats-Gateway-Secret`, `Stats-Tailnet-Identity`,
   `X-OO-Peer`, `X-OO-Ingress`, `X-Forwarded-*`, `Forwarded`, `X-Real-Ip`;
2. `forward_auth` to the sidecar with `X-OO-Peer: <accepted connection ip:port>`;
   the sidecar answers `200` + `Stats-Tailnet-Identity: user:<login>` or
   `tags:<tag>,<tag>` (sorted, `tag:` prefix kept), `403` for anything that is not
   a tailnet peer, `503` if tailscaled is unavailable (fail closed);
3. proxies with `Stats-Gateway-Secret: <per-app secret>`, the identity header
   copied only from the sidecar response, `X-OO-Ingress: tailnet`, `Host`
   unchanged (Tansu's passkey origin checks), and Caddy's own
   `X-Forwarded-For` (the true peer).

There is no email allowlist. A local process on `lisa` is the node's own user.
Apps whose image is not deployed yet answer `503 app not deployed` after the
identity check.

### Public listener (Discover; tunnel pending)

Declared in `apps.toml`: `[ingress.public] bind = "127.0.0.1" port = 18480`
(validation rejects any non-loopback bind) and per app `public_hostnames`
(subset of `hostnames`) and `public_deny_paths` (default `["/admin"]`: the path
and `path/*`). Discover is defined as `discover` (`hq.`, `research.`) and
`discover-preview` (`hq-preview.`, `research-preview.`), each with its own
namespace, bucket and secret. On the public listener Caddy serves plain HTTP on
loopback only, with no sidecar and **no gateway secret**; it strips the same
client headers, sets `X-OO-Ingress: public`, answers `404` for `/admin`,
`/admin/*` and normalisation variants (case, `//`, `./`, `../`, `%61dmin`,
`%2f`, `;params`), and `404` for any other Host. The same hostnames on the
tailnet listener serve everything including `/admin`, with identity. Apps must
treat "no valid gateway secret" as anonymous public traffic and must use
`Cf-Connecting-Ip` for the client address on that path.

**Pending owner actions** (nothing was created): (1) an outbound-only
cloudflared tunnel on `lisa`, run like the existing `tansu-tunnel`
(`--network host`, token in its env), with public hostnames mapped to service
`http://127.0.0.1:18480` and the four Discover DNS records pointed at the
tunnel; (2) because those hostnames then resolve to Cloudflare for everyone,
tailnet clients need split DNS (or a MagicDNS/unbound override) sending them to
`100.68.104.43` to reach `/admin`, otherwise the tailnet listener is
unreachable by name; (3) for ACME on the tailnet listener the Discover
hostnames need DNS-01 only (no public A record is needed).

### Tansu

`tansu.17166969.xyz` is an `[apps.tansu]` `kind = "native"` entry: tailnet
listener, identity, upstream `127.0.0.1:3001`, `Host` untouched. `tansu`,
`tansu-http-redirect` and `tansu-tunnel` were not stopped or changed. At
cutover (S10), after DNS for `tansu.` resolves to the tailnet address for
every client that needs it (passkeys are bound to the RP ID, so the hostname
must not change), retire the tunnel: `docker stop tansu-tunnel && docker
update --restart=no tansu-tunnel`, then delete the tunnel and its public
hostname in Cloudflare. Not done now.

### Hostnames (tailnet listener; all get DNS-01 certificates)

`stats.` (Stats); `stuff.`, `crates.`, `crates-blobs.`, `npm.`, `pypi.`, `oci.`,
`oci-blobs.`, `mirror.` (Stuff); `hq.`, `research.` (Discover);
`hq-preview.`, `research-preview.` (Discover preview); `tansu.` (Tansu), all
under `17166969.xyz`.

### TLS

`[ingress].tls = "internal"` (rehearsal): Caddy's internal CA, certificates
for the exact hostnames; clients use `-k` or trust
`/var/lib/openorange/caddy/data/caddy/pki/authorities/local/root.crt`.
`tls = "acme"`: DNS-01 through `caddy-dns/cloudflare`, token from
`/etc/openorange/cloudflare-dns.env` (`CF_API_TOKEN=...`, 0600 root, passed with
`--env-file`, never in argv). Real ACME is **pending the token**: the
`acme` Caddyfile validates against the built binary, and `oo run-args ingress`
refuses to start in `acme` mode while the file is missing. Switching: write the
token file, set `tls = "acme"`, `bin/oo render`, `rc-service oo-ingress
restart`. `bin/oo render --stdout --tls acme` previews it.

## Layout

| Path | Contents |
| --- | --- |
| `/opt/openorange` | this directory (`bin/sync-code HOST` copies it) |
| `/etc/openorange` (0700 root) | `engine.env`, `store.env`, `ingress.env`, `cloudflare-dns.env` (owner), `apps/NAME.env`, `apps/NAME.image`, `apps/NAME.extra.env`, rendered `engine/config.json`, `ingress/Caddyfile`, `overlay.toml` (rehearsal only) |
| `/var/lib/openorange` | `engine/{log,objects}`, `store/{data,versions,iam}`, `caddy/{data,config}`, `backups/`, `restore/` |
| `/var/log/openorange` | `store.log engine.log identity.log ingress.log app-NAME.log` |
| `/run/openorange` | `identity/`, `caddy/`, `store-admin/` sockets (recreated at start) |
| `/etc/init.d/oo-{store,engine,identity,ingress}`, `oo-app[.NAME]` | OpenRC units |
| `/etc/conf.d/oo-ingress`, `/etc/periodic/daily/oo-logrotate` | generated ordering, log rotation |

`bin/`: `oo` (validate, run-args, secrets, prepare, render, network,
provision, set-image, remove, reload-ingress, wait), `install`, `install-app`,
`build-images`, `build-orange-app`, `health`, `drain`, `disk-usage`, `backup`,
`restore-rehearsal`, `logrotate`, `pin-verify`, `s3-conformance`, `sync-code`.
`tests/`: `client-checks.sh` (from a tailnet client),
`public-listener-checks.sh` (on the host), `s3-backend-results.json`;
`identity/test_identity.py` (unit tests with a fake LocalAPI; run
`cd identity && python3 -m unittest`).

### Supervision design

Each unit is an OpenRC `supervise-daemon` running `docker run --rm` in the
foreground, with the argument vector from `oo run-args` (single source:
`apps.toml` + `images.lock.json`). Chosen over Compose because `docker compose`
is not installed on `lisa`, and installing it would add a host package and a
second supervisor; a foreground CLI lets OpenRC own restart, ordering and
`rc-service ... stop` (the CLI proxies SIGTERM to the container; each unit has
`retry TERM/<n>/KILL/5`, the Engine 150 s). `start_post` blocks until the
service answers; on failure it stops the supervisor and removes the container
(verified: an app with a bogus image failed after 90 s with no supervisor or
container left). Output goes to `/var/log/openorange/*.log` (container log
driver `none`); `bin/logrotate` (daily, copy-truncate, 5 x gz over 20 MiB).
Containers run `--read-only`, `--cap-drop ALL`, `no-new-privileges`, non-root
where the image allows (store, engine, ingress, identity, apps).

## Install (on `lisa`, as root; `doas` is the privilege tool there, not `sudo`)

`doas` does not search `/sbin`: spell out `/sbin/rc-service` or work in a root shell.

```sh
bin/sync-code lisa                            # from a workstation
ssh lisa doas /opt/openorange/bin/install     # units, secrets, network, builds, render, rc-update
ssh lisa doas /sbin/rc-service oo-store start
ssh lisa doas /sbin/rc-service oo-engine start
ssh lisa doas /opt/openorange/bin/oo provision    # S3 users/buckets, Rivet namespaces/pools
ssh lisa doas /sbin/rc-service oo-identity start
ssh lisa doas /sbin/rc-service oo-ingress start
ssh lisa doas /opt/openorange/bin/health
```

Prerequisites already on the host (no package was installed): `docker`
(29.9), `python3` 3.14 (needs `tomllib`, >= 3.11), `rsync`, `curl`, `openssl`.
`install` pulls and builds images (network needed once; the Go module download
for the Cloudflare plugin).

## Add an app

1. Add `[apps.NAME]` to `apps.toml` (namespace, `ip`, `port`, `hostnames`,
   `buckets`, optional `memory`, `cpus`, `secrets`, `public_hostnames`).
   `bin/oo validate` rejects unknown keys, duplicate hostnames/IPs/buckets and a
   non-loopback public bind.
2. Build the image in the app's own repo/CI. For a Worker:
   `bin/build-orange-app WORKER_DIR IMAGE HOST [TAG]` runs
   `open-orange@1.0.2 deploy --dry-run --outdir`, copies the context and runs
   `docker build --platform linux/amd64` on the host. The generated Dockerfile
   uses an unpinned `node:24-slim` base: pinning is the app repo's decision.
3. `doas /opt/openorange/bin/install-app NAME IMAGE`: provisions namespace,
   pools, S3 key and buckets, writes `/etc/openorange/apps/NAME.env`, creates the
   `oo-app.NAME` instance, starts it, re-renders and reloads Caddy.
   Other apps are not restarted. Roll an app: build, `oo set-image NAME IMAGE`
   (previous kept as `NAME.image.prev` for rollback), `rc-service oo-app.NAME restart`.
4. The app environment: `RIVET_ENDPOINT RIVET_NAMESPACE RIVET_TOKEN`,
   `OO_GATEWAY_SECRET` (declared in `ORANGE_WORKER_SECRETS`, so it appears as a
   Worker secret binding), `S3_*`, and `ORANGE_R2_<BUCKET>_S3_*` for each R2
   binding. Extra secrets: list the names in `secrets = [...]` and put the
   values in `/etc/openorange/apps/NAME.extra.env`. `RIVET_NODE_VERSION` is not
   set (Orange logs a warning that actors will not drain automatically across
   deploys).
5. Remove: `oo remove NAME [--purge-store]`. Rivet 3.1.x has no namespace
   delete, so the namespace and the app's Actor state remain in the Engine
   volumes.

## Operate

* **Health / readiness:** `bin/health [--json]` (root). Checks network, each
  OpenRC service and container, store and Engine health, unauthenticated Engine
  rejection, namespaces/pools, app node connections, sidecar behaviour (own
  address resolves, non-tailnet peer is 403), every ingress hostname, public
  listener `/admin` 404, TLS certificate, that no stack port listens on a host
  interface, that 443 is bound only on the tailnet address, that no container
  publishes a port, free space (Engine refuses to start below 10% / 4 GiB) and
  backup age. Exit 1 on any FAIL.
* **Drain / start:** `bin/drain` stops ingress -> apps -> identity -> engine ->
  store, each SIGTERM first (observed 1-3 s each; Caddy's `grace_period` 15 s);
  `bin/drain --start` reverses it and continues past failures; `--apps` stops
  only ingress and apps. OpenRC dependencies make `rc-service oo-engine restart`
  bounce the apps and `oo-identity restart` bounce ingress.
* **Logs:** `/var/log/openorange/*.log` (root-readable); Caddy writes access
  logs without request or response headers.
* **Disk accounting:** `bin/disk-usage [--json]`: Engine log, Engine objects,
  each app's bucket set (incl. versions), store total, caddy, backups, restore
  rehearsals, logs, free space, Docker.
* **Backup:** `bin/backup [--dest DIR] [--keep N]`. Rivet documents no online
  snapshot for a single instance, so it fences: drains the stack, `rsync -aHX`
  (hard-linked against the previous backup) of Engine log+objects and store
  data/versions/iam, `/etc/openorange` (secrets: the backup directory is 0700),
  `apps.toml`, `images.lock.json`; restarts; then writes `SHA256SUMS` and
  `manifest.json`. Downtime measured: 7 s on an idle stack. A failed copy
  still restarts the stack and leaves `*.partial`.
* **Restore rehearsal:** `bin/restore-rehearsal [BACKUP] [--app A --probe PATH
  --expect TEXT] [--purge]` copies the Engine directories into
  `/var/lib/openorange/restore/<ts>/`, starts a second Engine on a spare
  address of the private network (no published port), verifies every declared
  namespace and pool, and with `--app` starts a throwaway container of the app
  image against it and prints the probe response. A real restore is the same
  copy into the live directories with the stack stopped
  (`drain`, copy back, `drain --start`). Rivet states a fresh-cluster restore
  "has not yet been validated for this release": a `/health` answer is not proof,
  so always probe application state. The store restore is a file copy and was
  verified only by `sha256sum -c` of the backup, not by serving it again.
* **Upgrade:** (1) `bin/backup`; (2) `bin/pin-verify --resolve` after choosing
  the new tags in `images.lock.json` (review the diff), `bin/pin-verify`;
  (3) `bin/sync-code`, `doas bin/build-images` for Caddy/identity;
  (4) Rivet requires a full stop for the first upgrade: `doas bin/drain`, pull
  the new Engine/store images, `doas bin/drain --start`, `bin/health`;
  (5) upgrade the Engine before the apps' rivetkit (apps behind the Engine are
  accepted; newer SDKs may need the newer Engine); (6) `bin/restore-rehearsal
  --app ... --probe ...` from the pre-upgrade backup if anything looks wrong.
  Caddy-only changes: `oo reload-ingress` (validates, graceful reload).

## Loss model

Single host, single NVMe disk. All of Engine, store, Caddy state and the
default backups live on one filesystem, so a disk or host loss loses everything
on `lisa`; the default backup protects against operator error, corruption and a
bad upgrade, not hardware. Anything written after the last backup is lost on
restore. A copy on another device or host (`--dest`) is the owner's decision
and is **not configured**. The Engine acknowledges a write after a majority of
instances have it on disk; with one instance a log-disk loss loses writes not
yet in the object tier. The application export planned in S4 stays the
recovery contract independent of Engine internals. The store is a single
copy of plain files; bucket versioning is enabled but unproven under restore.

## Rehearsal record (lisa, 2026-11-13)

Demo Worker `deploy/openorange/demo` (SQLite Durable Object + R2 binding)
answered `stats.17166969.xyz` and `demo.17166969.xyz` in namespace `demo`
(overlay `rehearsal/overlay.toml`). From the Mac (100.107.2.101),
`tests/client-checks.sh`: identity forwarded as `user:<login>`; forged
`Stats-Tailnet-Identity`, `Stats-Gateway-Secret`, `Tailscale-*`,
`X-Forwarded-*`, `X-OO-Peer` stripped or overwritten; Tansu `/api/v2/health`
200 via ingress; ports 6420/6421/6422/6430/7070/7071/8080/80/18480 closed on
`100.68.104.43` and `10.0.1.56`, 443 open only on the tailnet address.
`public-listener-checks.sh` on the host: `/admin` and ten variants 404, forged
headers not delivered, loopback only. DO count survived app restart, Engine
restart and an Engine SIGKILL (supervisor respawn); backup at count 9 then
restore into a fresh directory returned 9 while the live count was 11.
Tagged-node identity: not run on a real device (no tagged node on the tailnet;
`tailscale status --json` shows no tags); the mapping is unit-tested with
canned `whois` documents only.

After the demo was removed (`oo remove demo --purge-store`, overlay deleted)
the stack stays running, idle: `oo-store`, `oo-engine`, `oo-identity`,
`oo-ingress` (all four boot-enabled), Caddy on `100.68.104.43:443` and
`127.0.0.1:18480`, namespaces `stats stuff discover discover-preview` (plus the
leftover `demo` namespace, whose Actor state cannot be deleted) with all apps
answering `503 app not deployed`. `oo-app.stats` and `oo-app.stuff` init
instances exist but are not in the default runlevel until an image is deployed.

## Deviations and open items

* Engine storage uses Rivet's `file_system` tier, not an S3 bucket: the Engine
  with `object_storage.primary.s3` pointed at versitygw timed out at startup
  (`config operation timed out`) with no request ever reaching the store, twice
  in the same way (also with `api_peer` removed); not diagnosed further
  (budget). The documented single-instance compose guide uses `file_system`.
  Rivet's production guidance (S3 with versioning, 3 instances) is not met.
* Release-candidate Engine image; app pools fixed to `default`/`services`;
  admin token shared by all app containers (see Topology).
* `sudo` does not exist on `lisa`; passwordless `doas` was used. Host changes:
  the paths above, `rc-update add` of the four services, images pulled/built
  (`rivetdev/engine`, `versity/versitygw`, `caddy`, `python`, two local
  builds), docker network `openorange`. No package installed.
* The IAM, `.vgwlocks` and `versions` directories belong to the store; do not
  edit them by hand.

## Pending (owner)

1. Cloudflare API token limited to DNS edit on `17166969.xyz` ->
   `/etc/openorange/cloudflare-dns.env`, then switch `tls = "acme"`; DNS
   records (not created here).
2. Discover public path: the cloudflared tunnel and its DNS (above), plus
   split DNS for tailnet access to the same hostnames.
3. Tailnet policy for tagged machines that must reach `100.68.104.43:443`; a
   tagged-node request is untested.
4. Off-host/off-disk backup destination.
5. Tansu tunnel retirement at cutover.
