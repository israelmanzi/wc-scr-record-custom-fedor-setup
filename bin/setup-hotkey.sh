#!/usr/bin/env bash
# Binds a key in XFCE's keyboard settings.
#   setup-hotkey.sh [keysym] [toggle|screen]
#     toggle (default) -> rec toggle          (Super+F9)
#     screen            -> rec screen toggle  (Super+F10)
set -euo pipefail

SELF_DIR="$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")"
KEY="${1:-<Super>F9}"
ACTION="${2:-toggle}"
PROP="/commands/custom/$KEY"
CHANNEL="xfce4-keyboard-shortcuts"

case "$ACTION" in
  toggle) CMD="$SELF_DIR/rec toggle" ;;
  screen) CMD="$SELF_DIR/rec screen toggle" ;;
  *)      CMD="$ACTION" ;;
esac

if xfconf-query -c "$CHANNEL" -p "$PROP" >/dev/null 2>&1; then
  xfconf-query -c "$CHANNEL" -p "$PROP" -s "$CMD"
else
  xfconf-query -c "$CHANNEL" -p "$PROP" -n -t string -s "$CMD"
fi

printf 'bound %s -> %s\n' "$KEY" "$CMD"
