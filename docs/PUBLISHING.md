# Publish

`stuff publish cargo|npm|pypi|oci|mirror` is the normal entry point. `stuff publish --help` is authoritative for its grammar, options, and exit codes. A publish reads the live generation and manifest, prepares the requested packages against them, validates the complete result, uploads only missing objects, promotes `current` with a conditional write, reads back, and reports; a request whose bytes are already published reports `unchanged` and promotes nothing.

## Check prerequisites

| Requirement | Use | Check |
| --- | --- | --- |
| `cf` signed in, unless `R2_S3_*` is supplied | Mint temporary R2 credentials | `cf --profile stuff auth whoami` |
| Publication state | Extend the live registry instead of replacing it | A publish reads the live generation; local state is optional |
| Python 3.11+ | Run the kept R2 maintenance scripts and the `mycrates` oracle | `python3 --version` |
| Rust nightly | `cargo publish` for crates | `cargo --version` |
| Node and npm | `mynpm` fetches npm packages | `node --version` |
| uv and CPython 3.15 | `mypypi` resolves and builds wheels | `uv --version` |
| `auditwheel` or `delocate` | `mypypi` repairs native wheels | `deploy/provision-mypypi-repair.sh` installs the pin |
| `sandbox-exec` (macOS, built in) or `bwrap` (Linux) | Confine sdist build backends and wheel import checks | `bwrap --version`; the container needs the flags in the mypypi README |
| Network | Fetch crates, npm tarballs, and Python wheels | |
| R2 S3 token (optional) | Long-lived credentials, conditional promotion, and multipart uploads over 300 MB | `R2_S3_*` is set, or `cf` mints temporary credentials |
| OCI writer host (optional) | Stream authenticated pushes through signed upload URLs | `deploy/myoci-writer.sh` runs with the shared secret and R2 credentials |

`stuff` mints R2 credentials through the `cf` profile named by `cf_profile` in its configuration (default `stuff`) and passes `--profile` on every `cf` call, so `make publish-*` works from any directory without a directory binding. Create the profile once with `cf auth create stuff`; check it with `cf --profile stuff auth whoami`. The kept maintenance scripts (`r2-pull.py`, `r2-gc.py`) mint through `stuff credentials`, so they use the same profile.

`stuff` keeps configuration (`cf_profile`, the per-registry `bucket` and `public_base_url`, and `cargo.download_base_url`) in `${XDG_CONFIG_HOME:-$HOME/.config}/stuff/config.json`, a disposable cache in `${XDG_CACHE_HOME:-$HOME/.cache}/stuff/<registry>`, and durable state in `${XDG_STATE_HOME:-$HOME/.local/state}/stuff/<registry>`; `STUFF_CONFIG_HOME`, `STUFF_CACHE_HOME`, and `STUFF_STATE_HOME` override each. The kept maintenance scripts use the same state root. Back up the state directories. The live generation, not local state, is the publication source of truth.

## Publish a registry

Run `stuff publish` from the repository that owns the packages:

```sh
stuff publish cargo -p crate-a -p crate-b

stuff publish cargo --lockfile Cargo.lock

stuff publish npm --lockfile pnpm-lock.yaml --project my-project

stuff publish npm --tarball joshuarli-ui-0.1.0.tgz

stuff publish pypi six==1.16.0

stuff publish pypi --wheel dist/laputa_buildlib-0.1.0-py3-none-any.whl
```

`stuff publish cargo` publishes the `-p` crates in the order given. Each crate's verification resolves a dependency on an earlier crate of the same run from the registry being published to, so list dependencies before their dependents; the mirror need not hold the new versions. A crate version already in that registry's index, including one a failed earlier run recorded locally but did not promote, is skipped, so rerunning the same command completes the remaining crates.

`--dry-run` stages the requested change without uploading or promoting. `stuff` prepares against the live generation rather than replacing it with a local subset. Validation and drop checks precede uploads; an upload failure can leave unactivated objects. Keep one publishing machine per registry, as described under conditional promotion below. PyPI acquisition can currently populate persistent wheel state during a dry run; the candidate/cache separation issue is tracked in [TODO.md](../TODO.md).

Every writer of a registry holds one lock file: `$STUFF_STATE_HOME/<registry>/publish.lock` for `cargo`, `npm`, and `pypi`, and `$STUFF_STATE_HOME/oci/transaction/publish.lock` for OCI publish and rollback. `stuff lock-path BUCKET` prints it for any bucket in the configuration, including custom names and the OCI blob bucket; `stuff lock-path --registry NAME` prints it by registry. `r2-gc.py`, `r2-gc-blobs.py`, and `mypypi-container.sh` resolve their lock through it and fail on an unconfigured bucket, so the `stuff` on `PATH` (or `$STUFF`) must include `lock-path`; `make install` rebuilds it into `$PREFIX/bin` (default `~/.local/bin`).

`stuff publish pypi` mirrors wheels for six targets: `linux-x86_64`, `linux-aarch64`, `linux-musl-x86_64`, `linux-musl-aarch64`, `macos-x86_64`, and `macos-arm64`. The musl targets take `musllinux_1_0` through `musllinux_1_2` wheels only, so Alpine hosts install from the mirror; a package with no musllinux wheel and no source archive fails the publish, and one with only a source archive needs `--allow-sdist`. The publishing host's native target is `linux-musl-<arch>` when its Python interpreter links against musl, and `linux-<arch>` otherwise. Adding the musl targets adds wheels and manifest targets without changing existing simple pages, but a `stuff` binary from before this change fails planning against a live manifest that records a musl target (`manifest target is not configured`), so upgrade every publishing host before the first publish that includes them.

PyPI publication inspects foreign-target wheels statically and executes native-target installation/import checks. Use matching runners or `deploy/mypypi-container.sh` for foreign-platform execution checks; see [crates/mypypi/README.md](../crates/mypypi/README.md).

Package code runs only inside an execution boundary. Sdist build backends (`--allow-sdist`) run under `sandbox-exec` on macOS or `bwrap` on Linux with no network; their `build-system.requires` are resolved first in a confined prime stage that pins the hash-pinned build-constraints set (`deploy/build-constraints.txt`), so an unpinned backend fails instead of fetching. The import check of every new native wheel runs in the same boundary, also with no network. If the sandbox tool is missing or cannot start, the publish fails with `execution boundary unavailable` (exit 1) before any package code runs, and nothing is promoted. R2 credentials never leave the `stuff` process. The grants, host requirements, and residual risks are in the [mypypi README](../crates/mypypi/README.md#execution-boundary).

To ingest a whole fleet locally, `deploy/fleet-ingest.sh OUT_DIR --cargo LIST --npm LIST --pypi LIST` (`make fleet-ingest`) runs each registry's import, fetch, verify or validate, and publish into `OUT_DIR/{mycrates,mynpm,mypypi}/site`. Lists hold Cargo.lock paths, pnpm-lock.yaml paths, and `name==version` requirements. It never writes to R2; publish through `stuff publish`.

`stuff publish cargo --lockfile PATH` (repeatable) extends the crates.io mirror view (`current/crates-io/`) with every crates.io package the locks pin; it does not combine with `-p`, `--manifest-path`, or `--registry`. Git, path, and other-registry entries are skipped (`--verbose` lists them). A package neither the local state nor the live generation holds is taken from `https://index.crates.io/` with its exact index line; its archive comes through the index's `dl` template and must match both the lock checksum and the index `cksum` before it is scanned and stored, or the run fails with exit 4 and promotes nothing. Packages already mirrored are not downloaded again, and a run that adds nothing reports `unchanged`. `repomod deps mirror` drives this, `stuff publish npm --lockfile`, and `stuff publish pypi` for the fleet; see [fleet](fleet.md#dependency-upgrades).

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

## Publish a first-party Python package

`stuff publish pypi --wheel PATH.whl` hosts a pure-Python package that exists nowhere upstream beside the mirror. Build it first with `uv build --wheel` or `pip wheel` (`--wheel` may repeat); `stuff` reads the wheel and never runs a build backend or imports the package. `--wheel` and `SPEC`/`--allow-sdist` are separate modes and do not combine.

```sh
(cd ../buildlib && uv build --wheel --out-dir /tmp/wheels)
stuff publish pypi --wheel /tmp/wheels/laputa_buildlib-0.1.0-py3-none-any.whl --dry-run
stuff publish pypi --wheel /tmp/wheels/laputa_buildlib-0.1.0-py3-none-any.whl
```

The publish extends the live generation exactly as a mirror publish does, so every mirrored wheel stays. The wheel is stored at `wheels/<filename>` with its `.metadata` sidecar and served by the same simple pages as a mirrored wheel, including `#sha256=`, `data-requires-python`, and `data-core-metadata`. Each wheel is inspected statically for every configured target (METADATA name and version against the filename, `WHEEL` tags, `RECORD` hashes and sizes, unsafe members, native binaries) and must carry only `none-any` tags; a platform wheel exits 2. The wheel is not installed or imported, so its dependencies need not resolve at publish time.

Rules, all exit 3 on violation:

- A first-party `name==version` is immutable. The same wheel reports `unchanged`; different bytes, or a different wheel filename for the same version, conflict.
- A first-party name cannot reuse a name the mirror hosts.
- A later mirror publish cannot add another version of a first-party name.

First-party wheels live in the generation manifest as rows marked `"first_party": true`, with no local state of their own, so later mirror publishes and `stuff republish pypi` carry them forward and a machine with no state sees them. Rows of mirrored wheels are unchanged, so existing generations and their ids are unaffected; older binaries reject a manifest that contains a first-party row. Consumers install by version:

```sh
uv pip install --index-url https://pypi.17166969.xyz/current/simple/ laputa-buildlib==0.1.0
```

A mirror publish resolves its specs against upstream PyPI, so a spec or dependency naming a first-party package fails to resolve there (exit 2) instead of using the hosted wheel.

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

stuff publish oci --inventory deploy/oci-mirror.toml --lock deploy/oci-mirror.lock

stuff republish oci
```

An empty `stuff-oci` bucket makes the first publication.

The mirror input mode publishes the entries from an inventory file and
their lock. The inventory pins a source by digest or tag; `myoci mirror
resolve --config <inventory> --lock <lock>` refreshes the lock, verifying
roots, graphs, platforms, and referrers, then writes the lock atomically.
A publish with `--inventory` and `--lock` never resolves tags: it acquires
exactly the locked descriptors, verifies the batch, applies the declared
aliases in one recoverable transaction, renders the generation, and
promotes it. It is mutually exclusive with `--image` and `--layout`. The
published generation carries a deterministic `provenance.json` sidecar
recording the inventory fingerprint, policy, resolved platforms, and
applied aliases; roll back generations as usual with
`stuff rollback oci <generation>`. Acquiring the batch does not enforce
pinned aliases alone: an unmanaged alias whose bytes differ from the lock
aborts the publication before anything is promoted, and no alias moves.
The publish prepares local state and site first, then syncs blobs and
metadata, then promotes `current` conditionally; a failure before
promotion leaves the previously promoted generation untouched, and the
report separates local preparation from remote promotion. Keep one
publishing machine per registry: a home pinned to an older live generation
is refused with exit 3 instead of overwriting a competing publication.

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

Blob GC keeps everything younger than `--grace-minutes` (default 60, the
upload session lifetime): recent blobs, generations whose manifest is recent,
and the `blobs.json` of a recent generation still awaiting its manifest. A
recent generation without `blobs.json` blocks blob deletion entirely. Before
each delete batch it re-reads the metadata bucket and aborts if `current`, a
manifest, or the generation set changed. `--grace-minutes 0` is only safe
when no network push or publication can be in flight. Retain the blobs for
every generation you need to restore: `stuff rollback oci`, including
`--dry-run`, reads the target's `blobs.json` and refuses, with `current`
unchanged, when any referenced blob is missing from the blob bucket.

The registry is sha256-only. Limits: repository name 255 characters, tag 128
characters, manifest 4 MiB, upload session one hour, one upload 20 GiB.
Reads are anonymous. Writes require `Authorization: Basic` with the registry
push token; delete endpoints are not implemented.

### Push writer

The checked-in Worker configuration supplies no `STUFF_OCI_PUSH_TOKEN`, `STUFF_OCI_PUSH_SECRET`, or `STUFF_OCI_WRITER_ORIGIN`, so writes return the OCI `405 UNSUPPORTED` error. Use `stuff publish oci`. The optional network-push path has open activation and snapshot-preservation defects in [TODO.md](../TODO.md); resolve those before enabling it. Its setup uses the writer below on a TLS host and those three Worker settings.

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

## Operate the OCI mirror

The mirror copies declared upstream images into this registry with locked,
verified descriptors into a reproducible local state. Commands run from the
repository root; the inventory and lock live in `deploy/oci-mirror.toml` and
`deploy/oci-mirror.lock`. myoci exit codes: `2` usage, `3` integrity,
`4` conflict, `5` limit, `6` network, `1` other.

### Add or change an entry

1. Add the `[[entries]]` block to `deploy/oci-mirror.toml`: a `source`
   reference (tag or digest; Docker Hub shorthand is normalized), a
   `destination` repository, `aliases`, `platforms`, and `referrers`
   (`snapshot` to observe referrers, `none` to skip discovery). Each
   destination may appear once across the inventory; two entries cannot
   write one destination.
2. Regenerate the lock:
   `myoci mirror resolve --config deploy/oci-mirror.toml --lock deploy/oci-mirror.lock`.
   It verifies the root, the full graph (nested indexes, configs, layers,
   embedded attestations) and referrer discovery, then writes the lock
   atomically; one failed entry aborts the whole resolution and nothing is
   written. Attachments whose media types are not manifests or indexes are
   not recorded; resolve prints how many were skipped.
3. Review the lock diff. Commit inventory and lock together.

### Refresh a selected entry

`myoci mirror resolve --entry manylinux-amd64 --config deploy/oci-mirror.toml --lock deploy/oci-mirror.lock`
refreshes one entry and preserves every unselected entry byte for byte;
unrelated provenance and ownership carry forward.

### Review the plan

`myoci mirror plan --config deploy/oci-mirror.toml --lock deploy/oci-mirror.lock --state-dir <state> [--json]`
reports desired aliases, local conflicts (aliases unmanaged at a different
digest, managed aliases moved by hand, adopted identical unmanaged aliases),
missing objects (present, missing, total), and deduplicated transfer bytes.
It is local and read-only, never initializes the state, and refuses to
proceed while a mirror transaction is pending — the message says which
command recovers it. A missing or stale transfer cache degrades estimates
to `unknown`, never zero.

### Locked sync

`myoci mirror sync --locked --config deploy/oci-mirror.toml --lock deploy/oci-mirror.lock --state-dir <state> [--json]`
acquires exactly the locked digests (never a tag, never an implicit
upgrade), verifies digests and sizes, and commits aliases into
`mirror.json` plus `provenance.json` as one recoverable transaction. A tag
that moved upstream since `resolve` does not change what is acquired.
`Exit 4` with `conflict-unmanaged`: an alias at the destination is not
managed by the mirror and points at a different digest — inspect the
destination, then add the destination to the inventory intentionally or move
the tag by hand; nothing moves. `Exit 4` with `conflict-stale`: a managed
alias was moved by hand away from the last applied mapping — same recovery,
but decide deliberately before re-resolving. Foreign layers and identical
content across entries transfer at most once; acquired immutable objects may
remain after a failure.

### Offline replay

Add `--offline` to replay purely from local state:
`myoci mirror sync --locked --offline --config ... --lock ... --state-dir <state>`.
It succeeds only with complete local content and otherwise fails naming the
missing objects; it performs zero network requests.

### Publish

`stuff publish oci --inventory deploy/oci-mirror.toml --lock deploy/oci-mirror.lock`
validates the batch, acquires and verifies the locked descriptors, commits
local metadata, renders, syncs blobs and metadata, and promotes `current`
conditionally. It never resolves tags. An inventory with zero entries is
refused with a usage error; an empty generation is never promoted. The
generation carries a deterministic `provenance.json` sidecar. The publishing
machine first reconciles its local state with the live generation by adopting
into a fresh directory and swapping it in, so a concurrent command never
observes a half-rebuilt state; the previous directory is removed only after
the swap. A failure before promotion leaves
the previously promoted generation untouched; the report separates local
preparation from remote promotion. The `stuff-oci-layout` and `--image`
modes stay mutually exclusive with this one.

### Repair missing content

A missing local object: run `myoci mirror plan` to see it, then re-run the
locked sync; acquisition re-fetches exactly the missing digests.
Interrupted acquisitions can leave `mirror-*.tmp` files under `.uploads/`;
they are swept automatically once older than two hours. A missing
remote object: `stuff publish oci --inventory ... --lock ...` re-uploads it
and repairs the generation before reporting success; the previously promoted
generation keeps serving until then. Conflicting immutable remote objects
fail before any upload (exit 3) and `current` stays put; resolve which side
is wrong, never force past the conflict.

### Adopt a generation

`myoci adopt --state-dir <state> --site-dir <site>` rebuilds state from a
published generation. The `provenance.json` sidecar restores the managed
alias ownership; generations without a sidecar adopt cleanly with no
managed aliases, and the next locked sync adopts identical aliases back
into managed ownership. Finish with `myoci verify --state-dir <state>`.

### Roll back

`stuff rollback oci <generation-id>` verifies the target generation's
metadata and every referenced blob (against `stuff-oci-blobs`) before moving
`current`; `--dry-run` verifies without promoting. Retained generations are
adoptable and republishable. Missing referenced blobs make the rollback
refuse with `current` unchanged.

### Initial publication and consumer cutover

Initial publication and consumer cutover are operator steps, not CI:

1. Publish with the mirror mode; note the promoted generation.
2. Verify the live registry:
   `oras manifest fetch oci.17166969.xyz/<repo>:<alias>` and a blob pull,
   plus `stuff doctor --sample-artifacts`.
3. Cut consumers over repository by repository, pinning the mirrored
   digest or alias, verifying one consumer before the next. Do not cut a
   consumer over before the promoted generation verifies.
## Publish the static mirror

`stuff publish mirror` re-hosts static artifacts the `~/usr` installer downloads — Node.js, Codex, OpenCode, installer scripts, and dwl — under `mirror.17166969.xyz` as one homogeneous format: every pinned upstream is extracted, filtered to a list of members that matter, and re-zipped once. Consumers never see tarballs or zips with unknown layouts, only `<id>.zip`. The manifest lives in [`mirror/manifest.toml`](../mirror/manifest.toml); [crates/mymirror](../crates/mymirror/README.md) documents the format. GitHub-hosted artifacts are deliberately not mirrored yet.

```sh
stuff publish mirror --dry-run
stuff publish mirror
```

Each entry pins an upstream URL, an upstream SHA-256, and an optional keep list (`keep = ["bin/**", "lib/**"]`) applied after stripping leading components. The build downloads the upstream once, verifies the SHA-256 before anything else happens, extracts (`.tar.gz`, `.tar.xz`, `.zip`, or a raw file), drops members outside the keep list, and writes a deterministic DEFLATE zip (fixed timestamps, Unix file types and modes, symlink targets that stay inside the archive root). The generation contains `<id>.zip` per entry plus `catalog.json` (schemaVersion 1, with the standard `packages` list used by the drop guard) and `SHA256SUMS`; `current` selects the live generation like the other registries. Downloads are cached by upstream SHA-256 under `~/.cache/stuff/mirror/objects` and re-verified on every reuse. When the live marker already names the generation derived from the manifest and every expected remote object is present, the publish rebuilds nothing: it only ensures the local `current` symlink points at that generation and reports `unchanged`, so a republish of an unchanged manifest downloads nothing, uploads nothing, and touches no upstream.

Because bumping an artifact's pinned `version` removes the old row from `packages`, the shared drop guard treats a version bump as a removal: `stuff publish mirror` exits 3 and lists the dropped pins. Pass `--allow-drops` when the bump is intended — this is the one publish verb that accepts the flag; `stuff republish mirror` also rebuilds from the manifest (with `--allow-drops` like the other registries and `--manifest PATH` to use a non-default manifest), and `stuff rollback mirror GENERATION` restores a previous generation. The bucket is `stuff-mirror` (configurable as the `mirror` section in `config.json`, like `npm`), the publish is a normal transaction against it, and the Worker custom domain `mirror.17166969.xyz` serves it like the static registries. The OCI inventory mode is separate and unrelated: it re-hosts container images through `stuff publish oci --inventory`.

The mirror's `catalog.json` carries the artifact manifest itself (`artifacts` with upstream URL and upstream SHA-256, `baseUrl`, `renderer`, `schemaVersion`, `registry: "mirror"`) alongside the standard `packages` list, so the drop guard only needs the packages section and `stuff doctor`, `stuff ls`, and `stuff render` all understand the mirror like any other registry: `stuff doctor --registry mirror --sample-artifacts` checks the immutable catalog, that `current/SHA256SUMS` lists the first artifact, and hashes that artifact against it.

## Republish without new packages

A renderer or catalog change reaches a registry's pages only when that registry publishes again. `stuff republish cargo|npm|pypi|oci` does that without package inputs: it reads the live generation, renders it again under the current renderer, and promotes it. `stuff republish mirror` re-derives the mirror generation from the manifest instead — cached upstream downloads make that an unchanged no-op when the manifest did not change. Unchanged output reports no new generation. The bucket keeps older manifests until `make gc` prunes generations outside its retention window.

Republish uses the observed live generation as its promotion base even when this machine's last promoted generation is older. If another publisher changes `current` during the operation, republish refuses the competing update; rerun it to render the new live generation.

```sh
stuff republish cargo
stuff republish npm
stuff republish pypi
```

`make deploy` runs the whole rollout in order. It builds one Worker bundle, tests it, and deploys that bundle with `cf deploy --prebuilt`. It then runs the four republishes in parallel — `republish-mycrates`, `republish-mynpm`, `republish-mypypi`, and `republish-mirror` (plus `republish-oci` with `OCI_DEPLOY=1`). The mirror republish is an unchanged no-op unless the pinned manifest changed; then `make smoke-deployed`. Each publish mints its own temporary S3 credentials through `cf` when `R2_S3_*` is absent. Set `DEPLOY_JOBS=1` to serialize them. A failure stops the rollout before the live check.

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

A machine with no local state reads the live generation and manifest, verifies the objects it uses against listing size and ETag, and prepares against that snapshot. Use the single-writer restriction above; a fresh local cache does not establish ownership against another publishing machine.

`deploy/r2-pull.py BUCKET DEST` only reads the bucket. It fetches the current generation and the shared stores and checks each object against its listing ETag, using the multipart ETag algorithm for objects uploaded in parts. After the download it checks every entry of the generation's manifest (key, size, ETag) against the pulled tree; a generation published before manifests existed is pulled without that check. With `R2_S3_*`, or with credentials minted through `cf`, it lists through the S3 API and downloads `R2_PULL_JOBS` objects in parallel. `R2_PULL_CF_ONLY=1` forces the paced `cf` transport.

The Python `mycrates` adoption sequence (`mycrates init`, `adopt`, and `verify`) belongs to the legacy oracle and its golden tests; it is not a publishing entry point.

## Upgrading mycrates state from the Rust tool

The Rust `mycrates` stored native records and pins as TOML; the Python tool stores JSON and refuses a state directory with TOML records left in it. Once, per state directory, with a backup in hand (`cp -a "$H/mycrates" "$H/mycrates.bak"`), run:

```sh
crates/mycrates/bin/mycrates migrate-state --state-dir "$H/mycrates/state"
```

`stuff publish cargo` has its own automatic migration: it backs up the selected legacy state, copies records and pins into its `cargo` state directory, and converts legacy TOML records. Source selection, conflicts, and backup paths are documented in [mycrates state migration](mycrates-migration.md). The standalone command above converts a Python oracle state directory in place.

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

`pnpm run build` writes the Cloudflare Build Output Specification under `.cloudflare/output/v0`, and `cf deploy --prebuilt` uploads it without rebuilding (see [crates/stuff-worker/README.md](../crates/stuff-worker/README.md)). Use `--provision` only when adding a bucket. Custom domains come from `domains` in `cloudflare.config.ts`.

The Worker uses Workers Caching. The default entrypoint is an uncached gateway. The `Registry` entrypoint caches responses under a key that includes the host, so the four domains cannot share cache entries. Immutable artifacts carry a one-year TTL. Mutable generation views carry 60 seconds, so a promotion becomes visible within that window without a cache purge. The gateway costs one extra worker-to-worker request per request; a cache hit skips `Registry` but still runs the gateway. Two requests to the same URL should show `cf-cache-status: HIT` on the second. Range requests are sliced from the cached full object, so a cold range fetch downloads the whole artifact. On a cache miss, `Registry` still reads each `current` pointer, so the 60-second window holds. It starts the object read for the pointer value its isolate last saw in parallel with the pointer read, and discards that read when the pointer has moved. The unified page and `llms.txt` keep their rendered body in the colo cache under the Worker version and the four generation ids, so they render again only after a deploy or a promotion.

## Edge protection

The registry hosts serve package clients, not browsers, so they must never receive an
interactive challenge. Keep these zone settings for `17166969.xyz`:

- Bot Fight Mode and Browser Integrity Check off.
- Security Level `medium`, with the Cloudflare managed WAF ruleset enabled.
- The `/ingest/` rate limit at 20 requests per 10 seconds per source IP and colo; do not
  rate-limit or challenge the read paths.
- No Managed Challenge, JS Challenge, or Turnstile rule on `crates.`, `npm.`, `pypi.`,
  `oci.`, `stuff.`, `oci-blobs.`, `crates-blobs.`, or `mirror.`.

Cargo downloads `.crate` files from `crates-blobs.17166969.xyz`, an R2 custom domain
on `stuff-mycrates`, so they never invoke the Worker. Each generation's
`config.json` sets `dl` to `<download_base_url>/blobs/{sha256-checksum}.crate`, and
`cargo.download_base_url` defaults to that host. A config that sets
`cargo.public_base_url` without `download_base_url` keeps downloads on the public host.
The host needs two zone rules. The `stuff cache rules` Cache Rule caches
`/blobs/*` for a year. The custom firewall rule "crates-blobs serves only blob
objects" blocks every other path and method, because the bucket also holds
`current`, `generations/`, `manifests/`, and `_probe/`. Generations published before
the change keep `dl` on `crates.17166969.xyz`, and the Worker still serves
`/blobs/` for them.

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

This builds the `stuff` binary and runs `deploy/deployed-smoke.sh`. The check is read-only and needs no Cloudflare credentials. It runs `stuff doctor --sample-artifacts` against the active generations, catalog objects, and sample artifact hashes, plus `npm view`, `uv pip install --dry-run`, and `oras manifest fetch` probes, and verifies the static artifact mirror by downloading the first `SHA256SUMS` artifact and hashing it.

## Roll back a generation

```sh
stuff rollback cargo <generation-id>
```

`stuff rollback` verifies every object in the target generation's manifest before moving `current`, and records the target as this machine's promoted generation. For OCI it also HEADs every digest in the target generation's `blobs.json` against the blob bucket and refuses the rollback, with `current` unchanged, when any referenced blob is missing. `--dry-run` performs the verification without promoting. To roll forward, run the same command with the newer generation id; gc keeps generations newer than `current` by default, so the target survives.

## Reclaim storage

`make gc` deletes R2 generations outside its retention window and shared-store objects no retained manifest references, then prints per-bucket and total storage before and after. It keeps `current` plus `GC_KEEP_PREVIOUS` older generations (default 1) and `GC_KEEP_NEWER` newer generations (default `all`). Preview with `GC_DRY_RUN=1`, and restrict buckets with `GC_BUCKETS`.

The command mints bucket-scoped credentials per bucket, so the usual `cf` login is enough. Because manifests are deleted with their generations, rollback and roll-forward only reach retained generations.

Generation GC holds its helper lock while planning and deleting; the package-registry lock mismatch above still requires separate scheduling from Rust publishers. It deletes manifests first, then generation trees, then shared objects. Before every batch it re-reads `current` and the manifest set and stops if either changed. It also defers shared-store deletion while a recent generation directory has no manifest. These checks belong to `r2-gc.py`; the separate OCI blob collector lacks that incomplete-generation protection.

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
| `generation ... drops N versions served by live generation ...` | The proposed catalog lacks served versions | Re-plan against the current publication; use explicit rollback for a deliberate generation switch |

## Copy a registry bucket to another store

`deploy/store-copy.py copy SOURCE DEST --ledger FILE` copies every object of a bucket to another
S3-compatible store and records each verified object in a JSON Lines ledger (key, size, SHA-256,
source and destination ETags). Source credentials come from `STUFF_COPY_SOURCE_S3_*` or, when unset,
`stuff credentials SOURCE`; destination credentials come from `STUFF_COPY_DEST_S3_*`. Objects stream
through a temporary file. A multipart object is re-uploaded with the source's part sizes (read with
`HEAD ?partNumber=N`, or the 64 MiB publication part size), so its ETag and every manifest that
records it stay valid; the ledger's `etagPreserved` says whether that held. A rerun skips ledger
entries whose source and destination still match and re-copies changed sources.

`deploy/store-copy.py verify SOURCE DEST --ledger FILE [--deep]` compares both listings, sizes and
ETags with the ledger and, with `--deep`, re-downloads the destination and checks SHA-256. It exits 1
naming each missing, extra, changed or corrupted key. `make test-store-copy` proves both offline.

### Publication coordinator (not enabled)

The Worker contains an experimental per-registry Durable Object. It is disabled unless
`STUFF_COORDINATOR_ENABLED` is `true`; do not set that flag in production yet. Its
SQLite row owns the activation revision, generation, manifest SHA-256, and pending
projection state. An activation transaction also stores its idempotency receipt.
After the SQLite commit the owner writes R2 `current` as a compatibility projection;
it compares the projection with authority and repairs drift before responding or
serving a coordinator-backed mutable pointer. SQLite and R2 are not one atomic
transaction. A request still in flight when an actor dies may finish late; all
mutable reads use SQLite authority, and the next coordinator operation repairs
the projection. Direct reads of the compatibility marker must be fenced at
cutover. Immutable
downloads continue to use R2 directly.

The authenticated management endpoint is `/_publication/<registry>/<operation>`
on the stuff host. Registries are `cargo`, `npm`, `pypi`, `oci`, and `mirror`.
Requests use `Authorization: Bearer <STUFF_COORDINATOR_TOKEN>`; the token is a
server secret. `GET current` returns `revision`, `generation`, `manifest`, and
`pending`. All other operations use POST with JSON. `begin` and `gc-claim` return
an exclusive five-minute lease token, expiry in Unix milliseconds, and the current
revision. `renew` and `abort` accept `{"token":"..."}`. `activate` and `rollback`
accept `token`, `key` (an idempotency key), `generation`, `manifest` (lowercase
SHA-256 of the exact manifest bytes), and `expectedRevision`. Both verify the
existing version-1 manifest and its metadata-bucket objects before committing. A
replay of the same key and operation payload returns its original receipt; reuse
with another payload conflicts. A fresh request with a stale revision or lease
conflicts. `gc-delete` accepts a GC `token` and `generation`: it only deletes the
manifest and generation tree, refuses the current generation and references from
other manifests, and does not collect shared objects. The owner serializes these
operations and checks lease expiry before activation and each delete. Before
issuing any delete, it durably retires that generation. Retirement is permanent:
neither the generation nor references to its tree or manifest may be activated
again, even if a crashed collector's deletion is still in flight. Re-uploading
its manifest cannot undo retirement. This makes late external completion safe;
the immutable generation ID must never be reused for different bytes.

The next integration must replace native publication, rollback, and both GC
scripts' direct mutations with this API, and remove or route OCI push's direct
activation. Native uploads should retain their existing streaming and multipart
implementation: issue narrowly scoped, short-lived upload credentials or a gateway
that permits only immutable keys associated with an active lease, refuses `current`,
manifest replacement and deletion, and prevents a stale upload from completing
after its lease is superseded. A streaming proxy for every object would put the
actor on the data path and sacrifice the native multipart design. OCI activation
also checks every declared blob in the separate blob bucket. GC must extend its
durable retirement protocol to shared objects and blob retention. Until those controls exist,
unrestricted R2 credentials and the legacy writers can bypass this authority;
lease checks alone cannot protect an external request already in flight at a crash.
