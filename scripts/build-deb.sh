#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found" >&2
  exit 1
fi
if ! command -v dpkg-deb >/dev/null 2>&1; then
  echo "error: dpkg-deb not found" >&2
  exit 1
fi

PKG_NAME="logtool"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
if [ -z "$VERSION" ]; then
  echo "error: failed to parse version from Cargo.toml" >&2
  exit 1
fi

ARCH="${1:-$(dpkg --print-architecture)}"
OUT_DIR="$ROOT_DIR/dist"
BUILD_ROOT="$ROOT_DIR/packaging/.build"
PKG_DIR="$BUILD_ROOT/${PKG_NAME}_${VERSION}_${ARCH}"
DEBIAN_DIR="$PKG_DIR/DEBIAN"

rm -rf "$PKG_DIR"
mkdir -p \
  "$DEBIAN_DIR" \
  "$PKG_DIR/usr/bin" \
  "$PKG_DIR/usr/lib/systemd/system" \
  "$PKG_DIR/usr/share/doc/$PKG_NAME"

cargo build --release

install -m 0755 "$ROOT_DIR/target/release/logtool" "$PKG_DIR/usr/bin/logtool"
install -m 0755 "$ROOT_DIR/target/release/logtool-daemon" "$PKG_DIR/usr/bin/logtool-daemon"
install -m 0644 "$ROOT_DIR/packaging/deb/logtool.service" "$PKG_DIR/usr/lib/systemd/system/logtool.service"
install -m 0644 "$ROOT_DIR/README.md" "$PKG_DIR/usr/share/doc/$PKG_NAME/README.md"

INSTALLED_SIZE="$(du -sk "$PKG_DIR/usr" | cut -f1)"

cat > "$DEBIAN_DIR/control" <<CONTROL
Package: $PKG_NAME
Version: $VERSION
Section: admin
Priority: optional
Architecture: $ARCH
Maintainer: Logtool Maintainers <maintainers@example.com>
Depends: systemd
Installed-Size: $INSTALLED_SIZE
Description: Ubuntu system error log diagnosis tool
 A lightweight Rust tool to analyze journalctl error logs,
 identify suspicious programs/services, and map related packages.
CONTROL

install -m 0755 "$ROOT_DIR/packaging/deb/postinst" "$DEBIAN_DIR/postinst"
install -m 0755 "$ROOT_DIR/packaging/deb/prerm" "$DEBIAN_DIR/prerm"
install -m 0755 "$ROOT_DIR/packaging/deb/postrm" "$DEBIAN_DIR/postrm"

mkdir -p "$OUT_DIR"
DEB_FILE="$OUT_DIR/${PKG_NAME}_${VERSION}_${ARCH}.deb"

dpkg-deb --build --root-owner-group "$PKG_DIR" "$DEB_FILE"

echo "Built: $DEB_FILE"
