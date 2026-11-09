#!/bin/bash
# Builds <app id>-<version>-<suffix>.flatpak, a single-file bundle of this
# repo's GTK app, from the manifest next to this (x86_64; aarch64 with
# ARCH=aarch64 on an aarch64 machine). Needs flatpak and flatpak-builder; the
# runtime (org.gnome.Platform//50) and the rust-stable extension come from
# Flathub. retifs is expected next to this repo (`../retifs`), as for the
# .deb. retifs itself (retifsd) is the host's: the retifs .deb.
#
#     packaging/flatpak/build.sh [OUT_DIR] [SUFFIX]
#
# Install: flatpak install --user <bundle> (the runtime comes from Flathub).
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
PROJ=$(cd "$ROOT/.." && pwd)
MANIFEST=$(ls "$HERE"/org.retifs.*.yml | head -1)
ID=$(basename "$MANIFEST" .yml)
# The repos it builds from, named in the manifest as src/<repo>.
SRCS="retifs $(basename "$ROOT")"
OUT=${1:-$ROOT/target/flatpak}
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1 | tr -d '\r')
COMMIT=$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)
DATE=$(git -C "$ROOT" log -1 --format=%cd --date=format:%Y%m%d 2>/dev/null || date +%Y%m%d)
SUFFIX=${2:-git$DATE.$COMMIT}
ARCH=${ARCH:-$(flatpak --default-arch)}
# Off the Windows mount, if that's where the repos are (WSL): much faster.
WORK=${WORK:-$HOME/.cache/retifs-flatpak/$ID}
mkdir -p "$OUT" "$WORK"

# flatpak-builder takes `dir` sources next to the manifest: the repos'
# files as they are in the working tree (tracked, and new ones not ignored),
# without target/ and .git, staged beside a copy of it. A checkout git
# can't read here (a Windows worktree seen from WSL): all of it but those.
rm -rf "$WORK/src"
for r in $SRCS; do
    mkdir -p "$WORK/src/$r"
    if FILES=$(git -c safe.directory='*' -C "$PROJ/$r" ls-files -z --cached --others --exclude-standard 2>/dev/null | tr '\0' '\n'); then
        (cd "$PROJ/$r" && tr '\n' '\0' <<<"$FILES" | tar --null -T - --ignore-failed-read -cf - 2>/dev/null) | tar -C "$WORK/src/$r" -xf -
    else
        tar -C "$PROJ/$r" --exclude=target --exclude=.git --exclude=node_modules -cf - . | tar -C "$WORK/src/$r" -xf -
    fi
done
cp "$MANIFEST" "$WORK/"

flatpak --user remote-add --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
# No FUSE needed (rofiles-fuse): WSL and containers often have none.
flatpak-builder --user --arch="$ARCH" --force-clean --disable-rofiles-fuse --install-deps-from=flathub \
    --state-dir="$WORK/state" --repo="$WORK/repo" "$WORK/build" "$WORK/$ID.yml"
BUNDLE="$OUT/$ID-$VERSION-$SUFFIX.flatpak"
# The runtime from Flathub when the bundle is installed.
flatpak build-bundle --arch="$ARCH" --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo "$WORK/repo" "$BUNDLE" "$ID"
echo "== $BUNDLE ($(du -h "$BUNDLE" | cut -f1))"
