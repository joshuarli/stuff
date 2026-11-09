#!/bin/sh
set -eu

cd "$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)"
target_dir=${CARGO_TARGET_DIR:-../../target}

command -v wasm-opt >/dev/null || {
  echo "wasm-opt from Binaryen is required" >&2
  exit 1
}

expected_wasm_bindgen() {
  awk '
    /^\[\[package\]\]/{in_pkg=0}
    /^name = "wasm-bindgen"$/{in_pkg=1}
    in_pkg && /^version = /{gsub(/"/,"",$3); print $3; exit}
  ' ../../Cargo.lock
}

sync_wasm_bindgen() {
  expected=$(expected_wasm_bindgen)
  [ -n "$expected" ] || return 0
  if command -v wasm-bindgen >/dev/null; then
    have=$(wasm-bindgen --version | awk '{print $2}')
    [ "$have" = "$expected" ] && return 0
    echo "syncing wasm-bindgen-cli $have -> $expected" >&2
  else
    echo "installing wasm-bindgen-cli $expected" >&2
  fi
  cargo install -f wasm-bindgen-cli --version "$expected" --locked
}

sync_wasm_bindgen

cargo build --target wasm32-unknown-unknown --profile wasm-release -p stuff-worker
wasm-bindgen --target web --out-dir build/wasm "$target_dir/wasm32-unknown-unknown/wasm-release/stuff_worker.wasm"
wasm-opt -O3 --strip-debug --strip-producers build/wasm/stuff_worker_bg.wasm -o build/wasm/stuff_worker_bg.opt.wasm
mv build/wasm/stuff_worker_bg.opt.wasm build/wasm/stuff_worker_bg.wasm
