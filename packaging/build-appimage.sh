#!/usr/bin/env bash
# Build a single-file, no-install AppImage for sampa2 (N3 — Linux citizen).
# Output: target/sampa2-<version>-x86_64.AppImage
#
#   ./packaging/build-appimage.sh
#   ./target/sampa2-*-x86_64.AppImage           # runs anywhere, no install
#
# The binary hard-links only libc6/libgcc-s1; the Wayland/X11 + Vulkan stack is dlopened
# from the host (varies per system), so we deliberately DON'T bundle libs — the AppImage
# stays small and uses the host's display/GPU drivers, same as the .deb.
set -euo pipefail
umask 022

ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN="$ROOT/target/release/sampa2"
VERSION=$(grep -m1 '^version' "$ROOT/crates/sampa-native/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
ARCH=x86_64
OUT="$ROOT/target/sampa2-${VERSION}-${ARCH}.AppImage"

# appimagetool: use one on PATH, else the cached download, else fetch it.
TOOL=$(command -v appimagetool || true)
if [ -z "$TOOL" ]; then
    TOOL="$ROOT/target/appimagetool-x86_64.AppImage"
    if [ ! -x "$TOOL" ]; then
        echo "==> fetching appimagetool"
        curl -sSL -o "$TOOL" \
            https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
        chmod +x "$TOOL"
    fi
fi

if [ ! -x "$BIN" ]; then
    echo "==> release binary missing; building"
    (cd "$ROOT" && cargo build --release -p sampa-native)
fi

echo "==> assembling AppDir"
APPDIR=$(mktemp -d)/sampa2.AppDir
trap 'rm -rf "$(dirname "$APPDIR")"' EXIT
install -Dm755 "$BIN" "$APPDIR/usr/bin/sampa2"
strip "$APPDIR/usr/bin/sampa2" 2>/dev/null || true
install -Dm644 "$ROOT/assets/sampa2.desktop" "$APPDIR/usr/share/applications/sampa2.desktop"
for sz in 64 128 256 512; do
    install -Dm644 "$ROOT/assets/sampa2-$sz.png" "$APPDIR/usr/share/icons/hicolor/${sz}x${sz}/apps/sampa2.png"
done
install -Dm644 "$ROOT/assets/sampa2-icon.svg" "$APPDIR/usr/share/icons/hicolor/scalable/apps/sampa2.svg"

# AppImage top-level requirements: a .desktop, a matching icon, and AppRun.
install -Dm644 "$ROOT/assets/sampa2.desktop" "$APPDIR/sampa2.desktop"
install -Dm644 "$ROOT/assets/sampa2-256.png" "$APPDIR/sampa2.png"
cat > "$APPDIR/AppRun" <<'RUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "${0}")")"
exec "${HERE}/usr/bin/sampa2" "$@"
RUN
chmod 755 "$APPDIR/AppRun"

echo "==> building $OUT"
# --appimage-extract-and-run: no FUSE needed for the tool itself. No signing / update info.
ARCH="$ARCH" "$TOOL" --appimage-extract-and-run "$APPDIR" "$OUT" >/dev/null 2>&1
chmod +x "$OUT"
echo "built: $OUT ($(du -h "$OUT" | cut -f1))"
