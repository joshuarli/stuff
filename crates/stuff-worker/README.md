# stuff-worker

`npm run build:wasm` builds the Worker's WebAssembly module and JavaScript
glue:

```sh
cargo build --target wasm32-unknown-unknown --profile wasm-release -p stuff-worker
wasm-bindgen --target web --out-dir build/wasm ../../target/wasm32-unknown-unknown/wasm-release/stuff_worker.wasm
wasm-opt -O3 --strip-debug --strip-producers build/wasm/stuff_worker_bg.wasm -o build/wasm/stuff_worker_bg.opt.wasm
mv build/wasm/stuff_worker_bg.opt.wasm build/wasm/stuff_worker_bg.wasm
```

The `wasm-release` profile uses `opt-level = 3`, fat LTO, one codegen unit, and
`panic = "abort"`. `wasm-opt -O3` is the speed-oriented Binaryen pass;
size-oriented `-Oz` is not used. Both `wasm-bindgen` 0.2.129 and Binaryen's
`wasm-opt` must be on `PATH`. `cf deploy` runs the same script before bundling
`src/entry.ts`.
