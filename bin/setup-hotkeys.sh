#!/usr/bin/env bash
# Binds the recorder shortcuts in XFCE:
#   Super+F9  -> show/focus the desktop launcher
#   Super+F10 -> hide/reveal the screen while screen recording
set -euo pipefail

SELF_DIR="$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")"

"$SELF_DIR/setup-hotkey.sh" '<Super>F9' "$SELF_DIR/rec-ui-show"
"$SELF_DIR/setup-hotkey.sh" '<Super>F10' "$SELF_DIR/rec-privacy"
