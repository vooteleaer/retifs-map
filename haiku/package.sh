#!/bin/sh
# Makes the retifs Map package for Haiku (an hpkg) from the app folder
# build.sh made: the app with its library in apps/retifs_map, and an
# entry in the Deskbar's Applications menu. retifs itself (the daemon) comes
# from the retifs package.
#
#     haiku/build.sh && haiku/package.sh
#     pkgman install haiku/build/retifs_map-<version>-x86_64.hpkg
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
APP=$HERE/build/Map
STAGE=$HERE/build/package
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)-4
ARCH=x86_64

for f in Map lib/libretifs_map.so; do
	[ -f "$APP/$f" ] || { echo "missing $APP/$f: build it first" >&2; exit 1; }
done

rm -rf "${STAGE:?}"
mkdir -p "$STAGE/apps" "$STAGE/data/deskbar/menu/Applications"
cp -a "$APP" "$STAGE/apps/retifs_map"
ln -s ../../../../apps/retifs_map/Map \
	"$STAGE/data/deskbar/menu/Applications/retifs Map"

cat > "$STAGE/.PackageInfo" <<EOF
name			retifs_map
version			$VERSION
architecture	$ARCH
summary			"Map on retifs"
description		"Map on retifs: every record with a place (photos where they were taken, contacts where they live, events where they happen) on maps from map libraries in retifs, never from the internet; the map drawn by the app from the library's own style. A working prototype."
packager		"retifs <https://github.com/vooteleaer/retifs>"
vendor			"retifs"
licenses {
	"MIT"
}
copyrights {
	"2026 retifs contributors"
}
provides {
	retifs_map = $VERSION
	app:retifs_map = $VERSION
}
requires {
	haiku
	retifs
	lib:libgcc_s
	lib:libstdc++
}
urls {
	"https://github.com/vooteleaer/retifs"
}
EOF

PKG=$HERE/build/retifs_map-$VERSION-$ARCH.hpkg
rm -f "$PKG"
package create -C "$STAGE" "$PKG"
echo "== $PKG"
package list "$PKG" | tail -12
