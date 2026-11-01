# stuff

One repository, four registries, one serving platform.

`stuff` consolidates the `mycrates` (Cargo), `mynpm` (npm), `mypypi`
(Python wheel), and `myoci` (OCI Distribution) registries onto shared Rust
infrastructure for HTTP serving, storage, and generation management. All
first-party registry logic is Rust; external ecosystems are orchestrated, not
reimplemented (npm/pnpm resolve and install, uv/CPython build and install
wheels, oras imports OCI images). Package semantics stay per-ecosystem; only
the serving layer is shared.

## Registries

Each registry publishes immutable generations activated through a `current`
pointer (`current -> generations/<id>`). One registry's publication never
rebuilds or republishes another's output.

Client roots include the generation pointer:

- Cargo sparse: `https://crates.17166969.xyz/current/registry/`
- npm: `https://npm.17166969.xyz/current/`
- pip: `https://pypi.17166969.xyz/current/simple/`
- OCI: `https://oci.17166969.xyz` (blobs at `https://oci-blobs.17166969.xyz/sha256/`)

`https://stuff.17166969.xyz` serves the aggregate catalog of all four.

OCI reads are anonymous and follow the Distribution 1.1 API:

```sh
docker pull oci.17166969.xyz/example/app:stable
oras manifest fetch oci.17166969.xyz/example/app:stable
```

Pushes need the registry token and are documented in
[docs/PUBLISHING.md](docs/PUBLISHING.md); the protocol and storage design is
in [docs/oci-design.md](docs/oci-design.md).

Per-registry usage: [mycrates](crates/mycrates/README.md),
[mynpm](crates/mynpm/README.md), [mypypi](crates/mypypi/README.md),
[myoci](crates/myoci/README.md).
Publishing and operations: [docs/PUBLISHING.md](docs/PUBLISHING.md).

## Exit codes

All four publish CLIs share one exit-code contract: `2` usage/input,
`3` integrity, `4` conflict, `5` limit, `6` network, `1` other
operational failures (`stuff doctor` exits `1` on a failed check and
`2` on its own error).

## Release

Run the `release` workflow from the default branch to publish the `stuff`
CLI. It builds Apple Silicon and Linux musl binaries, verifies them, and
attaches `stuff-<target>.zip` plus `SHA256SUMS` to a `release-<sha>`
prerelease. Install a release with `sh dl stuff` from `~/usr`.

## Serve

Serve a published site directory (or a mounted registry prefix) locally:

```sh
cargo run -p stuff-server -- --route /=path/to/site
cargo run -p stuff-server -- --route /mirror=path/to/mirror --max-connections 64
```

The server resolves `current/` through the generation symlink, serves
`generations/` paths as immutable, supports conditional and range requests,
and serves `index.html` inside directories (PEP 503 simple pages).
`/` serves the generation's catalog landing page (`current/index.html`).
`/index.html`, `/catalog.json`, `/llms.txt`, and `/simple/...` also alias
through `current` at the registry root. `/llms.txt` is the generation's
LLM-readable inventory.

The native server and the Cloudflare Worker share `stuff-http`'s route
plan, so slash-less directory requests, shared wheel and tarball aliases, and
cache kinds agree. Content-addressed tarballs and wheels, including
`-/tarballs/` and `tarballs/`, are immutable; other `current/` paths keep
the 60-second generation pointer. Both also serve the standard npm tarball
path `current/<name>/-/<basename>-<version>.tgz`, which pnpm requests from
integrity-only lockfiles: the version's `dist.tarball` in the current
packument names the object, and unknown versions are 404. pnpm 12 also checks
the publish time of every lockfile entry, including platform packages mynpm
does not host; those are published as metadata-only versions (see
[crates/mynpm/README.md](crates/mynpm/README.md)), so default pnpm settings
install from such lockfiles.

## Catalog

Every publish renders a normalized package catalog from the registry's
authoritative published state (never from input lockfiles), plus a
human-readable landing page and an LLM-readable `llms.txt`, into each
generation:

```sh
cargo run -p stuff -- ls
cargo run -p stuff -- ls --registry npm --versions
cargo run -p stuff -- search h12tiny
cargo run -p stuff -- diff before/catalog.json after/catalog.json
cargo run -p stuff -- doctor --sample-artifacts
```

`ls` works against the live registries by default (no credentials), and
against local output with `--cargo-dir/--npm-dir/--pypi-dir` (site,
generation, or `catalog.json`). Supplying any local dir limits `doctor` to
the supplied registries: it prints a skip check for each omitted registry
and refuses `--expected-generation` unless every selected registry is
checked. `--json` emits machine-readable inventory, `--versions` lists every
published version, and `render --output index.html` aggregates the three
catalogs into one page (`render --output llms.txt` renders the aggregate
`llms.txt` instead). Health checks cover the active generations, pinned
catalog objects, essential endpoints (including `current/llms.txt`), and one
fully hash-verified sample artifact per registry.

Catalog schema 3 records each package's dependencies as `runtime`, `peer`,
`build`, and `dev` name lists: the sorted, unique packages in the same
registry that its latest version depends on. Cargo supplies normal, build, and
dev kinds by real crate name. npm supplies `dependencies`, `peerDependencies`,
and `devDependencies`. PyPI supplies `Requires-Dist` without extras,
canonicalized, as runtime only, because wheel metadata has no dev, build, or
peer kinds. Both HTML pages link each package to its dependencies and
dependents through in-page `#registry/name` views, with dev, build, and peer
lists collapsed. Dependencies never cross registries. Schema 1 and 2 catalogs
still load; schema 2 has runtime names only and schema 1 has none, and the
pages say so instead of showing empty lists. Readers that predate schema 3
reject schema 3 catalogs, so deploy the Worker and `stuff` CLI before
publishing.

Both pages carry a package search box that indexes the table rows already on
the page (name, registry, source), so the pages carry no extra data. It stays
hidden without scripts.

The aggregate host renders its page and `/llms.txt` in the Worker from the
three current catalogs with the same Rust catalog code (cached 60 s at the
edge). If one registry's catalog cannot be read, for example because it uses a
newer schema than the deployed Worker understands, the aggregate page and
`/llms.txt` list the other registries and say which one is missing. They only
fail when no catalog is readable. `stuff render` stays strict and fails.

## License

Apache-2.0 for stuff files; imported crates keep their own license fields.
