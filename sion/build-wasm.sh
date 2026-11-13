#!/bin/sh
# Release build of sion.wasm (the player with the `wasm` feature: the exports in src/wasm.rs), post-optimised
# with Binaryen's wasm-opt. Output: target/wasm32-unknown-unknown/wasm/sion.wasm.
# Needs nightly (with rust-src) for the size options, all given here so the crate itself builds on stable:
# core/alloc rebuilt size-optimised, and panics as a bare trap with no panic machinery.
set -eu
cd "$(dirname "$0")"

cargo +nightly rustc --lib --profile wasm --target wasm32-unknown-unknown --crate-type cdylib --features wasm \
  -Zbuild-std=core,alloc -Zpanic-immediate-abort --config 'profile.wasm.panic="immediate-abort"' \
  --config 'target.wasm32-unknown-unknown.rustflags=["-Ctarget-feature=+simd128"]'

WASM=target/wasm32-unknown-unknown/wasm/sion.wasm
if ! command -v wasm-opt >/dev/null 2>&1; then
  echo "wasm-opt not found (install binaryen); leaving $WASM unoptimised" >&2
  exit 0
fi

before=$(wc -c < "$WASM")
# wasm-opt takes the features to allow from the target_features section (kept by `strip = "debuginfo"`).
wasm-opt "$WASM" -Oz --converge --strip-debug --strip-producers --strip-target-features -o "$WASM.opt"
mv "$WASM.opt" "$WASM"
echo "wasm-opt: $before -> $(wc -c < "$WASM") bytes"
