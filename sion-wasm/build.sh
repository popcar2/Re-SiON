#!/bin/sh
# Release build of sion.wasm, post-optimised with Binaryen's wasm-opt.
# Output: target/wasm32-unknown-unknown/release/sion.wasm (optimised in place).
set -eu
cd "$(dirname "$0")"

cargo build --release

WASM=target/wasm32-unknown-unknown/release/sion.wasm
if ! command -v wasm-opt >/dev/null 2>&1; then
  echo "wasm-opt not found (install binaryen); leaving $WASM unoptimised" >&2
  exit 0
fi

before=$(wc -c < "$WASM")
# Features rustc emits for wasm32-unknown-unknown, plus the simd128 enabled in .cargo/config.toml.
wasm-opt "$WASM" -Oz --converge \
  --enable-simd --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
  --enable-mutable-globals --enable-multivalue --enable-reference-types \
  --strip-debug --strip-producers \
  -o "$WASM.opt"
mv "$WASM.opt" "$WASM"
echo "wasm-opt: $before -> $(wc -c < "$WASM") bytes"
