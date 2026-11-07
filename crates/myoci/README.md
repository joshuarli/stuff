# myoci

The OCI Distribution 1.1 registry CLI and native server. It imports OCI image
layouts, publishes immutable generations, serves the read API, and accepts
authenticated pushes through a separate blob writer. Protocol logic lives in
[`stuff-oci`](../stuff-oci/README.md).

## State

`--state-dir DIR` contains:

- `manifests/sha256/<hex>`: verified manifest bytes.
- `repositories/<name>/repository.json`, `blobs.json`, and
  `referrers/sha256/<subject>.json`.
- `blobs/sha256/<hex>`: verified local copies of imported blobs.

`publish --site-dir DIR` writes generation-local repository views, catalog
JSON/HTML/`llms.txt`, and an `oci/manifests/sha256/<hex>` tree under the
generation. Metadata and blobs are synced to `stuff-oci` and
`stuff-oci-blobs`; `stuff publish oci` drives that rollout.

## Commands

```sh
myoci init --state-dir STATE
myoci import --state-dir STATE --layout tests/fixtures/oci/layout \
  --repository example/app --tag stable
myoci import --state-dir STATE --from docker.io/library/alpine:3.20 \
  --repository example/alpine --tag 3.20
myoci inspect --state-dir STATE [--json]
myoci verify --state-dir STATE
myoci publish --state-dir STATE --site-dir SITE [--public-url URL]
myoci adopt --state-dir STATE --site-dir SITE
myoci tag --state-dir STATE --repository NAME --tag TAG --digest DIGEST
myoci untag --state-dir STATE --repository NAME --tag TAG
```

`import --from` uses pinned `oras` 1.3.6 into a scratch layout. Import
verifies every digest and size, walks indexes, retains subject referrers, and
rejects sparse indexes, missing blobs, foreign layers, and schema 1.

## Serve and push

```sh
myoci serve --site-dir SITE [--bind ADDR] [--max-connections N]

myoci serve --site-dir SITE --state-dir STATE --push \
  --writer-url URL --push-token-file TOKEN --push-secret-file SECRET

myoci writer --state-dir STATE --bind ADDR --secret-file SECRET \
  [--r2-bucket stuff-oci-blobs --deploy-dir deploy \
   --public-origin https://oci.17166969.xyz]
```

The front owns Basic auth, manifest validation, generation creation, and the
conditional `current` flip. The writer owns only blob bytes and accepts
HMAC-signed, repo-bound upload `Location`s; it never sees registry
credentials. Without `--r2-bucket` the writer stores blobs in the local state
directory. The Worker push front uses the same `stuff-oci` plans and the
`STUFF_OCI_*` bindings documented in [PUBLISHING.md](../../docs/PUBLISHING.md).

## Benchmarks

```sh
myoci bench --base-url URL --repo NAME --tag TAG \
  [--output FILE] [--runs 5] [--label TEXT]
```

The matrix covers manifest GET/HEAD at 1, 8, and 32 concurrency, blobs from
1 MiB to 1 GiB cold and warm, and a full multi-platform pull. Results are
recorded in [docs/oci-benchmarks.md](../../docs/oci-benchmarks.md).

## Limits and exit codes

Repository names are at most 255 characters; tags at most 128 and match
`[A-Za-z0-9_][A-Za-z0-9._-]*`; manifests at most 4 MiB; upload sessions live
one hour; one upload is at most 20 GiB. The registry is sha256-only and reads
are anonymous.

Exit codes: `2` usage/input, `3` integrity, `4` conflict, `5` limit, `6`
network, `1` other.

## Tests

```sh
cargo test -q -p myoci
cargo clippy -q -p myoci --all-targets -- -D warnings
make test-oci-pull
make test-oci-push
make test-oci-conformance
make test-oci-conformance-push
make test-oci-publish
make oci-bench
```
