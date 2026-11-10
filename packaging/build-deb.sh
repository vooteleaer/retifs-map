#!/bin/bash
# Builds retifs-map_<version>_<arch>.deb: the GTK 4 / libadwaita Map app.
# GTK 4.12 and libadwaita 1.4 or newer: Debian 13, Ubuntu 24.04. Built on
# such a machine (it links the system's libraries); needs the retifs package.
# arm64 on an amd64 machine: with the arm64 libraries installed (dpkg
# multiarch: libgtk-4-dev:arm64 libadwaita-1-dev:arm64 libshumate-dev:arm64) and
# gcc-aarch64-linux-gnu.
#
#     packaging/build-deb.sh [OUT_DIR] [ARCH]
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
OUT=${1:-$ROOT/target/deb}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$ROOT/target}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1 | tr -d '\r')
COMMIT=$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)
# A release when this commit is tagged with the version, else a build towards
# it; the commit count keeps those in order (retifs docs/versioning.md).
COUNT=$(git -C "$ROOT" rev-list --count HEAD 2>/dev/null || echo 0)
if [ "$(git -C "$ROOT" describe --exact-match --tags HEAD 2>/dev/null)" = "v$VERSION" ]; then
    DEBVER=$VERSION
else
    DEBVER="$VERSION~dev.$COUNT.$COMMIT"
fi
ARCH=${2:-$(dpkg --print-architecture)}
mkdir -p "$OUT"

if [ "$ARCH" = "$(dpkg --print-architecture)" ]; then
    cargo build --release --locked --manifest-path "$ROOT/Cargo.toml" -p retifs-map-gtk
    BIN=$CARGO_TARGET_DIR/release/retifs-map
    OBJDUMP=objdump
else
    case $ARCH in
        arm64) TRIPLE=aarch64-linux-gnu; TARGET=aarch64-unknown-linux-gnu ;;
        *) echo "unknown arch $ARCH" >&2; exit 2 ;;
    esac
    PKG_CONFIG_ALLOW_CROSS=1 PKG_CONFIG_PATH=/usr/lib/$TRIPLE/pkgconfig PKG_CONFIG_LIBDIR=/usr/lib/$TRIPLE/pkgconfig:/usr/share/pkgconfig \
        CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=$TRIPLE-gcc \
        cargo build --release --locked --target $TARGET --manifest-path "$ROOT/Cargo.toml" -p retifs-map-gtk
    BIN=$CARGO_TARGET_DIR/$TARGET/release/retifs-map
    OBJDUMP=$TRIPLE-objdump
fi
# The newest glibc symbol it uses: its libc6 dependency.
GLIBC=$($OBJDUMP -T "$BIN" | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -V | tail -1)

STAGE=$(mktemp -d)
chmod 0755 "$STAGE"
trap 'rm -rf "$STAGE"' EXIT
install -d "$STAGE/DEBIAN" "$STAGE/usr/bin" "$STAGE/usr/share/applications" "$STAGE/usr/share/icons/hicolor/scalable/apps" "$STAGE/usr/share/doc/retifs-map"
install -m 0755 "$BIN" "$STAGE/usr/bin/retifs-map"
install -m 0644 "$HERE/org.retifs.Map.desktop" "$STAGE/usr/share/applications/"
install -m 0644 "$HERE/org.retifs.Map.svg" "$STAGE/usr/share/icons/hicolor/scalable/apps/"
[ -f "$ROOT/README.md" ] && install -m 0644 "$ROOT/README.md" "$STAGE/usr/share/doc/retifs-map/"
SIZE=$(du -sk "$STAGE/usr" | cut -f1)
cat > "$STAGE/DEBIAN/control" <<CTRL
Package: retifs-map
Version: $DEBVER
Architecture: $ARCH
Maintainer: retifs <https://github.com/vooteleaer/retifs>
Installed-Size: $SIZE
Depends: retifs, libc6 (>= $GLIBC), libgtk-4-1 (>= 4.12), libadwaita-1-0 (>= 1.4), libshumate-1.0-1
Section: utils
Priority: optional
Description: retifs Map
 Places, photos, contacts and events from your libraries, on maps kept in retifs. A prototype, over retifs (the retifs package's retifsd).
CTRL
DEB="$OUT/retifs-map_${DEBVER}_${ARCH}.deb"
dpkg-deb --root-owner-group -Zxz --build "$STAGE" "$DEB" >/dev/null
echo "== $DEB ($(du -h "$DEB" | cut -f1))"
