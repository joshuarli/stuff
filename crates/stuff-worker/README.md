# stuff-worker

`npm run build` produces the Cloudflare Build Output Specification under
`.cloudflare/output/v0`: the wasm module and bindgen glue from
`scripts/build-wasm.sh`, `src/entry.ts` emitted by `tsrs`, and the
`worker.config.json` manifest that names the three ES modules.

```sh
cf workers types             # regenerate .cloudflare/types from cloudflare.config.ts
sh scripts/build-wasm.sh     # cargo build + wasm-bindgen + wasm-opt
tsrs -p tsconfig.json --fast # type-check and emit src/entry.ts into the bundle
```

`npm run build:wasm` runs only the first wasm step. `npm run typecheck` checks
the TypeScript without emitting. `cf deploy --prebuilt` uploads the spec
as-is; it does not rebuild anything.

The wasm step runs:

```sh
cargo build --target wasm32-unknown-unknown --profile wasm-release -p stuff-worker
wasm-bindgen --target web --out-dir build/wasm ../../target/wasm32-unknown-unknown/wasm-release/stuff_worker.wasm
wasm-opt -O3 --strip-debug --strip-producers build/wasm/stuff_worker_bg.wasm -o build/wasm/stuff_worker_bg.opt.wasm
mv build/wasm/stuff_worker_bg.opt.wasm build/wasm/stuff_worker_bg.wasm
```

The `wasm-release` profile uses `opt-level = 3`, fat LTO, one codegen unit, and
`panic = "abort"`. `wasm-opt -O3` is the speed-oriented Binaryen pass;
size-oriented `-Oz` is not used. Both `wasm-bindgen` 0.2.129 and Binaryen's
`wasm-opt` must be on `PATH`.
