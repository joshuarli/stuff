# OCI benchmarks

- Command: `myoci bench --base-url http://127.0.0.1:49312 --repo example/bench --tag stable --runs 5 --label myoci serve (loopback)`
- Label: myoci serve (loopback)
- Host: Darwin 27.0.0 arm64; rustc 1.100.0-nightly (574ff7d98 2026-09-14)
- Date: 2026-11-01T17:30:22Z
- Runs: 5
- Method: local loopback unless the base URL is deployed. Cold is the first pass with a new client; warm repeats with the same client. On the deployed Worker, cold means a freshly published blob digest because the gateway cache key ignores query strings. The 1 GiB row is a local measurement; the Worker's 512 MB cacheable-object cap applies only to the deployed path (Needs environment).

## Manifest requests

| Method | Concurrency | Temperature | Requests | p50 ms | p95 ms | p99 ms |
| --- | --- | --- | --- | --- | --- | --- |
| GET | 1 | cold (run 1) | 16 | 7.452 | 7.653 | 7.698 |
| GET | 1 | warm | 64 | 7.435 | 7.569 | 7.592 |
| HEAD | 1 | cold (run 1) | 16 | 7.479 | 7.559 | 7.565 |
| HEAD | 1 | warm | 64 | 7.490 | 7.623 | 7.710 |
| GET | 8 | cold (run 1) | 128 | 7.481 | 7.795 | 7.850 |
| GET | 8 | warm | 512 | 7.364 | 7.772 | 7.909 |
| HEAD | 8 | cold (run 1) | 128 | 6.978 | 7.772 | 7.905 |
| HEAD | 8 | warm | 512 | 7.403 | 7.737 | 7.844 |
| GET | 32 | cold (run 1) | 512 | 7.291 | 8.619 | 8.960 |
| GET | 32 | warm | 2048 | 7.487 | 8.809 | 9.245 |
| HEAD | 32 | cold (run 1) | 512 | 7.241 | 8.425 | 8.734 |
| HEAD | 32 | warm | 2048 | 7.300 | 8.576 | 11.113 |

## Blob downloads

| Size | Temperature | Samples | p50 ms | p95 ms | p99 ms | MiB/s |
| --- | --- | --- | --- | --- | --- | --- |
| 1 MiB | cold | 5 | 7.102 | 7.840 | 7.840 | 141.8 |
| 1 MiB | warm | 5 | 7.130 | 7.590 | 7.590 | 142.3 |
| 64 MiB | cold | 5 | 14.897 | 30.060 | 30.060 | 3716.9 |
| 64 MiB | warm | 5 | 14.027 | 16.691 | 16.691 | 4462.2 |
| 256 MiB | cold | 5 | 41.640 | 42.510 | 42.510 | 6214.3 |
| 256 MiB | warm | 5 | 42.100 | 44.864 | 44.864 | 6099.5 |
| 1024 MiB | cold | 5 | 160.674 | 185.602 | 185.602 | 6218.8 |
| 1024 MiB | warm | 5 | 159.484 | 166.954 | 166.954 | 6417.7 |

## Full image pull

| Pass | Samples | p50 ms | p95 ms | p99 ms | MiB/s |
| --- | --- | --- | --- | --- | --- |
| first pull | 5 | 249.979 | 252.405 | 252.405 | 5401.2 |
| repeat pull | 5 | 248.188 | 254.828 | 254.828 | 5393.3 |

