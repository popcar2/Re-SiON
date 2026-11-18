#!/bin/sh
# Release build of the C library (the player with the `ffi` feature: the exports in src/ffi.rs, API in sion.h):
# libsion.so, or libsion.dll + libsion.dll.a for Windows. Usage: build-ffi.sh [TARGET] (default: this machine).
# Output: target/TARGET/ffi/. Needs nightly (with rust-src) to keep the library small, all
# set here so the crate itself builds on stable: core/alloc rebuilt, and panics as a bare abort (no std: see src/ffi.rs).
set -eu
cd "$(dirname "$0")"

TARGET=${1:-$(rustc +nightly -vV | sed -n 's/^host: //p')}
cargo +nightly rustc --lib --profile ffi --target "$TARGET" --crate-type cdylib --features ffi \
  -Zbuild-std=core,alloc -Zpanic-immediate-abort --config 'profile.ffi.panic="immediate-abort"'

OUT=target/$TARGET/ffi
case $TARGET in
*-windows-gnu*)
  # rustc names the Windows cdylib sion.dll; ship it as libsion.dll (matching libsion.so), and rebuild the
  # import library from the DLL's exports so it points programs at libsion.dll rather than sion.dll.
  rm -f "$OUT/libsion.dll" # cargo may have hard-linked it to sion.dll on an earlier run
  mv "$OUT/sion.dll" "$OUT/libsion.dll"
  DLLTOOL=$(command -v x86_64-w64-mingw32-dlltool || command -v dlltool)
  OBJDUMP=$(command -v x86_64-w64-mingw32-objdump || command -v objdump)
  { echo EXPORTS
    "$OBJDUMP" -p "$OUT/libsion.dll" | sed -n '/Name Pointer\] Table/,/^$/s/^\s*\[ *[0-9]*\] +base\[ *[0-9]*\] *[0-9a-f]* //p'
  } > "$OUT/libsion.def"
  "$DLLTOOL" -D libsion.dll -d "$OUT/libsion.def" -l "$OUT/libsion.dll.a"
  rm -f "$OUT/libsion.def"
  ;;
esac
echo "built $OUT/"
