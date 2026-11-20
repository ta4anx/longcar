#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Pack the GNOME Shell extension into build/.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"

mkdir -p build
gnome-extensions pack extension/longcar@ta4anx.github.io --force --out-dir build
