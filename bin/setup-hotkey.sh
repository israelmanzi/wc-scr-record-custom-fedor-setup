#!/usr/bin/env bash
# Bind a key in XFCE's keyboard settings.
#   setup-hotkey.sh [keysym] [command]
# Default: <Super>F9 -> bin/rec-ui-show
# The command may be an absolute path or any shell command.
set -euo pipefail

SELF_DIR="$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")"
KEY="${1:-<Super>F9}"
CMD="${2:-$SELF_DIR/rec-ui-show}"
PROP="/commands/custom/$KEY"
CHANNEL="xfce4-keyboard-shortcuts"

if xfconf-query -c "$CHANNEL" -p "$PROP" >/dev/null 2>&1; then
  xfconf-query -c "$CHANNEL" -p "$PROP" -s "$CMD"
else
  xfconf-query -c "$CHANNEL" -p "$PROP" -n -t string -s "$CMD"
fi

printf 'bound %s -> %s\n' "$KEY" "$CMD"
