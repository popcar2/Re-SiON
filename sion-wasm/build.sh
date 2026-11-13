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
# wasm-opt takes the features to allow from the target_features section (kept by `strip = "debuginfo"`).
wasm-opt "$WASM" -Oz --converge --strip-debug --strip-producers --strip-target-features -o "$WASM.opt"
mv "$WASM.opt" "$WASM"
echo "wasm-opt: $before -> $(wc -c < "$WASM") bytes"
