# stuff-worker

Cloudflare Worker that serves the three registry layouts from R2. URL routing
and response planning are Rust (`src/map.rs`, `src/api.rs`, shared `stuff-http`
semantics, `deser` for config parsing), with no `worker` SDK and no async
runtime in the dependency tree. Routes match on the request hostname, so each
registry serves from its own domain root (see `STUFF_ROUTES` in
`cloudflare.config.ts`, which also defines the bindings and custom domains).
The thin fetch handler `entry.ts` initializes the compiled wasm once
(`initSync`) and talks to R2 directly.

## Build

`npm run build` (`scripts/build.mjs`) produces the Cloudflare Build Output
Specification under `.cloudflare/output/v0`: the wasm module and bindgen glue
from `scripts/build-wasm.sh`, `entry.ts` compiled by `tsrs` to
`bundle/entry.js`, and the `worker.config.json` manifest that names the
three ES modules. It runs, in order:

```sh
cf workers types             # regenerate .cloudflare/types from cloudflare.config.ts
sh scripts/build-wasm.sh     # cargo build + wasm-bindgen + wasm-opt
tsrs -p tsconfig.json --fast # type-check and compile entry.ts into the bundle
```

`npm run build:wasm` runs only the wasm step. `npm run typecheck` checks
the TypeScript without emitting. `npm run dev` starts the Vite dev server that
`cf dev` discovers through `@cloudflare/vite-plugin`. `cf deploy --prebuilt`
uploads the spec as-is without rebuilding it. Validate without credentials
with `cf deploy --prebuilt --dry-run`.

Vite 8 builds with Rolldown. Wrangler is a test dependency and brings esbuild;
the Worker build does not invoke either. `make search-bundle` uses Vite to
rebuild `crates/stuff-catalog/assets/search.js`.

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
size-oriented `-Oz` is not used. Both `wasm-bindgen` and Binaryen's `wasm-opt`
must be on `PATH`; keep the `wasm-bindgen` CLI at the version `Cargo.toml`
pins (`=0.2.130`).

## Test

`make worker-test` builds the bundle and runs it under workerd with Wrangler's
integration test harness. The tests seed local R2 bindings and use the built
Worker's compatibility date. They cover cold-HEAD caching, root links, pinned
artifact aliases, OCI reads, and OCI writes.

`npm run check` builds the Worker, runs Oxlint and the TypeScript checker, and
runs the Worker tests. It requires Node.js 26.12.1 or newer.

CI runs `npm run build` and then `npm run test:runtime` in the same build
directory. `cf build` alone cannot work: `entry.ts` imports the wasm glue that
only `scripts/build.mjs` generates, so the wasm step must run before `tsrs`.
