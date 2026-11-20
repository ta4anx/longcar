#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Install debug binaries, data files, user unit, D-Bus service and the Shell
# extension into the current user's prefix (~/.local). No root required.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"

PREFIX=${PREFIX:-$HOME/.local}
BIN="$PREFIX/bin"
LIBEXEC="$PREFIX/libexec"
APPS="$PREFIX/share/applications"
METAINFO="$PREFIX/share/metainfo"
ICONS="$PREFIX/share/icons/hicolor/scalable/apps"
DBUS_SERVICES="$PREFIX/share/dbus-1/services"
USER_UNITS="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
EXT_DIR="$PREFIX/share/gnome-shell/extensions"

cargo build --workspace

install -d "$BIN" "$LIBEXEC" "$APPS" "$METAINFO" "$ICONS" "$DBUS_SERVICES" "$USER_UNITS"

install -m0755 target/debug/longcar "$BIN/longcar"
install -m0755 target/debug/longcarctl "$BIN/longcarctl"
install -m0755 target/debug/longcar-daemon "$LIBEXEC/longcar-daemon"
install -m0644 data/io.github.ta4anx.Longcar.desktop "$APPS/"
install -m0644 data/io.github.ta4anx.Longcar.metainfo.xml "$METAINFO/"
install -m0644 data/icons/hicolor/scalable/apps/io.github.ta4anx.Longcar.svg "$ICONS/"

cat > "$DBUS_SERVICES/io.github.ta4anx.Longcar.Daemon.service" <<EOF
[D-BUS Service]
Name=io.github.ta4anx.Longcar.Daemon
Exec=$LIBEXEC/longcar-daemon
SystemdService=longcar-daemon.service
EOF

cat > "$USER_UNITS/longcar-daemon.service" <<EOF
[Unit]
Description=Longcar virtual display daemon
PartOf=graphical-session.target
After=graphical-session.target

[Service]
Type=dbus
BusName=io.github.ta4anx.Longcar.Daemon
ExecStart=$LIBEXEC/longcar-daemon
Restart=on-failure
RestartSec=2

[Install]
WantedBy=graphical-session.target
EOF

./scripts/build-extension.sh
if [ -f "build/longcar@ta4anx.github.io.shell-extension.zip" ]; then
    gnome-extensions install --force "build/longcar@ta4anx.github.io.shell-extension.zip" || true
fi

systemctl --user daemon-reload || true

echo "Installed to $PREFIX"
echo "Start with: systemctl --user start longcar-daemon.service && longcar"
