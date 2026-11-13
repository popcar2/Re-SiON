#!/bin/sh
# Release build of the C library (the player with the `ffi` feature: the exports in src/ffi.rs, API in sion.h):
# libsion.so, or sion.dll + libsion.dll.a for Windows. Usage: build-ffi.sh [TARGET] (default: this machine).
# Output: target/TARGET/ffi/. Needs nightly (with rust-src) to keep the library small, all
# set here so the crate itself builds on stable: core/alloc rebuilt, and panics as a bare abort (no std: see src/ffi.rs).
set -eu
cd "$(dirname "$0")"

TARGET=${1:-$(rustc +nightly -vV | sed -n 's/^host: //p')}
cargo +nightly rustc --lib --profile ffi --target "$TARGET" --crate-type cdylib --features ffi \
  -Zbuild-std=core,alloc -Zpanic-immediate-abort --config 'profile.ffi.panic="immediate-abort"'
echo "built target/$TARGET/ffi/"
