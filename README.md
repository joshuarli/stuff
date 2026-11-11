# stuff

Four package registries with one publisher: Cargo, npm, Python wheels, and OCI images.
`stuff` publishes immutable generations, serves a searchable catalog, and supports rollback to retained generations. Package resolution and installation use the ecosystem tools: Cargo, npm/pnpm, uv/CPython, and oras.

## Install

Run the `release` workflow from the default branch to build the `stuff` CLI for Apple Silicon and Linux musl. Each `release-<sha>` prerelease contains standalone `stuff-<target>` executables and `SHA256SUMS`. Install with `sh dl stuff` from `~/usr`.

Publication prerequisites, configuration, credentials, and operations are in [docs/PUBLISHING.md](docs/PUBLISHING.md). `stuff publish --help` describes the supported options.

## Registries

| Ecosystem | Client root |
| --- | --- |
| Cargo sparse index | `https://crates.17166969.xyz/current/registry/` |
| npm | `https://npm.17166969.xyz/current/` |
| pip | `https://pypi.17166969.xyz/current/simple/` |
| OCI | `https://oci.17166969.xyz` |

The aggregate catalog is at [stuff.17166969.xyz](https://stuff.17166969.xyz).
Cargo downloads use `crates-blobs.17166969.xyz`; OCI blobs use `oci-blobs.17166969.xyz`.
Each registry activates its own generation through `current`; publishing one does not republish another.

```sh
stuff publish cargo -p my-crate
stuff publish npm --lockfile pnpm-lock.yaml --project my-project
stuff publish pypi six==1.16.0
stuff publish oci --image example/app=ghcr.io/example/app:v1
```

OCI reads are anonymous and follow the Distribution 1.1 API:

```sh
docker pull oci.17166969.xyz/example/app:stable
oras manifest fetch oci.17166969.xyz/example/app:stable
```

The checked-in deployment disables network pushes. Deployments that enable them need a registry token and a separate blob writer, as described in [publishing operations](docs/PUBLISHING.md#push-writer).

## Browse and inspect

Every published generation includes `catalog.json`, an HTML landing page, and an LLM-readable `llms.txt` derived from the published registry state.

```sh
stuff ls
stuff ls --registry npm --versions
stuff search h12tiny
stuff diff before/catalog.json after/catalog.json
stuff doctor --sample-artifacts
stuff render --output index.html
stuff render --output llms.txt
```

Commands read the live registries by default without credentials. Use `--cargo-dir`, `--npm-dir`, `--pypi-dir`, or `--oci-dir` to inspect a local site, generation, or catalog. `--json` emits inventory as JSON; `--versions` includes every published version.

With any local directory supplied, `doctor` skips omitted registries. It refuses `--expected-generation` unless every selected registry is checked. An explicitly selected registry needs its local directory or an explicit base URL when another registry is local. Artifact sampling checks hashes as well as availability.

Catalog pages link packages to their dependencies and dependents, grouped by dependency kind. Search matches package name, registry, and source, and remains hidden when scripts are disabled. The [catalog format](docs/mycrates-contracts.md#catalogjson-schema-4) describes schema compatibility and ecosystem-specific dependency data.

The aggregate page and `/llms.txt` combine all four catalogs and cache responses for 60 seconds. A missing or invalid generation pointer returns 404; on a render-cache miss, a missing catalog object also returns 404. If catalog bytes exist but some cannot be parsed, the renderer lists compatible catalogs with notices. It fails when none can be parsed. `stuff render` requires all selected catalogs to load.

## Serve a local site

```sh
cargo run -p stuff-server -- --route /=path/to/site
cargo run -p stuff-server -- --route /mirror=path/to/mirror --max-connections 64
```

The server follows `current` to a generation, serves immutable `generations/` paths, and supports conditional and range requests. Directory requests serve `index.html`. `/`, `/index.html`, `/catalog.json`, `/llms.txt`, and `/simple/...` alias through the current generation.

The native server and Worker support shared wheel/tarball aliases and npm's standard `current/<name>/-/<basename>-<version>.tgz` lookup. Metadata-only npm versions support lockfile checks for excluded platforms without hosting their artifacts; see [mynpm](crates/mynpm/README.md).

## Exit codes and further usage

The canonical `stuff publish`, `republish`, and `rollback` commands use `0` for success/no-op, `2` for usage/configuration/validation, `3` for conflict, `4` for integrity failure, and `5` for transport failure. `stuff doctor` uses `1` for a failed check and `2` for its own error. Standalone registry tools retain their own exit-code contracts.

Per-registry usage: [mycrates archive and oracle](crates/mycrates/README.md), [mynpm](crates/mynpm/README.md), [mypypi](crates/mypypi/README.md), and [myoci](crates/myoci/README.md). Known issues are in [TODO.md](TODO.md).

## License

Apache-2.0 for stuff files; imported crates keep their own license fields.
