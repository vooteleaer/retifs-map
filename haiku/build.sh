#!/bin/sh
# Builds retifs Map for Haiku on Haiku: the core as a library (haiku/capi),
# the BeAPI app over it, as an app folder (haiku/build/Map: the app,
# lib/libretifs_map.so).
#
#     haiku/build.sh
#
# Needs: pkgman install rust_bin gcc haiku_devel; retifs next to retifs-map.
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
OUT=$HERE/build/Map
cd "$ROOT"
cargo build --release --ignore-rust-version -p retifs-map-capi
mkdir -p "$OUT/lib"
# A new file, not the old one overwritten: a running app has it mapped.
rm -f "${OUT:?}/lib/libretifs_map.so"
cp target/release/libretifs_map.so "$OUT/lib/"
rm -f "${OUT:?}/Map"
g++ -O2 -std=c++17 -Wall -o "$OUT/Map" "$HERE/app/MapApp.cpp" \
	-I "$HERE/capi" -I /boot/system/develop/headers/private/shared \
	-L "$OUT/lib" -lretifs_map -lbe -lshared \
	-Wl,-rpath,'$ORIGIN/lib'
rc -o "$HERE/build/map.rsrc" "$HERE/app/map.rdef"
xres -o "$OUT/Map" "$HERE/build/map.rsrc"
mimeset -f "$OUT/Map"
echo "== $OUT"
ls -la "$OUT" "$OUT/lib"
