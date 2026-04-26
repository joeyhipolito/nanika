#!/usr/bin/env bash
# scripts/nanika-update.sh — Build, install, restart daemons, and verify nanika plugins
# Runs four phases per plugin: build → install → restart → verify
#
# Usage: nanika-update.sh [OPTIONS]
#   --dry-run          Print what would run without executing
#   --skip PLUGIN,...  Skip one or more plugins (comma-separated)
#   --only PLUGIN,...  Process only these plugins (comma-separated)
#   --json             Emit machine-readable JSON array on exit
#
# Env:
#   NANIKA_UPDATE_SKIP  Comma-separated plugin names to always skip
#                       (merged with --skip; useful for plugins that
#                       require unavailable toolchains, e.g. cargo-tauri)
set -eo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ALLUKA_BIN="${HOME}/.alluka/bin"
PLUGINS_DIR="${REPO_ROOT}/plugins"

# ── Flags ─────────────────────────────────────────────────────────────────────

JSON_OUTPUT=0
DRY_RUN=0
SKIP_LIST=()
ONLY_LIST=()

# Env-var seed: persistent skips without retyping --skip every run.
# e.g. `export NANIKA_UPDATE_SKIP=dust,nen_mcp` in ~/.zshrc.
if [[ -n "${NANIKA_UPDATE_SKIP:-}" ]]; then
  IFS=',' read -ra _env_items <<< "${NANIKA_UPDATE_SKIP}"
  SKIP_LIST+=("${_env_items[@]}")
fi

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    --json)
      JSON_OUTPUT=1
      shift
      ;;
    --skip)
      [[ -z "${2:-}" ]] && { printf 'ERROR: --skip requires a plugin name\n' >&2; exit 1; }
      IFS=',' read -ra _items <<< "$2"
      SKIP_LIST+=("${_items[@]}")
      shift 2
      ;;
    --only)
      [[ -z "${2:-}" ]] && { printf 'ERROR: --only requires a plugin name\n' >&2; exit 1; }
      IFS=',' read -ra _items <<< "$2"
      ONLY_LIST+=("${_items[@]}")
      shift 2
      ;;
    -h | --help)
      printf 'Usage: %s [--dry-run] [--skip PLUGIN,...] [--only PLUGIN,...] [--json]\n' \
        "$(basename "$0")"
      exit 0
      ;;
    *)
      printf 'ERROR: Unknown flag: %s\n' "$1" >&2
      exit 1
      ;;
  esac
done

# ── Colors ────────────────────────────────────────────────────────────────────

if [[ -t 1 && "$JSON_OUTPUT" -eq 0 ]]; then
  RED=$'\033[0;31m'
  YELLOW=$'\033[1;33m'
  GREEN=$'\033[0;32m'
  CYAN=$'\033[0;36m'
  BOLD=$'\033[1m'
  DIM=$'\033[2m'
  RESET=$'\033[0m'
else
  RED=''
  YELLOW=''
  GREEN=''
  CYAN=''
  BOLD=''
  DIM=''
  RESET=''
fi

# ── Output Helpers ─────────────────────────────────────────────────────────────

phase_header() {
  [[ "$JSON_OUTPUT" -eq 1 ]] && return
  printf '\n%s%s── %s ─────────────────────────────────%s\n' "$BOLD" "$CYAN" "$*" "$RESET"
}

step() {
  [[ "$JSON_OUTPUT" -eq 1 ]] && return
  printf '%s==>%s %s%s%s\n' "$CYAN" "$RESET" "$BOLD" "$*" "$RESET"
}

ok() {
  [[ "$JSON_OUTPUT" -eq 1 ]] && return
  printf '  %s✓%s %s\n' "$GREEN" "$RESET" "$*"
}

skip_line() {
  [[ "$JSON_OUTPUT" -eq 1 ]] && return
  printf '  %s– %s%s\n' "$DIM" "$*" "$RESET"
}

warn() {
  [[ "$JSON_OUTPUT" -eq 1 ]] && return
  printf '  %sWARNING: %s%s\n' "$YELLOW" "$*" "$RESET" >&2
}

loud_error() {
  printf '%sERROR: %s%s\n' "$RED" "$*" "$RESET" >&2
}

# ── JSON Result Accumulator ───────────────────────────────────────────────────

_JSON_LINES=()

json_record() {
  local plugin="$1" phase="$2" status="$3" detail="${4:-}"
  detail="${detail//\\/\\\\}"
  detail="${detail//\"/\\\"}"
  _JSON_LINES+=("{\"plugin\":\"${plugin}\",\"phase\":\"${phase}\",\"status\":\"${status}\",\"detail\":\"${detail}\"}")
}

emit_json() {
  [[ "$JSON_OUTPUT" -eq 0 ]] && return
  local first=1
  printf '[\n'
  local line
  for line in "${_JSON_LINES[@]}"; do
    if [[ "$first" -eq 1 ]]; then
      printf '  %s\n' "$line"
      first=0
    else
      printf ', %s\n' "$line"
    fi
  done
  printf ']\n'
}

trap 'emit_json' EXIT

# ── python3 plugin.json Reader ────────────────────────────────────────────────
# JSON content is delivered via stdin to avoid shell injection on file paths.
# The field to extract is passed via the _PJSON_FIELD environment variable.
# A missing key returns empty output (exit 0).
# A malformed JSON document prints a loud error to stderr and exits 2.

_PY_SCRIPT='
import json, sys, os

try:
    data = json.load(sys.stdin)
except json.JSONDecodeError as e:
    path = os.environ.get("_PJSON_PATH", "<unknown>")
    sys.stderr.write("ERROR: malformed plugin.json at " + path + ": " + str(e) + "\n")
    sys.exit(2)

field = os.environ.get("_PJSON_FIELD", "")
val = data.get(field)
if val is None:
    sys.exit(0)
elif isinstance(val, (list, dict)):
    print(json.dumps(val))
else:
    print(val)
'

pjson_get() {
  local pjson="$1" field="$2"
  local content

  if ! content=$(cat "$pjson" 2>/dev/null); then
    loud_error "Cannot read: $pjson"
    return 1
  fi

  # Feed JSON via stdin; field name via env var — no shell interpolation into python source
  printf '%s' "$content" \
    | _PJSON_FIELD="$field" _PJSON_PATH="$pjson" python3 -c "$_PY_SCRIPT"
}

# ── Plugin Discovery ──────────────────────────────────────────────────────────

DISCOVERED_PLUGINS=()

discover_plugins() {
  local pjson plugin_dir plugin_name

  while IFS= read -r pjson; do
    plugin_dir="$(dirname "$pjson")"
    # Do NOT silence stderr — malformed JSON errors must surface loudly
    if ! plugin_name=$(pjson_get "$pjson" "name"); then
      loud_error "Skipping plugin with unreadable or malformed plugin.json: $pjson"
      continue
    fi
    if [[ -z "$plugin_name" ]]; then
      loud_error "Skipping plugin.json missing 'name' field: $pjson"
      continue
    fi
    DISCOVERED_PLUGINS+=("${plugin_name}:${plugin_dir}")
  done < <(find "$PLUGINS_DIR" -maxdepth 2 -name 'plugin.json' | sort)
}

# ── Filter Helpers ────────────────────────────────────────────────────────────

in_list() {
  local needle="$1"
  shift
  local item
  for item in "$@"; do
    [[ "$item" == "$needle" ]] && return 0
  done
  return 1
}

should_process() {
  local name="$1"
  if [[ ${#ONLY_LIST[@]} -gt 0 ]]; then
    in_list "$name" "${ONLY_LIST[@]}" || return 1
  fi
  if [[ ${#SKIP_LIST[@]} -gt 0 ]]; then
    in_list "$name" "${SKIP_LIST[@]}" && return 1
  fi
  return 0
}

# ── Daemon Registry ───────────────────────────────────────────────────────────
# Only scheduler and nen ship long-running daemons.
# Stop is best-effort (daemon may not be running).
# Start launches in background; disown detaches from the shell job table.

daemon_stop() {
  local name="$1"
  case "$name" in
    scheduler) scheduler daemon --stop 2>/dev/null || true ;;
    nen)       nen-daemon stop 2>/dev/null || true ;;
  esac
}

daemon_start() {
  local name="$1"
  case "$name" in
    scheduler) scheduler daemon & disown ;;
    nen)       nen-daemon start & disown ;;
  esac
}

is_daemon_plugin() {
  local name="$1"
  case "$name" in
    scheduler | nen) return 0 ;;
    *) return 1 ;;
  esac
}

# ── Phase: Voice Checks ───────────────────────────────────────────────────────
# Runs voice existence + drift checks against persona files before any plugin
# work. Either failure aborts the run — letting the build continue would ship
# persona drift silently.

run_voice_check() {
  local check_name="$1" script_path="$2"

  if [[ "$DRY_RUN" -eq 1 ]]; then
    skip_line "[dry-run] bash ${script_path}"
    json_record "voice" "$check_name" "dry-run" "$script_path"
    return 0
  fi

  local rc=0 output
  output=$(bash "$script_path" 2>&1) || rc=$?
  if [[ $rc -ne 0 ]]; then
    loud_error "voice ${check_name} check failed (exit $rc)"
    if [[ "$JSON_OUTPUT" -eq 0 && -n "$output" ]]; then
      printf '%s\n' "$output" >&2
    fi
    json_record "voice" "$check_name" "failed" "exit $rc"
    return "$rc"
  fi

  ok "voice ${check_name}: ${output#OK: }"
  json_record "voice" "$check_name" "ok"
  return 0
}

phase_voice_checks() {
  phase_header "voice checks"

  run_voice_check "existence" "${REPO_ROOT}/scripts/check-voice-existence.sh" || return $?
  run_voice_check "drift"     "${REPO_ROOT}/scripts/check-voice-drift.sh"     || return $?
  return 0
}

# ── Phase: Build ──────────────────────────────────────────────────────────────

phase_build() {
  local name="$1" dir="$2" pjson="$3"
  local build_cmd rc

  if ! build_cmd=$(pjson_get "$pjson" "build"); then
    json_record "$name" "build" "error" "failed to read build field"
    return 1
  fi

  if [[ -z "$build_cmd" ]]; then
    json_record "$name" "build" "skipped" "no build command"
    skip_line "$name build: no build command"
    return 0
  fi

  if [[ "$DRY_RUN" -eq 1 ]]; then
    skip_line "[dry-run] cd $dir && $build_cmd"
    json_record "$name" "build" "dry-run" "$build_cmd"
    return 0
  fi

  rc=0
  (cd "$dir" && bash -c "$build_cmd") || rc=$?
  if [[ $rc -ne 0 ]]; then
    loud_error "Build failed for $name (exit $rc)"
    json_record "$name" "build" "failed" "exit $rc"
    return "$rc"
  fi

  ok "$name: built"
  json_record "$name" "build" "ok"
}

# ── Phase: Install ─────────────────────────────────────────────────────────────

phase_install() {
  local name="$1" dir="$2" pjson="$3"
  local install_cmd rc

  if ! install_cmd=$(pjson_get "$pjson" "install"); then
    json_record "$name" "install" "error" "failed to read install field"
    return 1
  fi

  if [[ -z "$install_cmd" ]]; then
    json_record "$name" "install" "skipped" "no install command"
    skip_line "$name install: no install command"
    return 0
  fi

  if [[ "$DRY_RUN" -eq 1 ]]; then
    skip_line "[dry-run] cd $dir && $install_cmd"
    json_record "$name" "install" "dry-run" "$install_cmd"
    return 0
  fi

  rc=0
  (cd "$dir" && bash -c "$install_cmd") || rc=$?
  if [[ $rc -ne 0 ]]; then
    loud_error "Install failed for $name (exit $rc)"
    json_record "$name" "install" "failed" "exit $rc"
    return "$rc"
  fi

  ok "$name: installed"
  json_record "$name" "install" "ok"
}

# ── Phase: Restart ────────────────────────────────────────────────────────────

phase_restart() {
  local name="$1"
  local rc

  if ! is_daemon_plugin "$name"; then
    json_record "$name" "restart" "skipped" "not a daemon plugin"
    skip_line "$name restart: not a daemon plugin"
    return 0
  fi

  if [[ "$DRY_RUN" -eq 1 ]]; then
    skip_line "[dry-run] stop + start daemon for $name"
    json_record "$name" "restart" "dry-run"
    return 0
  fi

  # Stop — ignore failure; daemon may not be running
  daemon_stop "$name"

  # Start — background; failure here is real
  if ! daemon_start "$name"; then
    rc=$?
    loud_error "Failed to start daemon for $name (exit $rc)"
    json_record "$name" "restart" "failed" "exit $rc"
    return "$rc"
  fi

  ok "$name: daemon restarted"
  json_record "$name" "restart" "ok"
}

# ── Phase: Verify ─────────────────────────────────────────────────────────────

phase_verify() {
  local name="$1" pjson="$2"
  local binary

  if ! binary=$(pjson_get "$pjson" "binary"); then
    json_record "$name" "verify" "error" "failed to read binary field"
    return 1
  fi

  if [[ -z "$binary" ]]; then
    json_record "$name" "verify" "skipped" "no binary field"
    skip_line "$name verify: no binary field"
    return 0
  fi

  local bin_path="${ALLUKA_BIN}/${binary}"

  if [[ "$DRY_RUN" -eq 1 ]]; then
    skip_line "[dry-run] verify ${bin_path} exists and is executable"
    json_record "$name" "verify" "dry-run" "$bin_path"
    return 0
  fi

  if [[ ! -x "$bin_path" ]]; then
    loud_error "$name: binary not found or not executable: $bin_path"
    json_record "$name" "verify" "failed" "missing: $bin_path"
    return 1
  fi

  ok "$name: verified (${bin_path})"
  json_record "$name" "verify" "ok" "$bin_path"
}

# ── Main ──────────────────────────────────────────────────────────────────────

main() {
  local voice_rc=0
  phase_voice_checks || voice_rc=$?
  if [[ $voice_rc -ne 0 ]]; then
    loud_error "voice checks failed — aborting before plugin work."
    return "$voice_rc"
  fi

  discover_plugins

  if [[ ${#DISCOVERED_PLUGINS[@]} -eq 0 ]]; then
    loud_error "No plugins found in ${PLUGINS_DIR}"
    exit 1
  fi

  local overall_rc=0
  local entry name dir pjson rc

  for entry in "${DISCOVERED_PLUGINS[@]}"; do
    name="${entry%%:*}"
    dir="${entry#*:}"
    pjson="${dir}/plugin.json"

    if ! should_process "$name"; then
      skip_line "$name: filtered out"
      continue
    fi

    step "Plugin: $name"

    # ── build ──
    phase_header "build: $name"
    rc=0
    phase_build "$name" "$dir" "$pjson" || rc=$?
    if [[ $rc -ne 0 ]]; then
      overall_rc=$rc
      warn "$name: build failed (exit $rc) — skipping install/restart/verify"
      continue
    fi

    # ── install ──
    phase_header "install: $name"
    rc=0
    phase_install "$name" "$dir" "$pjson" || rc=$?
    if [[ $rc -ne 0 ]]; then
      overall_rc=$rc
      warn "$name: install failed (exit $rc) — skipping restart/verify"
      continue
    fi

    # ── restart ──
    phase_header "restart: $name"
    rc=0
    phase_restart "$name" || rc=$?
    if [[ $rc -ne 0 ]]; then
      overall_rc=$rc
      warn "$name: restart failed (exit $rc) — continuing to verify"
    fi

    # ── verify ──
    phase_header "verify: $name"
    rc=0
    phase_verify "$name" "$pjson" || rc=$?
    if [[ $rc -ne 0 ]]; then
      overall_rc=$rc
    fi
  done

  if [[ "$JSON_OUTPUT" -eq 0 ]]; then
    printf '\n'
    if [[ $overall_rc -eq 0 ]]; then
      printf '%sAll plugins updated successfully.%s\n' "$GREEN" "$RESET"
    else
      printf '%sSome plugins failed to update (see errors above).%s\n' "$RED" "$RESET"
    fi
  fi

  return "$overall_rc"
}

main
