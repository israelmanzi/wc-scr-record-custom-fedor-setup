#!/usr/bin/env bash
# Binds one recorder shortcut in XFCE:
#   Super+F9 -> show/focus the desktop launcher
# The old Super+F10 screen-recording shortcut is removed.
set -euo pipefail

SELF_DIR="$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")"
CHANNEL="xfce4-keyboard-shortcuts"

"$SELF_DIR/setup-hotkey.sh" '<Super>F9' "$SELF_DIR/rec-ui-show"

OLD_PROP="/commands/custom/<Super>F10"
if xfconf-query -c "$CHANNEL" -p "$OLD_PROP" >/dev/null 2>&1; then
  xfconf-query -c "$CHANNEL" -p "$OLD_PROP" -r
  printf 'removed old shortcut: %s\n' "$OLD_PROP"
fi
