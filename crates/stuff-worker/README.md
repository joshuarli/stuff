# stuff-worker

Cloudflare Worker that serves the registry layouts and the static artifact
mirror from R2. URL routing
and response planning are Rust (`src/map.rs`, `src/api.rs`, shared `stuff-http`
semantics, `deser` for config parsing), with no `worker` SDK and no async
runtime in the dependency tree. Routes match on the request hostname, so each
registry serves from its own domain root (see `STUFF_ROUTES` in
`cloudflare.config.ts`, which also defines the bindings and custom domains).
The thin fetch handler `entry.ts` initializes the compiled wasm once
(`initSync`) and talks to R2 directly.

That boundary is the wasm-bindgen ABI produced by `scripts/build-wasm.sh`.
The exports are synchronous and carry JSON strings and primitives.
workerd instantiates the resulting core module, and `cf deploy` ships that
same bundle. A WIT component would need a second host. Workers load core
modules through `WebAssembly.instantiate`, and the platform's Rust path is
wasm-bindgen, so this Worker does not add a component host.

## Build

`pnpm run build` (`scripts/build.mjs`) produces the Cloudflare Build Output
Specification under `.cloudflare/output/v0`: the wasm module and bindgen glue
from `scripts/build-wasm.sh`, `entry.ts` compiled by `tsrs` to
`bundle/entry.js`, and the `worker.config.json` manifest that names the
three ES modules. It runs, in order:

```sh
cf workers types             # regenerate .cloudflare/types from cloudflare.config.ts
sh scripts/build-wasm.sh     # cargo build + wasm-bindgen + wasm-opt
tsrs -p tsconfig.json --fast # type-check and compile entry.ts into the bundle
```

`pnpm run build:wasm` runs only the wasm step. `pnpm run typecheck` checks
the TypeScript without emitting. `pnpm run dev` starts the Vite dev server that
`cf dev` discovers through `@cloudflare/vite-plugin`. `cf deploy --prebuilt`
uploads the spec as-is without rebuilding it. Validate without credentials
with `cf deploy --prebuilt --dry-run`.

Vite 8 builds with Rolldown. Wrangler is a test dependency and brings esbuild;
the Worker build does not invoke either. `make search-bundle` uses Vite to
rebuild `crates/stuff-catalog/assets/search.js`. `make catalog-css` copies the
stylesheets of the pinned `@laputa-systems/ui-dashboard` devDependency, which
`.npmrc` routes to the stuff npm registry, into
`crates/stuff-catalog/assets/ui-dashboard/`.

The wasm step runs:

```sh
cargo build --target wasm32-unknown-unknown --profile wasm-release -p stuff-worker
wasm-bindgen --target web --out-dir build/wasm ../../target/wasm32-unknown-unknown/wasm-release/stuff_worker.wasm
wasm-opt -O3 --strip-debug --strip-producers build/wasm/stuff_worker_bg.wasm -o build/wasm/stuff_worker_bg.opt.wasm
mv build/wasm/stuff_worker_bg.opt.wasm build/wasm/stuff_worker_bg.wasm
```

`--target web` with explicit init is used because the bundler target's
namespace import does not resolve to the wasm exports. The `wasm-release`
profile uses `opt-level = 3`, fat LTO, one codegen unit, and
`panic = "abort"`. `wasm-opt -O3` is the speed-oriented Binaryen pass;
size-oriented `-Oz` is not used. Binaryen's `wasm-opt` must be on `PATH`.
`scripts/build-wasm.sh` reads the bindgen version from `Cargo.lock` and
installs/replaces `wasm-bindgen-cli` with `cargo install --locked` when it
is missing or mismatched.

## Test

`make worker-test` builds the bundle and runs it under workerd with Wrangler's
integration test harness. The tests seed local R2 bindings and use the built
Worker's compatibility date. They cover cold-HEAD caching, root links, pinned
artifact aliases, OCI reads, and OCI writes.

`pnpm run check` builds the Worker, runs Oxlint and the TypeScript checker, and
runs the Worker tests. It requires Node.js 26.13.0 or newer.

## Bench

`cargo bench -p stuff-worker --bench request_paths` times the Rust half of
the request paths that production metrics show as hot, with allocation
counts:

- the unified page at small (one package per registry), production
  (`tests/fixtures/catalogs`), and heavy (production ×10) scale;
- the unified `llms.txt` on the production catalogs;
- a cargo index revalidation burst: route mapping, pointer resolution,
  and a 304 plan for every crate in the production catalog.

The JavaScript glue, the wasm boundary, and R2 latency are not in these
numbers. Read those from production with the `stats-improve` workflow.

CI runs `pnpm run build` and then `pnpm run test:runtime` in the same build
directory. `cf build` alone cannot work: `entry.ts` imports the wasm glue that
only `scripts/build.mjs` generates, so the wasm step must run before `tsrs`.

## OpenOrange

The registry runs on self-hosted OpenOrange (pinned in `orange/`; runtime in
`deploy/openorange`). `scripts/orange.mjs OUTPUT RELEASE` writes the Wrangler
descriptor from the typed build output and rejects unmapped fields; it maps text
and R2 bindings and sets `STUFF_RELEASE`. `make worker-orange-test` runs the
built Worker under the pinned `orange dev` with seeded storage for every
registry hostname.

Cloudflare-specific surfaces and what replaced them:

| Surface | On OpenOrange |
| --- | --- |
| `ctx.exports.Registry.fetch` with `cf.cacheKey` | Removed; one fetch handler. Orange builds `request.url` from the Host header (`runner.js:25926` in 1.0.2), so hostname routing is unchanged |
| `caches.default` render cache | In-process LRU of rendered unified pages (64 entries, 16 MiB) keyed by origin, release, generation ids and path; artifact bodies always stream from storage |
| `env.VERSION` | `STUFF_RELEASE` text binding |
| `.wasm` import + `initSync` | Orange loads `.wasm` imports as `WebAssembly.Module` (`bin.js:8100`); unchanged |
| Conditional R2 `put` | The S3 facade pins the write with `If-None-Match: *` or `If-Match` (`runner.js:14178`); the self-hosted store honors both |

The facade spools streamed S3 PUT bodies to a temporary file
(`runner.js:14006`). Publishers upload natively through the Rust S3 client,
so only Worker-side OCI pushes take that path. GET and range reads stream
(`runner.js:14134`).
