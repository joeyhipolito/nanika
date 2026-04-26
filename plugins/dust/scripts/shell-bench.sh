#!/usr/bin/env bash
# shell-bench.sh — dust shell latency measurement harness
#
# Measures four timing buckets across 10+ samples each and emits a Markdown
# results table. Requires the dust binary to already be running (launched by
# dust-shell.sh) and System Events accessibility permissions granted.
#
# Usage:
#   plugins/dust/scripts/shell-bench.sh [--samples N] [--no-launch]
#
# Options:
#   --samples N    Number of samples per bucket (default: 12)
#   --no-launch    Skip launching the app (assumes it is already running)
#   --csv          Also emit raw CSV to ./bench-raw-$(date +%Y%m%d-%H%M%S).csv
#
# Timings measured:
#   (a) hotkey-press to pane-visible     budget: p95 ≤120ms
#   (b) keystroke to first result change budget: p95 ≤50ms
#   (c) Enter-press to component render  budget: p95 ≤250ms
#   (d) ⌘⇧E to CodeMirror ready          budget: p95 ≤150ms
#
# Implementation notes:
#   - (a) uses osascript to synthesize ⌥Space and records wall-clock ms before
#     the synthesized event; the webview emits [bench] first-paint-raf <Date.now()>
#     to OSLog; delta = raf_wallclock - hotkey_wallclock.
#   - (b) uses osascript to type a character; the webview emits
#     [bench] results-updated keystroke=<t0> now=<t1>; delta = t1 - t0.
#   - (c) uses osascript to press Return; [bench] enter-pressed <t0> and
#     [bench] detail-rendered <t1> are paired; delta = t1 - t0.
#   - (d) activates a visible FileRef chip via osascript ⌘⇧E; [bench] editor-focused
#     wall=<W> delta=<D> carries the in-process latency D directly.
#
# All [bench] log lines are harvested from the macOS unified log via:
#   log stream --process dust --predicate 'eventMessage CONTAINS "[bench]"'

set -euo pipefail

SAMPLES=12
LAUNCH=true
EMIT_CSV=false
DUST_APP_NAME="dust"
HOTKEY_CODE=49        # Space bar key code
HOTKEY_MOD="option"   # ⌥

while [[ $# -gt 0 ]]; do
  case "$1" in
    --samples) SAMPLES="$2"; shift 2 ;;
    --no-launch) LAUNCH=false; shift ;;
    --csv) EMIT_CSV=true; shift ;;
    *) echo "Unknown option: $1" >&2; exit 1 ;;
  esac
done

# ---------------------------------------------------------------------------
# Prerequisites
# ---------------------------------------------------------------------------

require_cmd() {
  if ! command -v "$1" &>/dev/null; then
    echo "ERROR: '$1' not found. $2" >&2
    exit 1
  fi
}

require_cmd osascript "Required for event synthesis."
require_cmd python3   "Required for wall-clock milliseconds."
require_cmd log       "Required to read macOS unified log (macOS 10.12+)."
require_cmd bc        "Required for floating-point arithmetic."

wall_ms() {
  python3 -c 'import time; print(int(time.time() * 1000))'
}

# ---------------------------------------------------------------------------
# Launch dust shell if requested
# ---------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

if $LAUNCH; then
  echo "▶ Launching dust shell…"
  "$SCRIPT_DIR/dust-shell.sh" &
  DUST_PID=$!
  echo "  PID: $DUST_PID"
  # Allow Tauri + WebView to fully initialize
  sleep 3
  echo "  Waiting for window to be ready…"
  # Summon once and dismiss to prime the WebView render path
  osascript -e "tell application \"System Events\" to key code $HOTKEY_CODE using $HOTKEY_MOD down" 2>/dev/null || true
  sleep 0.8
  osascript -e 'tell application "System Events" to key code 53' 2>/dev/null || true  # Esc
  sleep 0.4
  echo "  Ready."
fi

# ---------------------------------------------------------------------------
# Start log stream capture
# ---------------------------------------------------------------------------

LOG_RAW=$(mktemp /tmp/dust-bench-XXXXXX.log)
trap 'kill "$LOG_STREAM_PID" 2>/dev/null; rm -f "$LOG_RAW"' EXIT

echo "▶ Starting OSLog capture (process: $DUST_APP_NAME)…"
log stream \
  --process "$DUST_APP_NAME" \
  --predicate 'eventMessage CONTAINS "[bench]"' \
  --style syslog \
  > "$LOG_RAW" 2>&1 &
LOG_STREAM_PID=$!

# Give log stream time to attach
sleep 0.8
echo "  Log stream PID: $LOG_STREAM_PID"

# ---------------------------------------------------------------------------
# Helper: extract the most recent [bench] line matching a pattern
# ---------------------------------------------------------------------------

latest_bench() {
  local pattern="$1"
  grep -oE "\[bench\] $pattern[^\n]*" "$LOG_RAW" 2>/dev/null | tail -1 || true
}

wait_for_bench() {
  local pattern="$1"
  local timeout_sec="${2:-5}"
  local deadline=$(( $(wall_ms) + timeout_sec * 1000 ))
  while true; do
    local line
    line=$(latest_bench "$pattern")
    if [[ -n "$line" ]]; then
      echo "$line"
      return 0
    fi
    if (( $(wall_ms) > deadline )); then
      echo ""
      return 1
    fi
    sleep 0.05
  done
}

# ---------------------------------------------------------------------------
# Statistics helpers (bc-based)
# ---------------------------------------------------------------------------

mean() {
  local vals=("$@")
  local sum=0
  for v in "${vals[@]}"; do sum=$(echo "$sum + $v" | bc); done
  echo "scale=1; $sum / ${#vals[@]}" | bc
}

percentile() {
  # $1 = percentile (50 or 95), remaining args = values
  local pct="$1"; shift
  local vals=("$@")
  local n=${#vals[@]}
  # Sort values
  IFS=$'\n' sorted=($(printf '%s\n' "${vals[@]}" | sort -n))
  local idx=$(echo "scale=0; ($pct * $n) / 100" | bc)
  # Clamp to last index
  (( idx >= n )) && idx=$(( n - 1 ))
  echo "${sorted[$idx]}"
}

# ---------------------------------------------------------------------------
# Bucket A: hotkey-press to pane-visible
# ---------------------------------------------------------------------------

echo ""
echo "━━━ Bucket (a): hotkey → pane-visible [$SAMPLES samples] ━━━"
A_DELTAS=()

for i in $(seq 1 $SAMPLES); do
  # Clear recent bench lines by noting the current log line count
  PREV_LINES=$(wc -l < "$LOG_RAW" || echo 0)

  # Record wall-clock before synthesizing hotkey
  T_HOTKEY=$(wall_ms)

  # Synthesize ⌥Space (global hotkey)
  osascript -e "tell application \"System Events\" to key code $HOTKEY_CODE using $HOTKEY_MOD down" 2>/dev/null

  # Wait for first-paint-raf log line
  DEADLINE=$(( $(wall_ms) + 3000 ))
  PAINT_LINE=""
  while true; do
    # Only look at lines added after our hotkey
    PAINT_LINE=$(tail -n +$((PREV_LINES + 1)) "$LOG_RAW" 2>/dev/null | grep -oE "\[bench\] first-paint-raf [0-9]+" | tail -1 || true)
    [[ -n "$PAINT_LINE" ]] && break
    (( $(wall_ms) > DEADLINE )) && break
    sleep 0.05
  done

  if [[ -n "$PAINT_LINE" ]]; then
    T_PAINT=$(echo "$PAINT_LINE" | grep -oE '[0-9]+$')
    DELTA=$(( T_PAINT - T_HOTKEY ))
    A_DELTAS+=("$DELTA")
    echo "  sample $i: ${DELTA}ms  (paint=$T_PAINT hotkey=$T_HOTKEY)"
  else
    echo "  sample $i: TIMEOUT (no first-paint-raf within 3s)"
  fi

  # Dismiss window (Esc)
  sleep 0.15
  osascript -e 'tell application "System Events" to key code 53' 2>/dev/null
  sleep 0.4
done

# ---------------------------------------------------------------------------
# Bucket B: keystroke → first result-list change
# ---------------------------------------------------------------------------

echo ""
echo "━━━ Bucket (b): keystroke → results-updated [$SAMPLES samples] ━━━"
B_DELTAS=()

# Summon and leave window visible
osascript -e "tell application \"System Events\" to key code $HOTKEY_CODE using $HOTKEY_MOD down" 2>/dev/null
sleep 0.6

for i in $(seq 1 $SAMPLES); do
  PREV_LINES=$(wc -l < "$LOG_RAW" || echo 0)

  # Type a unique character to trigger search
  CHAR=$(echo "abcdefghijklmnopqrstuvwxyz" | cut -c$((( (i - 1) % 26 ) + 1 )))
  osascript -e "tell application \"System Events\" to keystroke \"$CHAR\"" 2>/dev/null

  # Wait for results-updated log
  DEADLINE=$(( $(wall_ms) + 2000 ))
  RESULT_LINE=""
  while true; do
    RESULT_LINE=$(tail -n +$((PREV_LINES + 1)) "$LOG_RAW" 2>/dev/null | grep -oE "\[bench\] results-updated keystroke=[0-9.]+ now=[0-9.]+" | tail -1 || true)
    [[ -n "$RESULT_LINE" ]] && break
    (( $(wall_ms) > DEADLINE )) && break
    sleep 0.02
  done

  if [[ -n "$RESULT_LINE" ]]; then
    T0=$(echo "$RESULT_LINE" | grep -oE 'keystroke=[0-9.]+' | grep -oE '[0-9.]+')
    T1=$(echo "$RESULT_LINE" | grep -oE 'now=[0-9.]+' | grep -oE '[0-9.]+')
    DELTA=$(echo "scale=1; $T1 - $T0" | bc)
    B_DELTAS+=("$DELTA")
    echo "  sample $i: ${DELTA}ms  ($CHAR)"
  else
    echo "  sample $i: TIMEOUT"
  fi

  # Clear search field for next iteration
  osascript -e 'tell application "System Events" to keystroke "a" using command down' 2>/dev/null
  sleep 0.05
  osascript -e 'tell application "System Events" to key code 51' 2>/dev/null  # Delete
  sleep 0.1
done

# ---------------------------------------------------------------------------
# Bucket C: Enter-press → first component render
# ---------------------------------------------------------------------------

echo ""
echo "━━━ Bucket (c): Enter-press → detail-rendered [$SAMPLES samples] ━━━"
C_DELTAS=()

# Type a search query that returns results (empty string returns all)
osascript -e 'tell application "System Events" to keystroke ""' 2>/dev/null
sleep 0.3

for i in $(seq 1 $SAMPLES); do
  # Ensure selection exists (ArrowDown to first result)
  osascript -e 'tell application "System Events" to key code 125' 2>/dev/null  # Down
  sleep 0.05
  osascript -e 'tell application "System Events" to key code 126' 2>/dev/null  # Up (back to 0)
  sleep 0.05

  PREV_LINES=$(wc -l < "$LOG_RAW" || echo 0)

  # Press Enter
  osascript -e 'tell application "System Events" to key code 36' 2>/dev/null

  # Wait for detail-rendered log
  DEADLINE=$(( $(wall_ms) + 5000 ))
  RENDERED_LINE=""
  while true; do
    RENDERED_LINE=$(tail -n +$((PREV_LINES + 1)) "$LOG_RAW" 2>/dev/null | grep -oE "\[bench\] detail-rendered [0-9.]+" | tail -1 || true)
    [[ -n "$RENDERED_LINE" ]] && break
    (( $(wall_ms) > DEADLINE )) && break
    sleep 0.05
  done

  PREV_ENTER=$(tail -n +$((PREV_LINES + 1)) "$LOG_RAW" 2>/dev/null | grep -oE "\[bench\] enter-pressed [0-9.]+" | tail -1 || true)

  if [[ -n "$RENDERED_LINE" && -n "$PREV_ENTER" ]]; then
    T0=$(echo "$PREV_ENTER" | grep -oE '[0-9.]+$')
    T1=$(echo "$RENDERED_LINE" | grep -oE '[0-9.]+$')
    DELTA=$(echo "scale=1; $T1 - $T0" | bc)
    C_DELTAS+=("$DELTA")
    echo "  sample $i: ${DELTA}ms"
  else
    echo "  sample $i: TIMEOUT (enter=$PREV_ENTER rendered=$RENDERED_LINE)"
  fi

  sleep 0.3
done

# ---------------------------------------------------------------------------
# Bucket D: ⌘⇧E → CodeMirror editor ready
# ---------------------------------------------------------------------------

echo ""
echo "━━━ Bucket (d): ⌘⇧E → editor-focused [$SAMPLES samples] ━━━"
D_DELTAS=()

# Navigate to a result that renders a FileRef component
# Assumes the tracker or obsidian plugin exposes file references.
# If no FileRef is visible, this bucket will report TIMEOUT.
echo "  NOTE: requires a visible FileRef chip in the detail pane."
echo "        Navigate to a result that renders file refs before running."

for i in $(seq 1 $SAMPLES); do
  PREV_LINES=$(wc -l < "$LOG_RAW" || echo 0)

  # Record wall-clock before synthesizing ⌘⇧E
  T_TRIGGER=$(wall_ms)

  # Synthesize ⌘⇧E
  osascript -e 'tell application "System Events" to key code 14 using {command down, shift down}' 2>/dev/null

  # Wait for editor-focused log
  DEADLINE=$(( $(wall_ms) + 5000 ))
  EDITOR_LINE=""
  while true; do
    EDITOR_LINE=$(tail -n +$((PREV_LINES + 1)) "$LOG_RAW" 2>/dev/null | grep -oE "\[bench\] editor-focused wall=[0-9]+ delta=[0-9.]+" | tail -1 || true)
    [[ -n "$EDITOR_LINE" ]] && break
    (( $(wall_ms) > DEADLINE )) && break
    sleep 0.05
  done

  if [[ -n "$EDITOR_LINE" ]]; then
    # Use the in-process delta (performance.now() based, most accurate)
    DELTA=$(echo "$EDITOR_LINE" | grep -oE 'delta=[0-9.]+' | grep -oE '[0-9.]+')
    D_DELTAS+=("$DELTA")
    echo "  sample $i: ${DELTA}ms"
    # Close editor with Escape
    osascript -e 'tell application "System Events" to key code 53' 2>/dev/null
    sleep 0.3
  else
    echo "  sample $i: TIMEOUT (no FileRef chip visible — skip)"
  fi
done

# ---------------------------------------------------------------------------
# Statistics
# ---------------------------------------------------------------------------

echo ""
echo "━━━ Computing statistics ━━━"

print_stats() {
  local label="$1"
  local budget="$2"
  local unit="ms"
  shift 2
  local vals=("$@")

  if [[ ${#vals[@]} -eq 0 ]]; then
    printf "  %-40s  n=0  (no samples collected)\n" "$label"
    return
  fi

  local m p50 p95
  m=$(mean "${vals[@]}")
  p50=$(percentile 50 "${vals[@]}")
  p95=$(percentile 95 "${vals[@]}")

  local status="PASS"
  # Compare p95 (integer) vs budget
  if (( $(echo "$p95 > $budget" | bc -l) )); then
    status="FAIL"
  fi

  printf "  %-40s  n=%-3d  mean=%-7s  p50=%-7s  p95=%-7s  [%s]\n" \
    "$label" "${#vals[@]}" "${m}ms" "${p50}ms" "${p95}ms" "$status"
}

echo ""
echo "┌─────────────────────────────────────────────────────────────────────────────────────────┐"
echo "│  dust shell latency — benchmark results                                                 │"
echo "├─────────────────────────────────────────────────────────────────────────────────────────┤"

print_stats "(a) hotkey → pane-visible    (budget 120ms p95)"  120 "${A_DELTAS[@]+"${A_DELTAS[@]}"}"
print_stats "(b) keystroke → results      (budget  50ms p95)"   50 "${B_DELTAS[@]+"${B_DELTAS[@]}"}"
print_stats "(c) Enter → component render (budget 250ms p95)"  250 "${C_DELTAS[@]+"${C_DELTAS[@]}"}"
print_stats "(d) ⌘⇧E → editor ready       (budget 150ms p95)"  150 "${D_DELTAS[@]+"${D_DELTAS[@]}"}"

echo "└─────────────────────────────────────────────────────────────────────────────────────────┘"

# ---------------------------------------------------------------------------
# Optional CSV output
# ---------------------------------------------------------------------------

if $EMIT_CSV; then
  CSV_FILE="bench-raw-$(date +%Y%m%d-%H%M%S).csv"
  {
    echo "bucket,sample,ms"
    for v in "${A_DELTAS[@]+"${A_DELTAS[@]}"}"; do echo "a,,$v"; done
    for v in "${B_DELTAS[@]+"${B_DELTAS[@]}"}"; do echo "b,,$v"; done
    for v in "${C_DELTAS[@]+"${C_DELTAS[@]}"}"; do echo "c,,$v"; done
    for v in "${D_DELTAS[@]+"${D_DELTAS[@]}"}"; do echo "d,,$v"; done
  } > "$CSV_FILE"
  echo ""
  echo "Raw data → $CSV_FILE"
fi

echo ""
echo "Done. Kill the app manually if launched with --launch."
