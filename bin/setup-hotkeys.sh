#!/usr/bin/env bash
# Binds both recorder hotkeys in XFCE.
#   Super+F9  -> webcam-only recording + floating preview
#   Super+F10 -> screen recording with the webcam bubble baked in
set -euo pipefail

SELF_DIR="$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")"

"$SELF_DIR/setup-hotkey.sh" '<Super>F9' toggle
"$SELF_DIR/setup-hotkey.sh" '<Super>F10' screen
