#!/usr/bin/env bash
# Build a .deb for sampa2 from the release binary + assets (N3 — Linux citizen).
# Self-contained: needs only `dpkg-deb` (no cargo-deb). Output: target/sampa2_<ver>_<arch>.deb
#
#   ./packaging/build-deb.sh            # (re)builds release, verifies version, then packages
#   sudo apt install ./target/sampa2_*.deb
set -euo pipefail
umask 022   # so staged dirs are 0755 / files 0644 (Debian policy)

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DEB="$ROOT/packaging/deb"
BIN="$ROOT/target/release/sampa2"
VERSION=$(grep -m1 '^version' "$ROOT/crates/sampa-native/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')
ARCH=$(dpkg --print-architecture)

# Build the optimized binary. Always invoke cargo (incremental — a no-op when nothing has
# changed) so a stale artifact from a previous version is never silently packaged. Then
# verify the freshly built binary's own version matches Cargo.toml before staging, and abort
# on any mismatch — the packaged binary and the control metadata must agree.
echo "==> building release binary (v$VERSION)"
(cd "$ROOT" && cargo build --release -p sampa-native)
BIN_VERSION=$("$BIN" --version 2>/dev/null | awk '{print $NF}')
if [ "$BIN_VERSION" != "$VERSION" ]; then
    echo "error: built binary reports '$BIN_VERSION' but Cargo.toml is '$VERSION' —" >&2
    echo "       refusing to package a mismatched binary." >&2
    exit 1
fi
echo "==> binary version verified: $BIN_VERSION"

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
# Pin libc6 to the glibc the binary was built against (the max GLIBC_* symbol version it
# references) so apt refuses to install on too-old systems instead of installing a binary
# that fails at runtime with "GLIBC_x.y not found". (Build on an older base for a lower
# floor / wider reach.) The strip above leaves the dynamic version table intact.
GLIBC_MAX=$(objdump -T "$STAGE/usr/bin/sampa2" 2>/dev/null \
    | grep -oE 'GLIBC_[0-9]+\.[0-9]+' | sort -V | tail -1 | sed 's/GLIBC_//')
LIBC_DEP="libc6"
[ -n "$GLIBC_MAX" ] && LIBC_DEP="libc6 (>= $GLIBC_MAX)"
# libgcc-s1 is hard-linked too; the display + Vulkan stack is dlopened at runtime, so
# libvulkan1 is required to init wgpu, and the window-system client libs + GPU driver are
# Recommends (present on any desktop; the exact one depends on the session).
cat > "$STAGE/DEBIAN/control" <<EOF
Package: sampa2
Version: $VERSION
Architecture: $ARCH
Maintainer: Marcelo Pereira Barbosa <mpbarbosa@gmail.com>
Installed-Size: $INSTALLED_KB
Depends: $LIBC_DEP, libgcc-s1, libvulkan1
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
