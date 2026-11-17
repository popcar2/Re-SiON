#!/bin/sh
# Release build of convert.wasm (the converter library with the exports in src/wasm.rs), post-optimised with
# Binaryen's wasm-opt (nightly: it has the wasm32 target). Output: target/wasm32-unknown-unknown/wasm/convert.wasm.
set -eu
cd "$(dirname "$0")"

cargo +nightly rustc --lib --profile wasm --target wasm32-unknown-unknown --crate-type cdylib \
  --config 'target.wasm32-unknown-unknown.rustflags=["-Ctarget-feature=+simd128"]'

DIR=target/wasm32-unknown-unknown/wasm
WASM=$DIR/convert.wasm
cp "$DIR/sion_converter.wasm" "$WASM"
if ! command -v wasm-opt >/dev/null 2>&1; then
  echo "wasm-opt not found (install binaryen); leaving $WASM unoptimised" >&2
  exit 0
fi

before=$(wc -c < "$WASM")
wasm-opt "$WASM" -Os --enable-simd --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
  --enable-mutable-globals --strip-debug --strip-producers -o "$WASM.opt"
mv "$WASM.opt" "$WASM"
echo "wasm-opt: $before -> $(wc -c < "$WASM") bytes"
