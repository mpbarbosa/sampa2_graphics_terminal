#!/usr/bin/env bash
# Build an .rpm for sampa2 from the release binary + assets (N3 — Linux citizen).
# Needs `rpmbuild` (on RPM distros it's stock; on Debian/Ubuntu: `sudo apt install rpm`).
# Output: target/sampa2-<version>-1.<dist>.x86_64.rpm
#
#   ./packaging/build-rpm.sh
set -euo pipefail
umask 022

ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN="$ROOT/target/release/sampa2"
DEB="$ROOT/packaging/deb"
VERSION=$(grep -m1 '^version' "$ROOT/crates/sampa-native/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')

command -v rpmbuild >/dev/null 2>&1 || {
    echo "error: rpmbuild not found. Install it: 'sudo apt install rpm' (Debian/Ubuntu)" >&2
    echo "       or use the native rpm-build package on an RPM distro." >&2
    exit 1
}

if [ ! -x "$BIN" ]; then
    echo "==> release binary missing; building"
    (cd "$ROOT" && cargo build --release -p sampa-native)
fi

TOP=$(mktemp -d)
trap 'rm -rf "$TOP"' EXIT
mkdir -p "$TOP"/{SOURCES,SPECS,BUILD,RPMS,SRPMS}

echo "==> staging sources"
install -m755 "$BIN" "$TOP/SOURCES/sampa2"
strip "$TOP/SOURCES/sampa2" 2>/dev/null || true
install -m644 "$ROOT/assets/sampa2.desktop" "$TOP/SOURCES/sampa2.desktop"
install -m644 "$DEB/sampa2.1" "$TOP/SOURCES/sampa2.1"
install -m644 "$ROOT/LICENSE" "$TOP/SOURCES/LICENSE"
for sz in 64 128 256 512; do
    install -m644 "$ROOT/assets/sampa2-$sz.png" "$TOP/SOURCES/sampa2-$sz.png"
done
install -m644 "$ROOT/assets/sampa2-icon.svg" "$TOP/SOURCES/sampa2-icon.svg"

echo "==> rpmbuild"
rpmbuild -bb \
    --define "_topdir $TOP" \
    --define "_sampa_version $VERSION" \
    "$ROOT/packaging/rpm/sampa2.spec" >/dev/null

RPM=$(find "$TOP/RPMS" -name '*.rpm' | head -1)
OUT="$ROOT/target/$(basename "$RPM")"
cp "$RPM" "$OUT"
echo "built: $OUT ($(du -h "$OUT" | cut -f1))"
