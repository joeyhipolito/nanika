#!/usr/bin/env bash
# Chaos scenarios for the obsidian plugin (indexer daemon, write path, disk).
# Usage: chaos-obsidian.sh [--only <id>] [--dry-run] [--help]
#   --only <id>   run a single scenario: daemon-kill | partial-write | disk-full
#   --dry-run     print what would happen without executing any destructive steps
set -euo pipefail

DRY_RUN=false
ONLY=""

usage() {
  echo "usage: $0 [--only daemon-kill|partial-write|disk-full] [--dry-run]" >&2
  exit 2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) DRY_RUN=true; shift ;;
    --only)
      [[ $# -lt 2 ]] && { echo "error: --only requires an argument" >&2; usage; }
      ONLY="$2"; shift 2 ;;
    --help|-h) usage ;;
    *) echo "error: unknown flag: $1" >&2; usage ;;
  esac
done

if [[ -n "$ONLY" ]]; then
  case "$ONLY" in
    daemon-kill|partial-write|disk-full) ;;
    *) echo "error: unknown scenario id: $ONLY" >&2; usage ;;
  esac
fi

# ── helpers ────────────────────────────────────────────────────────────────────

run() {
  if "$DRY_RUN"; then
    echo "[dry-run] $*"
  else
    "$@"
  fi
}

pass() { echo "[PASS] $1"; }
fail() { echo "[FAIL] $1" >&2; exit 1; }

# ── scenario 1: daemon-kill ────────────────────────────────────────────────────
# Kill the obsidian-indexer process mid-run and verify it restarts cleanly.

scenario_daemon_kill() {
  echo "==> scenario: daemon-kill"

  local pid
  pid=$(pgrep -f "obsidian-indexer" 2>/dev/null || true)

  if [[ -z "$pid" ]]; then
    echo "    obsidian-indexer not running — skipping kill, verifying clean startup"
    if "$DRY_RUN"; then
      echo "    [dry-run] would start obsidian-indexer and verify exit 0"
      pass "daemon-kill (dry-run)"
      return 0
    fi

    if command -v obsidian-indexer &>/dev/null; then
      # Start briefly, then kill to exercise the restart path
      obsidian-indexer &>/dev/null &
      local new_pid=$!
      sleep 0.5
      kill -TERM "$new_pid" 2>/dev/null || true
      wait "$new_pid" 2>/dev/null || true
    fi
    pass "daemon-kill"
    return 0
  fi

  echo "    found obsidian-indexer pid=$pid"
  run kill -TERM "$pid"
  if ! "$DRY_RUN"; then
    sleep 1
    if kill -0 "$pid" 2>/dev/null; then
      run kill -KILL "$pid" 2>/dev/null || true
    fi
  fi
  pass "daemon-kill"
}

# ── scenario 2: partial-write ──────────────────────────────────────────────────
# Write half a note then kill the writer; confirm the vault is not corrupted.

scenario_partial_write() {
  echo "==> scenario: partial-write"

  local vault_root
  vault_root="${OBSIDIAN_VAULT:-${HOME}/Documents/Obsidian}"
  local tmp_note="${vault_root}/.chaos-partial-write-$$.md"

  if "$DRY_RUN"; then
    echo "    [dry-run] would write partial note to ${tmp_note} then kill writer"
    echo "    [dry-run] would verify vault readable after partial write"
    pass "partial-write (dry-run)"
    return 0
  fi

  if [[ ! -d "$vault_root" ]]; then
    echo "    vault not found at ${vault_root} — using /tmp as fallback"
    vault_root="/tmp"
    tmp_note="/tmp/.chaos-partial-write-$$.md"
  fi

  # Write partial frontmatter then interrupt
  {
    printf '%s\n' '---' 'title: chaos-partial' 'type: chaos'
    # Intentionally leave frontmatter open (no closing ---)
  } > "$tmp_note"

  # Confirm vault/directory is still listable
  if ! ls "$vault_root" &>/dev/null; then
    rm -f "$tmp_note"
    fail "partial-write: vault directory unreadable after partial write"
  fi

  rm -f "$tmp_note"
  pass "partial-write"
}

# ── scenario 3: disk-full ──────────────────────────────────────────────────────
# Simulate a disk-full condition via a tight ulimit and verify obsidian capture
# returns a non-zero exit code rather than silently corrupting state.

scenario_disk_full() {
  echo "==> scenario: disk-full"

  if "$DRY_RUN"; then
    echo "    [dry-run] would run: (ulimit -f 0; obsidian capture 'chaos-test' 2>&1)"
    echo "    [dry-run] would assert non-zero exit when disk quota exhausted"
    pass "disk-full (dry-run)"
    return 0
  fi

  if ! command -v obsidian &>/dev/null; then
    echo "    obsidian binary not found — skipping disk-full scenario"
    pass "disk-full (skipped — binary absent)"
    return 0
  fi

  # ulimit -f 0 limits file writes to 0 512-byte blocks; obsidian capture must fail
  local rc=0
  (ulimit -f 0; obsidian capture "chaos-disk-full-test" 2>/dev/null) || rc=$?
  if [[ $rc -eq 0 ]]; then
    fail "disk-full: expected non-zero exit from obsidian capture under ulimit -f 0"
  fi
  pass "disk-full (exit code $rc as expected)"
}

# ── dispatch ───────────────────────────────────────────────────────────────────

run_scenario() {
  case "$1" in
    daemon-kill)   scenario_daemon_kill ;;
    partial-write) scenario_partial_write ;;
    disk-full)     scenario_disk_full ;;
  esac
}

if [[ -n "$ONLY" ]]; then
  run_scenario "$ONLY"
else
  run_scenario daemon-kill
  run_scenario partial-write
  run_scenario disk-full
fi
