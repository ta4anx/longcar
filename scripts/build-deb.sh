#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the longcar Debian package and its checksum file.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"

VERSION="0.1.0"
ARCH="amd64"
PKG="longcar_${VERSION}_${ARCH}"
STAGE="build/deb/${PKG}"

cargo build --release --workspace
./scripts/build-extension.sh

rm -rf "$STAGE"
install -d "$STAGE/DEBIAN" \
    "$STAGE/usr/bin" \
    "$STAGE/usr/libexec" \
    "$STAGE/usr/share/applications" \
    "$STAGE/usr/share/metainfo" \
    "$STAGE/usr/share/icons/hicolor/scalable/apps" \
    "$STAGE/usr/share/dbus-1/services" \
    "$STAGE/usr/lib/systemd/user" \
    "$STAGE/usr/share/longcar" \
    "$STAGE/usr/share/doc/longcar"

install -m0755 target/release/longcar "$STAGE/usr/bin/longcar"
install -m0755 target/release/longcarctl "$STAGE/usr/bin/longcarctl"
install -m0755 target/release/longcar-daemon "$STAGE/usr/libexec/longcar-daemon"
install -m0644 data/io.github.ta4anx.Longcar.desktop "$STAGE/usr/share/applications/"
install -m0644 data/io.github.ta4anx.Longcar.metainfo.xml "$STAGE/usr/share/metainfo/"
install -m0644 data/icons/hicolor/scalable/apps/io.github.ta4anx.Longcar.svg \
    "$STAGE/usr/share/icons/hicolor/scalable/apps/"
install -m0644 data/io.github.ta4anx.Longcar.Daemon.service "$STAGE/usr/share/dbus-1/services/"
install -m0644 data/longcar-daemon.service "$STAGE/usr/lib/systemd/user/"
install -m0644 build/longcar@ta4anx.github.io.shell-extension.zip "$STAGE/usr/share/longcar/"
install -m0644 README.md "$STAGE/usr/share/doc/longcar/README.md"
install -m0644 LICENSE "$STAGE/usr/share/doc/longcar/copyright"

cat > "$STAGE/DEBIAN/control" <<EOF
Package: longcar
Version: ${VERSION}-1
Architecture: ${ARCH}
Maintainer: Kaan Çakmak <ta4anx@users.noreply.github.com>
Depends: libgtk-4-1 (>= 4.22), libadwaita-1-0 (>= 1.9), libgstreamer1.0-0 (>= 1.28), gstreamer1.0-pipewire, gstreamer1.0-plugins-base, gstreamer1.0-plugins-good, gstreamer1.0-gtk4, pipewire, dbus-user-session, systemd
Recommends: gnome-shell (>= 50)
Section: net
Priority: optional
Description: GNOME virtual displays for Sunshine/Moonlight
 Longcar creates virtual displays on GNOME Wayland through Mutter's ScreenCast
 API and streams them to Moonlight or Artemis clients with one isolated stock
 Sunshine instance per screen.
EOF

cat > "$STAGE/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q /usr/share/icons/hicolor || true
fi
exit 0
EOF
chmod 0755 "$STAGE/DEBIAN/postinst"

dpkg-deb --build --root-owner-group "$STAGE" "build/${PKG}.deb"
(cd build && sha256sum "${PKG}.deb" "longcar@ta4anx.github.io.shell-extension.zip" > SHA256SUMS)
echo "built build/${PKG}.deb"
