#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Run the daemon and GUI from the source tree against the live GNOME session.
set -uo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT" || exit 1

DAEMON_PID=""

cleanup() {
    trap - EXIT INT TERM
    if [ -n "$DAEMON_PID" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
        kill "$DAEMON_PID" 2>/dev/null
        for _ in $(seq 1 50); do
            kill -0 "$DAEMON_PID" 2>/dev/null || break
            sleep 0.1
        done
    fi
    # Leftover transient Sunshine units from a previous dev run.
    for unit in $(systemctl --user list-units --all --no-legend 'longcar-sunshine-*' 2>/dev/null | awk '{print $1}'); do
        systemctl --user stop "$unit" 2>/dev/null
        systemctl --user reset-failed "$unit" 2>/dev/null
    done
}
trap cleanup EXIT INT TERM

echo "==> building workspace"
cargo build --workspace || exit 1

echo "==> stopping installed longcar-daemon.service"
systemctl --user stop longcar-daemon.service 2>/dev/null

for unit in $(systemctl --user list-units --all --no-legend 'longcar-sunshine-*' 2>/dev/null | awk '{print $1}'); do
    systemctl --user stop "$unit" 2>/dev/null
    systemctl --user reset-failed "$unit" 2>/dev/null
done

echo "==> starting daemon"
RUST_LOG=${RUST_LOG:-longcar=debug} ./target/debug/longcar-daemon &
DAEMON_PID=$!

for _ in $(seq 1 100); do
    if gdbus call --session \
        --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
        --method org.freedesktop.DBus.NameHasOwner io.github.ta4anx.Longcar.Daemon 2>/dev/null | grep -q true; then
        break
    fi
    kill -0 "$DAEMON_PID" 2>/dev/null || { echo "daemon exited early" >&2; exit 1; }
    sleep 0.1
done

echo "==> starting GUI"
./target/debug/longcar "$@"
