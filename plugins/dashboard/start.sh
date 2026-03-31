#!/usr/bin/env bash
# start.sh — launch dashboard for local development
#
# Starts both servers so the dashboard is fully functional without Claude Code:
#   - channel server  (bun, port 7332) — HTTP bridge with SSE; runs in standalone/TTY mode
#   - Vite dev server (npm, port 5173) — React app with proxy rules pointing to channel
#
# Usage:
#   ./start.sh           # default ports
#   DASHBOARD_CHANNEL_PORT=9000 VITE_PORT=9001 ./start.sh

set -euo pipefail

CHANNEL_PORT="${DASHBOARD_CHANNEL_PORT:-7332}"
VITE_PORT="${VITE_PORT:-5173}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ── sanity checks ──────────────────────────────────────────────────────────────

if ! command -v bun &>/dev/null; then
  echo "error: bun not found — install from https://bun.sh" >&2
  exit 1
fi

if ! command -v npm &>/dev/null; then
  echo "error: npm not found — install Node.js from https://nodejs.org" >&2
  exit 1
fi

# ── install deps if needed ─────────────────────────────────────────────────────

if [[ ! -d "$SCRIPT_DIR/frontend/node_modules" ]]; then
  echo "installing app dependencies..."
  (cd "$SCRIPT_DIR/frontend" && npm install)
fi

if [[ ! -d "$SCRIPT_DIR/channel/node_modules" ]]; then
  echo "installing channel dependencies..."
  (cd "$SCRIPT_DIR/channel" && bun install)
fi

# ── cleanup on exit ────────────────────────────────────────────────────────────

cleanup() {
  echo ""
  echo "shutting down..."
  kill "$CHANNEL_PID" "$VITE_PID" 2>/dev/null || true
  wait "$CHANNEL_PID" "$VITE_PID" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# ── start channel server ───────────────────────────────────────────────────────

echo "starting channel server on :${CHANNEL_PORT}..."
DASHBOARD_CHANNEL_PORT="$CHANNEL_PORT" bun run "$SCRIPT_DIR/channel/index.ts" &
CHANNEL_PID=$!

# Wait for channel to be ready (up to 5s)
for i in $(seq 1 10); do
  if curl -s "http://localhost:${CHANNEL_PORT}/events" -o /dev/null --max-time 1 2>/dev/null; then
    break
  fi
  sleep 0.5
done

# ── start Vite dev server ─────────────────────────────────────────────────────

echo "starting Vite dev server on :${VITE_PORT}..."
(cd "$SCRIPT_DIR/frontend" && npm run dev -- --port "$VITE_PORT") &
VITE_PID=$!

echo ""
echo "dashboard running:"
echo "  dashboard:  http://localhost:${VITE_PORT}"
echo "  channel:    http://localhost:${CHANNEL_PORT}"
echo ""
echo "press Ctrl+C to stop"

wait "$CHANNEL_PID" "$VITE_PID"
