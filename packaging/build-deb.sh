#!/usr/bin/env bash
# Build a .deb for sampa2 from the release binary + assets (N3 — Linux citizen).
# Self-contained: needs only `dpkg-deb` (no cargo-deb). Output: target/sampa2_<ver>_<arch>.deb
#
#   ./packaging/build-deb.sh            # builds release if needed, then packages
#   sudo apt install ./target/sampa2_*.deb
set -euo pipefail
umask 022   # so staged dirs are 0755 / files 0644 (Debian policy)

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DEB="$ROOT/packaging/deb"
BIN="$ROOT/target/release/sampa2"
VERSION=$(grep -m1 '^version' "$ROOT/crates/sampa-native/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
ARCH=$(dpkg --print-architecture)

# Build the optimized binary if it's missing.
if [ ! -x "$BIN" ]; then
    echo "==> release binary missing; building"
    (cd "$ROOT" && cargo build --release -p sampa-native)
fi

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT

echo "==> staging files"
install -Dm755 "$BIN" "$STAGE/usr/bin/sampa2"
strip "$STAGE/usr/bin/sampa2" 2>/dev/null || true
install -Dm644 "$ROOT/assets/sampa2.desktop" "$STAGE/usr/share/applications/sampa2.desktop"
for sz in 64 128 256 512; do
    install -Dm644 "$ROOT/assets/sampa2-$sz.png" "$STAGE/usr/share/icons/hicolor/${sz}x${sz}/apps/sampa2.png"
done
install -Dm644 "$ROOT/assets/sampa2-icon.svg" "$STAGE/usr/share/icons/hicolor/scalable/apps/sampa2.svg"
install -Dm644 "$DEB/copyright" "$STAGE/usr/share/doc/sampa2/copyright"
# Native package (version has no Debian revision) → changelog.gz, mode 0644.
install -d "$STAGE/usr/share/doc/sampa2"
printf 'sampa2 (%s) unstable; urgency=medium\n\n  * Native (Path C) build package.\n\n -- Marcelo Pereira Barbosa <mpbarbosa@gmail.com>  Thu, 01 Jan 1970 00:00:00 +0000\n' \
    "$VERSION" | gzip -9n > "$STAGE/usr/share/doc/sampa2/changelog.gz"
chmod 644 "$STAGE/usr/share/doc/sampa2/changelog.gz"
# Man page.
mkdir -p "$STAGE/usr/share/man/man1"
gzip -9nc "$DEB/sampa2.1" > "$STAGE/usr/share/man/man1/sampa2.1.gz"
chmod 644 "$STAGE/usr/share/man/man1/sampa2.1.gz"

echo "==> control + maintainer scripts"
mkdir -p "$STAGE/DEBIAN"
INSTALLED_KB=$(du -sk "$STAGE/usr" | cut -f1)
# libc6/libgcc-s1 are hard-linked; the display + Vulkan stack is dlopened at runtime, so
# libvulkan1 is required to init wgpu, and the window-system client libs + GPU driver are
# Recommends (present on any desktop; the exact one depends on the session).
cat > "$STAGE/DEBIAN/control" <<EOF
Package: sampa2
Version: $VERSION
Architecture: $ARCH
Maintainer: Marcelo Pereira Barbosa <mpbarbosa@gmail.com>
Installed-Size: $INSTALLED_KB
Depends: libc6, libgcc-s1, libvulkan1
Recommends: mesa-vulkan-drivers, libwayland-client0, libxkbcommon0, libx11-6, libxcb1, fonts-hack
Section: x11
Priority: optional
Homepage: https://github.com/mpbarbosa/sampa2_graphics_terminal
Description: Native GPU terminal emulator (winit + wgpu + alacritty_terminal)
 A Rust-only graphical terminal for Linux: an in-app command palette, a live
 man-page panel, a safe preview-as-you-type pane, and an opt-in Claude command
 suggester. Renders with winit + wgpu + cosmic-text over an alacritty_terminal
 VT engine as a single self-contained binary (no webview runtime).
EOF
install -Dm755 "$DEB/postinst" "$STAGE/DEBIAN/postinst"
install -Dm755 "$DEB/prerm" "$STAGE/DEBIAN/prerm"

OUT="$ROOT/target/sampa2_${VERSION}_${ARCH}.deb"
echo "==> building $OUT"
dpkg-deb --root-owner-group --build "$STAGE" "$OUT" >/dev/null
echo "built: $OUT ($(du -h "$OUT" | cut -f1))"
