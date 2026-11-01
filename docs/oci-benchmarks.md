# OCI benchmarks

- Command: `myoci bench --base-url http://127.0.0.1:53244 --repo example/bench --tag stable --runs 5 --label myoci serve (loopback)`
- Label: myoci serve (loopback)
- Host: Darwin 27.0.0 arm64; rustc 1.100.0-nightly (574ff7d98 2026-09-14)
- Date: 2026-11-01T18:56:58Z
- Runs: 5
- Method: local loopback unless the base URL is deployed. Cold is the first pass with a new client; warm repeats with the same client. On the deployed Worker, cold means a freshly published blob digest because the gateway cache key ignores query strings. The 1 GiB row is a local measurement; the Worker's 512 MB cacheable-object cap applies only to the deployed path (Needs environment).

## Manifest requests

| Method | Concurrency | Temperature | Requests | p50 ms | p95 ms | p99 ms |
| --- | --- | --- | --- | --- | --- | --- |
| GET | 1 | cold (run 1) | 16 | 6.232 | 6.359 | 6.381 |
| GET | 1 | warm | 64 | 6.267 | 6.415 | 6.419 |
| HEAD | 1 | cold (run 1) | 16 | 6.262 | 6.336 | 6.351 |
| HEAD | 1 | warm | 64 | 6.262 | 6.377 | 6.391 |
| GET | 8 | cold (run 1) | 128 | 6.074 | 6.583 | 6.714 |
| GET | 8 | warm | 512 | 6.253 | 6.602 | 6.741 |
| HEAD | 8 | cold (run 1) | 128 | 6.191 | 6.571 | 6.644 |
| HEAD | 8 | warm | 512 | 6.264 | 6.585 | 6.726 |
| GET | 32 | cold (run 1) | 512 | 6.562 | 7.661 | 7.911 |
| GET | 32 | warm | 2048 | 6.447 | 7.565 | 8.547 |
| HEAD | 32 | cold (run 1) | 512 | 6.446 | 7.521 | 7.864 |
| HEAD | 32 | warm | 2048 | 6.462 | 7.341 | 7.693 |

## Blob downloads

| Size | Temperature | Samples | p50 ms | p95 ms | p99 ms | MiB/s |
| --- | --- | --- | --- | --- | --- | --- |
| 1 MiB | cold | 5 | 6.188 | 6.467 | 6.467 | 162.6 |
| 1 MiB | warm | 5 | 6.308 | 6.379 | 6.379 | 158.4 |
| 64 MiB | cold | 5 | 12.562 | 27.578 | 27.578 | 4073.5 |
| 64 MiB | warm | 5 | 12.524 | 16.590 | 16.590 | 4984.0 |
| 256 MiB | cold | 5 | 42.691 | 45.240 | 45.240 | 6005.0 |
| 256 MiB | warm | 5 | 42.580 | 43.465 | 43.465 | 6039.7 |
| 1024 MiB | cold | 5 | 154.490 | 158.303 | 158.303 | 6595.3 |
| 1024 MiB | warm | 5 | 155.329 | 156.737 | 156.737 | 6591.7 |

## Full image pull

| Pass | Samples | p50 ms | p95 ms | p99 ms | MiB/s |
| --- | --- | --- | --- | --- | --- |
| first pull | 5 | 237.953 | 242.697 | 242.697 | 5650.5 |
| repeat pull | 5 | 239.828 | 245.327 | 245.327 | 5601.2 |

## Direct CDN comparison (A11)

- Local proof: `deploy/test-oci-redirect.sh` starts the Worker front and a separate blob host, and Docker follows the Worker's `307` to the blob host.
- O2 gate: compare this matrix against the same matrix with `STUFF_OCI_BLOB_ORIGIN` set on the deployed Worker and a Cache Rule for `oci-blobs.17166969.xyz/sha256/*`. Adopt only if warm p95 and full-pull wall time improve by at least 10% with non-overlapping p95 over five interleaved runs.
- Status: pending. The loopback matrix cannot answer O2; no deployed comparison exists yet, so `STUFF_OCI_BLOB_ORIGIN` stays unset. Tracked as STUF-52.
