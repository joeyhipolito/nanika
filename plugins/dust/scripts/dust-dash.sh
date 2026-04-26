#!/usr/bin/env bash
# dust-dash — build, install, and launch the dust-dashboard TUI.
#
# The dashboard's Registry spawns dust-aware plugins itself (anything with a
# `dust` block in its plugin.json). Do NOT start tracker dust-serve manually —
# that causes a socket collision and the dashboard skips the plugin.
#
# Flags:
#   --no-build        skip rebuilding dust-dashboard / tracker
#   --release         use release builds for dependent plugins (default: debug)
#   -h|--help         show this help

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
DUST_DIR="$REPO_ROOT/plugins/dust"
TRACKER_DIR="$REPO_ROOT/plugins/tracker"
RUNTIME_DIR="${XDG_RUNTIME_DIR:-$HOME/.alluka/run}/plugins"
DASHBOARD_BIN="$DUST_DIR/target/release/dust-dashboard"
INSTALL_LINK="$HOME/.alluka/bin/dust-dashboard"

build=1
plugin_profile=debug

for arg in "$@"; do
  case "$arg" in
    --no-build) build=0 ;;
    --release)  plugin_profile=release ;;
    -h|--help)
      sed -n '2,12p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) echo "unknown flag: $arg" >&2; exit 2 ;;
  esac
done

# ── Build ────────────────────────────────────────────────────────────────────
if (( build )); then
  echo "→ building dust-dashboard (release)…"
  (cd "$DUST_DIR" && cargo build --release -p dust-dashboard --quiet)

  echo "→ building tracker ($plugin_profile)…"
  if [[ "$plugin_profile" == release ]]; then
    (cd "$TRACKER_DIR" && cargo build --release --quiet)
  else
    (cd "$TRACKER_DIR" && cargo build --quiet)
  fi
fi

# ── Install symlink ──────────────────────────────────────────────────────────
mkdir -p "$(dirname "$INSTALL_LINK")"
ln -sf "$DASHBOARD_BIN" "$INSTALL_LINK"

# ── Clean stale sockets ──────────────────────────────────────────────────────
mkdir -p "$RUNTIME_DIR"
# Kill any orphaned dust-aware plugin processes that may still hold sockets.
pkill -f "tracker dust-serve" 2>/dev/null || true
# Remove leftover sockets so the registry has a clean slate.
find "$RUNTIME_DIR" -maxdepth 1 -name '*.sock' -delete 2>/dev/null || true

# ── Sanity check ─────────────────────────────────────────────────────────────
dust_plugins=$(find "$REPO_ROOT/plugins" -maxdepth 2 -name plugin.json \
  -exec sh -c 'jq -e "has(\"dust\")" "$1" >/dev/null 2>&1 && basename "$(dirname "$1")"' _ {} \; \
  | sort)
echo "→ dust-aware plugins: $(echo "$dust_plugins" | tr '\n' ' ')"
echo "→ launching dust-dashboard…"
echo

# ── Run ──────────────────────────────────────────────────────────────────────
exec "$DASHBOARD_BIN"
