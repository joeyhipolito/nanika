#!/usr/bin/env bash
# dust-shell.sh — Launch the dust desktop shell.
#
# Once running, press ⌥Space from any macOS app to summon the command pane.
# Press ⌥Space again, Esc, or click outside to dismiss it.
#
# Usage:
#   scripts/dust-shell.sh            # launch (build if needed)
#   scripts/dust-shell.sh --build    # force a release build first
#   scripts/dust-shell.sh --help

set -euo pipefail

PLUGIN_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Tauri 2 builds into src-tauri/target/, not plugin-root/target/
TAURI_TARGET="$PLUGIN_DIR/src-tauri/target/release"
APP_BUNDLE="$TAURI_TARGET/bundle/macos/dust.app"
RAW_BIN="$TAURI_TARGET/dust-tauri"
LOG_FILE="/tmp/dust-shell.log"

usage() {
  cat <<EOF
dust-shell — launch the dust desktop shell

Usage: $(basename "$0") [--build] [--help]

  --build   Force a release build before launching.
  --help    Show this message.

Logs: $LOG_FILE
EOF
}

do_build() {
  echo "[dust-shell] building release binary..."
  cd "$PLUGIN_DIR"
  npm run tauri:build -- --no-bundle 2>&1 | tail -20
}

# Parse flags
FORCE_BUILD=false
for arg in "$@"; do
  case "$arg" in
    --build) FORCE_BUILD=true ;;
    --help|-h) usage; exit 0 ;;
    *) echo "[dust-shell] unknown flag: $arg" >&2; usage >&2; exit 1 ;;
  esac
done

# Already running?
if pgrep -f "dust-tauri" > /dev/null 2>&1; then
  echo "[dust-shell] already running — press ⌥Space to summon"
  exit 0
fi

# Clean up orphaned plugin subprocesses + stale sockets from a previous
# crashed/killed dust-tauri. Without this, Registry::new() hits a socket
# collision and the plugin isn't registered → "No capabilities found".
RUNTIME_DIR="${XDG_RUNTIME_DIR:-$HOME/.alluka/run}/plugins"
pkill -f "tracker dust-serve" 2>/dev/null || true
pkill -f "nanika-chat" 2>/dev/null || true
find "$RUNTIME_DIR" -maxdepth 1 -name '*.sock' -delete 2>/dev/null || true

[[ "$FORCE_BUILD" == true ]] && do_build

if [[ -d "$APP_BUNDLE" ]]; then
  # .app bundle: open in background (no Dock bounce, no foreground steal)
  open -g "$APP_BUNDLE"
  echo "[dust-shell] launched via .app bundle — press ⌥Space to summon"
elif [[ -f "$RAW_BIN" ]]; then
  # Raw binary: background with nohup
  nohup "$RAW_BIN" > "$LOG_FILE" 2>&1 &
  disown
  echo "[dust-shell] launched (pid $!) — press ⌥Space to summon"
  echo "[dust-shell] logs: $LOG_FILE"
else
  echo "[dust-shell] no release binary found. Build first:" >&2
  echo "  $0 --build" >&2
  echo "  — or —" >&2
  echo "  cd $PLUGIN_DIR && npm run tauri:build -- --no-bundle" >&2
  exit 1
fi
