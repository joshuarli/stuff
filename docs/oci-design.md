# OCI registry design

`stuff` serves OCI Distribution 1.1 as its fourth registry. Reads are
anonymous; writes use one registry token and a separate blob writer. The
implementation lives in [`stuff-oci`](../crates/stuff-oci/README.md) and
[`myoci`](../crates/myoci/README.md), with the Cloudflare Worker sharing the
same protocol core.

This document is the durable design. Deployment and operations are in
[PUBLISHING.md](PUBLISHING.md); the implementation evidence and open tickets
are in [oci-benchmarks.md](oci-benchmarks.md) and [TODO.md](../TODO.md).

## Architecture

- `crates/stuff-oci` is the I/O-free, `wasm`-safe protocol core: digests,
  references, media types, manifest/index parsing, storage keys, request
  parsing, response plans, OCI errors, and upload-session tokens.
- `crates/myoci` is the CLI and native server: import, inspect, publish,
  verify, adopt, tag, `serve`, `writer`, and `bench`.
- The Cloudflare Worker serves the read API from R2 and accepts push requests
  through the same `stuff-oci` plans.
- A separate `myoci writer` owns only blob bytes. The front owns
  authentication, manifest validation, generation creation, and the
  `current` flip. The writer never sees registry credentials.
- `stuff-catalog` renders OCI into catalog schema 4, HTML, and `llms.txt`
  alongside the other registries.

The wasm bundle must not inherit the CLI's HTTP client, so only `stuff-oci`
is shared between the native server and the Worker.

## Read API

| Request | Required behavior |
| --- | --- |
| `GET /v2/` | 200 with `Docker-Distribution-Api-Version: registry/2.0` |
| `GET`/`HEAD /v2/<name>/manifests/<tag|digest>` | Resolve the tag; serve the stored media type and `Docker-Content-Digest`; 404 `MANIFEST_UNKNOWN` |
| `GET`/`HEAD /v2/<name>/blobs/<digest>` | Serve blob bytes with `Docker-Content-Digest`; ranges return 206/416; 404 `BLOB_UNKNOWN` |
| `GET /v2/<name>/tags/list` | Sorted tags with `n`/`last` and a `Link` header when more remain; 404 `NAME_UNKNOWN` |
| `GET /v2/<name>/referrers/<digest>` | Precomputed OCI index, `artifactType` filter, pagination; empty index for an unknown subject |
| Any other `/v2` path | 404 `NAME_UNKNOWN` |
| Malformed name/digest/tag | 400 with the matching OCI error code |

Manifest GET and HEAD return the stored media type even when the request's
`Accept` list omits it. Strict negotiation would turn valid indexes into false
404s, and clients accept either OCI or Docker schema 2 families. The stored
type is authoritative.

A manifest request reads at most the generation pointer, one
`repository.json`, and one manifest object. A blob request reads only
`sha256/<hex>` from the blob bucket. The serving path does not list, scan,
hash, or parse.

## Push API

| Request | Required behavior |
| --- | --- |
| `POST /v2/<name>/blobs/uploads/` | 202 with a signed writer `Location` and `Docker-Upload-UUID`; a mount hit returns 201 |
| `PATCH`/`PUT` upload | 202/201; streamed; digest and size verified; 400 `DIGEST_INVALID` on mismatch |
| `GET`/`DELETE` upload | 204 with `Range`; cancel 204 |
| `PUT /v2/<name>/manifests/<tag|digest>` | 201 with digest and `OCI-Subject` when present; verifies blobs/children; creates a generation; retries conflicts |
| Auth | Basic on every write; 401 challenge; constant-time comparison |

Delete endpoints stay unimplemented and the conformance delete flags stay off.

## Media and digests

Accept OCI image manifests and indexes, Docker schema 2 manifests and lists,
OCI configs, OCI gzip/zstd layers, Docker gzip layers, in-toto layers,
arbitrary `artifactType` values, and non-distributable layers that carry
`urls` (their bytes need not be stored). Reject Docker schema 1 and every
non-`sha256` digest. A zstd layer is stored and served unchanged.

## Storage and generations

Metadata bucket `stuff-oci`:

- `current` -> `generations/<id>`
- `generations/<id>/repositories/<repo>/repository.json` (tags and every
  manifest descriptor)
- `generations/<id>/repositories/<repo>/referrers/sha256/<subject>.json`
  (an OCI index)
- `generations/<id>/blobs.json` (sorted digests reachable from the view)
- `generations/<id>/catalog.json`, `index.html`, `llms.txt`
- `oci/manifests/sha256/<hex>` (shared immutable manifest bytes)
- `manifests/<id>.json` (sync/GC manifest, one per generation)

Blob bucket `stuff-oci-blobs` holds only immutable `sha256/<hex>` objects. A
CDN can expose it without exposing metadata, GC prunes it independently, and a
1 GiB layer never enters a metadata generation.

A tag is a mutable reference inside an immutable generation. A publish or
push that changes any tag, manifest, or referrer produces a new
content-derived generation (`sha256` over the canonical repository view, first
24 hex). Re-publishing the identical view is a no-op. A tag move is activated
atomically with `current`; rollback is
`stuff rollback oci <id>`.

The push front checks referenced blobs and child manifests before constructing
a generation. It rebuilds from a complete snapshot of the current generation:
every listing page, every repository view, every repository's referrer
indexes, and `blobs.json`. A missing or malformed snapshot object fails the
push. Activation is a conditional `current` write against the source ETag, or
against absence for a first publication; a failed condition restarts from the
winning generation, and only a successful activation returns 201.
Native `stuff publish oci` uses its separate S3 transaction path.

## Catalog

`stuff_catalog::SCHEMA_VERSION` is 4 with `Registry::Oci`. One package per
repository, one version per tag, `latest` is the tag named `latest` otherwise
the lexicographically greatest tag, versions sort lexicographically,
`yanked=false`, and there are no dependencies. `stuff ls`, `search`, `render`,
`doctor`, the aggregate page, and `llms.txt` include OCI. Deploy the Worker
and `stuff` CLI before publishing OCI: schema 4 is unreadable by older
readers.

## Import

`myoci import --layout DIR --repository NAME [--tag TAG]` imports an OCI
image layout natively. `--from REGISTRY/REPO:TAG` runs pinned `oras` 1.3.6
into a scratch layout, then imports it. Import verifies every digest and
size, walks indexes recursively, retains attestation children and `subject`
referrers, and rejects a sparse index, a missing referenced blob, a
mismatched digest/size, a foreign layer, and schema 1. The tag comes from
`--tag`, else the layout's `org.opencontainers.image.ref.name`, else
`latest`.

## Push authentication and writer

The checked-in configuration leaves push off: the Worker has no push token, secret, or writer origin, and writes return `405`. This section describes the optional capability for a deployment that runs a writer host.

The Worker secret `STUFF_OCI_PUSH_TOKEN` (or `--push-token-file` locally) is
the registry token. Writes use `Authorization: Basic base64("stuff:" + token)`.
Missing or wrong credentials get 401 with `WWW-Authenticate: Basic
realm="stuff-oci"`; comparison is constant-time over SHA-256 digests.

The front issues an HMAC-signed, one-hour, repo-bound upload `Location` on the
writer. The writer verifies only that session (`STUFF_OCI_PUSH_SECRET`,
shared with the front), streams the upload to disk, verifies the digest, and
stores `sha256/<hex>` through `deploy/r2-put.py` or
`deploy/r2-multipart.py`. Worker and Docker do not forward `Authorization`
across hosts, so the signed URL is the writer's only authentication. This is
the bounded architecture for large layers; there is no Worker-body upload
path.

## Sync, GC, and rollback

Each generation carries `manifests/<id>.json`; `stuff publish oci` writes it,
and the Worker writes it during push from the ETags it
receives. Blob GC is separate: `deploy/r2-gc-blobs.py` unions `blobs.json`
from retained metadata generations and deletes unreferenced `sha256/*` from
the blob bucket, with the existing `--dry-run` preview. Generation GC and
rollback keep older generations readable.

## Limits

Repository name at most 255 characters matching the distribution grammar; tag
at most 128 characters matching `[A-Za-z0-9_][A-Za-z0-9._-]*`; manifest body
and descriptor at most 4 MiB; upload sessions live one hour; one upload at
most 20 GiB. Oversize or malformed input is a loud 4xx with the OCI error
code, never a truncated write.

## Conformance and benchmarks

The conformance gate pins `opencontainers/distribution-spec` commit
`97274622c11112caa21efb8c52acca3c6b8fa7f1`. The read suite runs with
`OCI_API_PUSH=false` and read-only data from `myoci inspect --json`; the push
suite runs with push on and deletes off against `myoci serve` plus the writer.
The gate passes when every enabled API/data row is `pass` or `disabled`;
`skip` is accepted only for the suite's optional "Blob post only" row, which
the pinned configuration does not enable.

`myoci bench` drives the HTTP API across manifest GET/HEAD at 1, 8, and 32
concurrency; blobs of 1 MiB, 64 MiB, 256 MiB, and 1 GiB cold and warm; and a
full multi-platform pull. Five interleaved runs report p50/p95/p99 and bytes
per second in [oci-benchmarks.md](oci-benchmarks.md). Deployed cold means a
freshly published blob digest; the 1 GiB row is expected to miss the Worker's 512 MB cacheable
object cap.

## Direct blob origin

When `STUFF_OCI_BLOB_ORIGIN` is set, blob GET/HEAD returns a 307 to
`<origin>/sha256/<hex>` with `Docker-Content-Digest`; otherwise blobs are
served from `OCI_BLOBS`. The deployed Worker sets it to
`https://oci-blobs.17166969.xyz` in `cloudflare.config.ts`, with a Cache Rule
on that host for `/sha256/*`. The deployed A/B matrix in
[oci-a11.md](oci-a11.md) adopted it: full-pull time fell about
40% at the median and 53% at p95, at the cost of about 100 ms on 1 MiB blobs.

## Security and operations

Reads are public and anonymous. There are no private images, per-repository
authorization, or token service. Writes use one token with no per-repository
scopes; that boundary returns if a second writer or private images arrive.
The writer host keeps only the shared HMAC secret and bucket-scoped R2
credentials. Local publish, sync, GC, and writer tests use `CF_LOCAL_STATE`
under `.work`; Worker tests use Wrangler's test harness with seeded R2 bindings; Docker
tests use `127.0.0.1`, which Docker treats as an insecure registry.

## Non-goals

Private images and per-repo authorization; a token service; enforcing a
Docker client version; pull-through or proxy mode; foreign layers without
`urls`; schema 1 and registry v1; `_catalog`; blob/manifest/tag delete; sparse
indexes; non-sha256 digests; server-side lazy-pull formats; image conversion,
decompression, or recompression; a database or Durable Object in the read
path; a Durable Object upload session; replacing npm/pnpm/uv orchestration;
Docker Hub pull-through.

## Contract surfaces

Catalog schema 4 and `registry:"oci"`; `myoci` CLI and flags; the `oci` and
`oci-blobs` hosts, `stuff-oci` and `stuff-oci-blobs` buckets, Worker bindings
(`OCI`, `OCI_BLOBS`, `STUFF_OCI_HOST`, `STUFF_OCI_PUSH_TOKEN`,
`STUFF_OCI_PUSH_SECRET`, `STUFF_OCI_WRITER_ORIGIN`,
`STUFF_OCI_BLOB_ORIGIN`); `stuff publish oci`; the `stuff-oci` and `myoci`
public APIs; and the writer's HTTP surface with its HMAC session token.
