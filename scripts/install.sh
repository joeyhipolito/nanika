#!/usr/bin/env bash
# scripts/install.sh — Interactive installer for Nanika
# Idempotent: safe to run multiple times.
set -eo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# ── Plugin Registry ──────────────────────────────────────────────────────────
# Categories: core (always installed), optional (opt-in)

CORE_SKILLS=(orchestrator)
CORE_PLUGINS=(nen tracker scheduler)
OPTIONAL_PLUGINS=(discord telegram dashboard)
ALL_PLUGINS=(nen tracker scheduler discord telegram dashboard)

plugin_desc() {
  case "$1" in
    orchestrator) echo "Multi-agent mission execution" ;;
    decomposer)   echo "Mission decomposition (knowledge-only)" ;;
    nen)          echo "Self-improvement (Shu, Gyo, Ko, En, Ryu)" ;;
    tracker)      echo "Local issue tracker (Rust)" ;;
    scheduler)    echo "Cron jobs + dispatch loop" ;;
    discord)      echo "Channel notifications + voice messages" ;;
    telegram)     echo "Channel notifications + voice messages" ;;
    dashboard)    echo "Desktop dashboard (requires Wails + Node.js)" ;;
    *) echo "" ;;
  esac
}

plugin_required_prereqs() {
  case "$1" in
    tracker)   echo "cargo" ;;
    dashboard) echo "wails node" ;;
    *) echo "" ;;
  esac
}

plugin_configure_cmd() {
  case "$1" in
    discord)    echo "discord configure|Discord bot token" ;;
    telegram)   echo "telegram configure|Telegram bot token" ;;
    scheduler)  echo "scheduler init|Initialize job database" ;;
    *) echo "" ;;
  esac
}

plugin_category() {
  local p="$1"
  case "$p" in
    nen|tracker|scheduler) echo "core"; return ;;
  esac
  local c
  for c in "${OPTIONAL_PLUGINS[@]}"; do [[ "$c" == "$p" ]] && echo "optional" && return; done
  echo "unknown"
}

# ── Output Helpers ───────────────────────────────────────────────────────────

if [[ -t 1 ]]; then
  RED='\033[0;31m'; YELLOW='\033[1;33m'; GREEN='\033[0;32m'
  CYAN='\033[0;36m'; BOLD='\033[1m'; DIM='\033[2m'; RESET='\033[0m'
else
  RED=''; YELLOW=''; GREEN=''; CYAN=''; BOLD=''; DIM=''; RESET=''
fi

step()   { [[ "$JSON_OUTPUT" -eq 1 ]] && return; echo -e "${CYAN}==>${RESET} ${BOLD}$*${RESET}"; }
ok()     { [[ "$JSON_OUTPUT" -eq 1 ]] && return; echo -e "  ${GREEN}✓${RESET} $*"; }
warn()   { [[ "$JSON_OUTPUT" -eq 1 ]] && return; echo -e "  ${YELLOW}!${RESET} $*"; }
fail()   { [[ "$JSON_OUTPUT" -eq 1 ]] && return; echo -e "  ${RED}✗${RESET} $*" >&2; }
die()    { echo -e "  ${RED}✗${RESET} $*" >&2; exit 1; }
info()   { [[ "$JSON_OUTPUT" -eq 1 ]] && return; echo -e "  $*"; }

header() {
  [[ "$JSON_OUTPUT" -eq 1 ]] && return
  echo ""
  echo -e "${BOLD}$(printf '━%.0s' {1..60})${RESET}"
  echo -e "${BOLD}  $*${RESET}"
  echo -e "${BOLD}$(printf '━%.0s' {1..60})${RESET}"
  echo ""
}

# ── Version Helpers ──────────────────────────────────────────────────────────

version_gte() { [[ "$1" -ge "$2" ]]; }
parse_major() { echo "$1" | sed 's/^[^0-9]*//' | cut -d. -f1; }

parse_go_version_int() {
  local ver maj min
  ver=$(echo "$1" | grep -oE '[0-9]+\.[0-9]+' | head -1)
  maj=$(echo "$ver" | cut -d. -f1)
  min=$(echo "$ver" | cut -d. -f2)
  echo $(( maj * 100 + min ))
}

# ── Utility ──────────────────────────────────────────────────────────────────

contains() {
  local needle="$1"; shift
  [[ $# -eq 0 ]] && return 1
  local item
  for item in "$@"; do [[ "$item" == "$needle" ]] && return 0; done
  return 1
}

read_plugin_json() {
  local plugin_dir="$1" field="$2"
  local pjson="$plugin_dir/plugin.json"
  [[ -f "$pjson" ]] || return 1
  python3 -c "import json,sys; d=json.load(open('$pjson')); v=d.get('$field',''); print(v)" 2>/dev/null
}

read_plugin_version() {
  local name="$1" dir=""
  [[ -d "$REPO_ROOT/plugins/$name" ]] && dir="$REPO_ROOT/plugins/$name"
  [[ -d "$REPO_ROOT/skills/$name" ]] && dir="$REPO_ROOT/skills/$name"
  if [[ -n "$dir" ]]; then
    read_plugin_json "$dir" "version" 2>/dev/null || echo "unknown"
  else
    echo "unknown"
  fi
}

format_item() { printf '%-16s %s' "$1" "$(plugin_desc "$1")"; }

# ── State ────────────────────────────────────────────────────────────────────

MODE=""
SELECTED_LIST=""
NO_INTERACTIVE=0
SKIP_DASHBOARD=0
DRY_RUN=0
REPAIR=0
JSON_OUTPUT=0

SELECTED_PLUGINS=()
SKIPPED_PLUGINS=()
INSTALLED_PLUGINS=()
FAILED_PLUGINS=()

# Prerequisites tracking for JSON output
PREREQ_NAMES=()
PREREQ_STATUS=()
PREREQ_VERSION=()
PREREQ_AFFECTS=()

USE_GUM=0
command -v gum >/dev/null 2>&1 && USE_GUM=1

IS_TTY=0
if [[ -t 0 ]] && [[ -t 1 ]]; then
  IS_TTY=1
fi

# ── Flag Parsing ─────────────────────────────────────────────────────────────

parse_flags() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --all)
        [[ -n "$MODE" ]] && die "Cannot combine --all with --$MODE"
        MODE="all" ;;
      --core)
        [[ -n "$MODE" ]] && die "Cannot combine --core with --$MODE"
        MODE="core" ;;
      --plugins)
        [[ -n "$MODE" ]] && die "Cannot combine --plugins with --$MODE"
        MODE="plugins"; shift
        [[ $# -gt 0 ]] || die "--plugins requires a comma-separated list"
        SELECTED_LIST="$1" ;;
      --no-interactive) NO_INTERACTIVE=1 ;;
      --skip-dashboard) SKIP_DASHBOARD=1 ;;
      --dry-run)        DRY_RUN=1 ;;
      --repair)         REPAIR=1 ;;
      --json)           JSON_OUTPUT=1; NO_INTERACTIVE=1 ;;
      --help|-h)        usage; exit 0 ;;
      *) die "Unknown flag: $1 (see --help)" ;;
    esac
    shift
  done
  # Force non-interactive without TTY
  if [[ "$IS_TTY" -eq 0 ]]; then
    NO_INTERACTIVE=1
  fi
}

usage() {
  cat <<'EOF'
Usage: scripts/install.sh [flags]

Selection flags (mutually exclusive):
  --all              Install everything (core + recommended + optional)
  --core             Install core plugins only
  --plugins LIST     Comma-separated plugin names (e.g. --plugins gmail,scout,ynab)

Behavior flags:
  --no-interactive   Skip all prompts, install core plugins only
  --skip-dashboard   Skip dashboard build (even with --all)
  --dry-run          Show what would be installed without doing it
  --repair           Re-run prerequisite checks and rebuild broken plugins
  --json             Machine-readable output (implies --no-interactive)
  --help, -h         Show this help

Examples:
  scripts/install.sh                           # Interactive mode
  scripts/install.sh --all                     # Everything, still prompts for prereqs
  scripts/install.sh --core                    # Minimal install, no prompts
  scripts/install.sh --all --no-interactive    # CI: everything, no prompts
  scripts/install.sh --plugins gmail,ynab      # Only core + selected plugins
  scripts/install.sh --repair                  # Fix broken install
EOF
}

# ── Selection ────────────────────────────────────────────────────────────────

select_plugins() {
  case "$MODE" in
    all)
      SELECTED_PLUGINS=("${CORE_PLUGINS[@]}" "${RECOMMENDED_PLUGINS[@]}" "${OPTIONAL_PLUGINS[@]}")
      ;;
    core)
      SELECTED_PLUGINS=("${CORE_PLUGINS[@]}")
      ;;
    plugins)
      SELECTED_PLUGINS=("${CORE_PLUGINS[@]}")
      IFS=',' read -ra requested <<< "$SELECTED_LIST"
      local p
      for p in "${requested[@]}"; do
        p=$(echo "$p" | tr -d ' ')
        contains "$p" "${ALL_PLUGINS[@]}" || die "Unknown plugin: $p"
        contains "$p" "${SELECTED_PLUGINS[@]}" || SELECTED_PLUGINS+=("$p")
      done
      ;;
    "")
      if [[ "$NO_INTERACTIVE" -eq 1 ]]; then
        SELECTED_PLUGINS=("${CORE_PLUGINS[@]}")
      else
        interactive_select
      fi
      ;;
  esac

  # Apply --skip-dashboard
  if [[ "$SKIP_DASHBOARD" -eq 1 ]]; then
    local filtered=()
    local p
    for p in "${SELECTED_PLUGINS[@]}"; do
      [[ "$p" != "dashboard" ]] && filtered+=("$p")
    done
    SELECTED_PLUGINS=("${filtered[@]}")
  fi
}

interactive_select() {
  header "Nanika Installer"

  local choice
  if [[ "$USE_GUM" -eq 1 ]]; then
    choice=$(gum choose --cursor="● " \
      "Core          orchestrator + nen + tracker + scheduler" \
      "Everything    Core + discord, telegram, dashboard (requires Rust, Wails)" \
      "Custom        Choose individual plugins")
    choice=$(echo "$choice" | awk '{print $1}' | tr '[:upper:]' '[:lower:]')
  else
    echo "  What would you like to install?"
    echo ""
    echo "  1) Core           orchestrator + nen + tracker + scheduler"
    echo "  2) Everything     Core + discord, telegram, dashboard"
    echo "  3) Custom         Choose individual plugins"
    echo ""
    read -rp "  Choice [1]: " choice
    case "${choice:-1}" in
      1) choice="core" ;;
      2) choice="everything" ;;
      3) choice="custom" ;;
      *) die "Invalid choice: $choice" ;;
    esac
  fi

  case "$choice" in
    core*)
      SELECTED_PLUGINS=("${CORE_PLUGINS[@]}")
      ;;
    everything*)
      SELECTED_PLUGINS=("${CORE_PLUGINS[@]}" "${RECOMMENDED_PLUGINS[@]}" "${OPTIONAL_PLUGINS[@]}")
      ;;
    custom*)
      interactive_custom_select
      ;;
  esac
}

interactive_custom_select() {
  SELECTED_PLUGINS=("${CORE_PLUGINS[@]}")

  local selectable=("${RECOMMENDED_PLUGINS[@]}" "${OPTIONAL_PLUGINS[@]}")

  if [[ "$USE_GUM" -eq 1 ]]; then
    echo ""
    echo "  Core (always installed): orchestrator, decomposer, nen"
    echo ""

    # Build item list and pre-select recommended
    local items=() selected_args=() p
    for p in "${selectable[@]}"; do
      items+=("$(format_item "$p")")
    done
    for p in "${RECOMMENDED_PLUGINS[@]}"; do
      selected_args+=(--selected "$(format_item "$p")")
    done

    local chosen
    chosen=$(printf '%s\n' "${items[@]}" | gum choose --no-limit \
      --cursor-prefix="[ ] " --selected-prefix="[x] " --unselected-prefix="[ ] " \
      "${selected_args[@]}" \
      --header="Select plugins (Space to toggle, Enter to confirm):") || true

    while IFS= read -r line; do
      local name
      name=$(echo "$line" | awk '{print $1}')
      [[ -n "$name" ]] && SELECTED_PLUGINS+=("$name")
    done <<< "$chosen"
  else
    echo ""
    echo "  Core (always installed): orchestrator, decomposer, nen"
    echo ""
    echo "  Recommended:"
    local idx=1 p
    for p in "${RECOMMENDED_PLUGINS[@]}"; do
      printf "    %2d) %-16s %s\n" "$idx" "$p" "$(plugin_desc "$p")"
      idx=$((idx + 1))
    done
    echo ""
    echo "  Optional:"
    for p in "${OPTIONAL_PLUGINS[@]}"; do
      printf "    %2d) %-16s %s\n" "$idx" "$p" "$(plugin_desc "$p")"
      idx=$((idx + 1))
    done
    echo ""
    echo "  Default (recommended): ${RECOMMENDED_PLUGINS[*]}"
    echo ""
    read -rp "  Enter names or numbers (comma-separated), 'a' for all, Enter for default: " input

    if [[ -z "$input" ]]; then
      SELECTED_PLUGINS+=("${RECOMMENDED_PLUGINS[@]}")
    elif [[ "$input" == "a" || "$input" == "all" ]]; then
      SELECTED_PLUGINS+=("${selectable[@]}")
    else
      IFS=',' read -ra picks <<< "$input"
      local pick
      for pick in "${picks[@]}"; do
        pick=$(echo "$pick" | tr -d ' ')
        if [[ "$pick" =~ ^[0-9]+$ ]]; then
          local pidx=$((pick - 1))
          if [[ $pidx -ge 0 && $pidx -lt ${#selectable[@]} ]]; then
            SELECTED_PLUGINS+=("${selectable[$pidx]}")
          else
            warn "Invalid number: $pick (skipping)"
          fi
        elif contains "$pick" "${selectable[@]}"; then
          SELECTED_PLUGINS+=("$pick")
        else
          warn "Unknown plugin: $pick (skipping)"
        fi
      done
    fi
  fi
}

# ── Prerequisite Checking ────────────────────────────────────────────────────

record_prereq() {
  PREREQ_NAMES+=("$1")
  PREREQ_STATUS+=("$2")
  PREREQ_VERSION+=("$3")
  PREREQ_AFFECTS+=("$4")
}

remove_from_selected() {
  local target="$1" new=() p
  for p in "${SELECTED_PLUGINS[@]}"; do
    [[ "$p" != "$target" ]] && new+=("$p")
  done
  SELECTED_PLUGINS=("${new[@]}")
}

check_prerequisites() {
  step "Checking prerequisites"
  [[ "$JSON_OUTPUT" -eq 0 ]] && echo "" && info "Required:"

  local errors=0

  # ── Required: go >= 1.25 ──
  if command -v go >/dev/null 2>&1; then
    local go_raw go_int
    go_raw=$(go version)
    go_int=$(parse_go_version_int "$go_raw")
    if version_gte "$go_int" 125; then
      ok "go        $go_raw"
      record_prereq "go" "ok" "$go_raw" ""
    else
      fail "go        $go_raw (need >= 1.25)"
      record_prereq "go" "old" "$go_raw" ""
      errors=$((errors + 1))
    fi
  else
    fail "go        not found — https://go.dev/dl/"
    record_prereq "go" "missing" "" ""
    errors=$((errors + 1))
  fi

  # ── Required: claude CLI ──
  if command -v claude >/dev/null 2>&1; then
    local claude_ver
    claude_ver=$(claude --version 2>/dev/null | head -1 || echo "present")
    ok "claude    $claude_ver"
    record_prereq "claude" "ok" "$claude_ver" ""
  else
    fail "claude    not found — https://claude.ai/code"
    record_prereq "claude" "missing" "" ""
    errors=$((errors + 1))
  fi

  if [[ "$errors" -gt 0 ]]; then
    echo ""
    if [[ "$JSON_OUTPUT" -eq 1 ]]; then
      emit_json_error "Required prerequisites missing"
    fi
    die "$errors required prerequisite(s) missing. Install them and re-run."
  fi

  # ── Conditionally required ──
  local need_cargo=0 need_wails=0 need_node=0 p
  for p in "${SELECTED_PLUGINS[@]}"; do
    case "$p" in
      tracker)   need_cargo=1 ;;
      dashboard) need_wails=1; need_node=1 ;;
    esac
  done

  local conditional_missing=()
  local has_conditional=$(( need_cargo + need_wails + need_node ))

  if [[ "$has_conditional" -gt 0 ]]; then
    [[ "$JSON_OUTPUT" -eq 0 ]] && echo "" && info "For selected plugins:"
  fi

  if [[ "$need_cargo" -eq 1 ]]; then
    if command -v cargo >/dev/null 2>&1; then
      local cargo_ver
      cargo_ver=$(cargo --version 2>/dev/null)
      ok "cargo     $cargo_ver"
      record_prereq "cargo" "ok" "$cargo_ver" "tracker"
    else
      fail "cargo     not found (needed for tracker)"
      info "          Install: curl https://sh.rustup.rs -sSf | sh"
      record_prereq "cargo" "missing" "" "tracker"
      conditional_missing+=("cargo:tracker")
    fi
  fi

  if [[ "$need_wails" -eq 1 ]]; then
    if command -v wails >/dev/null 2>&1; then
      local wails_ver
      wails_ver=$(wails version 2>/dev/null || echo "present")
      ok "wails     $wails_ver"
      record_prereq "wails" "ok" "$wails_ver" "dashboard"
    else
      fail "wails     not found (needed for dashboard)"
      info "          Install: go install github.com/wailsapp/wails/v2/cmd/wails@latest"
      record_prereq "wails" "missing" "" "dashboard"
      conditional_missing+=("wails:dashboard")
    fi
  fi

  if [[ "$need_node" -eq 1 ]]; then
    if command -v node >/dev/null 2>&1; then
      local node_raw node_major
      node_raw=$(node --version)
      node_major=$(parse_major "$node_raw")
      if version_gte "$node_major" 22; then
        ok "node      $node_raw"
        record_prereq "node" "ok" "$node_raw" "dashboard"
      else
        fail "node      $node_raw (need >= 22)"
        record_prereq "node" "old" "$node_raw" "dashboard"
        conditional_missing+=("node:dashboard")
      fi
    else
      fail "node      not found (needed for dashboard)"
      record_prereq "node" "missing" "" "dashboard"
      conditional_missing+=("node:dashboard")
    fi
  fi

  # ── Optional ──
  [[ "$JSON_OUTPUT" -eq 0 ]] && echo "" && info "Optional:"

  if command -v gemini >/dev/null 2>&1; then
    ok "gemini    $(gemini --version 2>/dev/null | head -1 || echo 'present')"
    record_prereq "gemini" "ok" "" ""
  else
    warn "gemini    not found (orchestrator can still work without it)"
    record_prereq "gemini" "missing" "" ""
  fi

  if contains "linkedin" "${SELECTED_PLUGINS[@]}"; then
    local chrome_found=0
    if command -v google-chrome >/dev/null 2>&1 || command -v chromium >/dev/null 2>&1; then
      chrome_found=1
    elif [[ "$(uname)" == "Darwin" ]]; then
      [[ -d "/Applications/Google Chrome.app" ]] || [[ -d "/Applications/Chromium.app" ]] && chrome_found=1
    fi
    if [[ "$chrome_found" -eq 1 ]]; then
      ok "chrome    found"
      record_prereq "chrome" "ok" "" "linkedin"
    else
      warn "chrome    not found (needed for linkedin browser automation)"
      record_prereq "chrome" "missing" "" "linkedin"
    fi
  fi

  # ── Handle conditional missing ──
  if [[ ${#conditional_missing[@]} -gt 0 ]]; then
    echo ""
    # Collect unique affected plugins
    local plugins_to_skip=() entry tool plugin
    for entry in "${conditional_missing[@]}"; do
      plugin="${entry##*:}"
      contains "$plugin" "${plugins_to_skip[@]}" || plugins_to_skip+=("$plugin")
    done

    if [[ "$NO_INTERACTIVE" -eq 1 ]]; then
      for p in "${plugins_to_skip[@]}"; do
        remove_from_selected "$p"
        SKIPPED_PLUGINS+=("$p")
        warn "Skipping $p (missing prerequisite)"
      done
    else
      for entry in "${conditional_missing[@]}"; do
        tool="${entry%%:*}"
        plugin="${entry##*:}"
        # Skip if already handled (e.g. dashboard needs both wails+node)
        if [[ ${#SKIPPED_PLUGINS[@]} -gt 0 ]] && contains "$plugin" "${SKIPPED_PLUGINS[@]}"; then
          continue
        fi

        echo -e "  ${BOLD}$tool${RESET} is needed for ${BOLD}$plugin${RESET} but is not installed."
        echo ""
        local action
        if [[ "$USE_GUM" -eq 1 ]]; then
          action=$(gum choose "Skip $plugin and continue" "Abort and install $tool first")
          [[ "$action" == *"Abort"* ]] && die "Aborting. Install $tool, then re-run."
        else
          read -rp "  Skip $plugin and continue? [Y/n] " action
          [[ "${action:-y}" =~ ^[Nn] ]] && die "Aborting. Install $tool, then re-run."
        fi
        remove_from_selected "$plugin"
        SKIPPED_PLUGINS+=("$plugin")
      done
    fi
  fi
}

# ── Dry Run ──────────────────────────────────────────────────────────────────

print_dry_run() {
  header "Dry run — nothing will be built or installed"

  echo "  Would install (core):"
  local s
  for s in "${CORE_SKILLS[@]}"; do
    printf "    %-18s skills/%-14s → ~/bin/%s\n" "$s" "$s" "$s"
  done
  echo "    decomposer         skills/decomposer     → (knowledge-only)"
  local p
  for p in "${CORE_PLUGINS[@]}"; do
    if [[ "$p" == "nen" ]]; then
      printf "    %-18s plugins/%-14s → ~/bin/{shu,gyo,en,ryu}\n" "$p" "$p"
    fi
  done
  echo ""

  # Non-core selected
  local has_selected=0
  for p in "${SELECTED_PLUGINS[@]}"; do
    if ! contains "$p" "${CORE_PLUGINS[@]}"; then
      if [[ "$has_selected" -eq 0 ]]; then
        echo "  Would install (selected):"
        has_selected=1
      fi
      local binary
      binary=$(read_plugin_json "$REPO_ROOT/plugins/$p" "binary" 2>/dev/null || echo "$p")
      printf "    %-18s plugins/%-14s → ~/bin/%s\n" "$p" "$p" "$binary"
    fi
  done
  [[ "$has_selected" -eq 1 ]] && echo ""

  # Skipped
  if [[ ${#SKIPPED_PLUGINS[@]} -gt 0 ]]; then
    echo "  Would skip:"
    for p in "${SKIPPED_PLUGINS[@]}"; do
      printf "    %-18s missing: %s\n" "$p" "$(plugin_required_prereqs "$p")"
    done
    echo ""
  fi

  # Not selected
  local not_selected=()
  for p in "${ALL_PLUGINS[@]}"; do
    if ! contains "$p" "${SELECTED_PLUGINS[@]}"; then
      [[ ${#SKIPPED_PLUGINS[@]} -gt 0 ]] && contains "$p" "${SKIPPED_PLUGINS[@]}" && continue
      not_selected+=("$p")
    fi
  done
  if [[ ${#not_selected[@]} -gt 0 ]]; then
    echo "  Not selected (use --all or --plugins to include):"
    printf "    %s" "${not_selected[0]}"
    for p in "${not_selected[@]:1}"; do printf ", %s" "$p"; done
    echo ""
    echo ""
  fi

  echo "  Would create directories:"
  echo "    ~/.alluka/{bin,missions,logs,workspaces,worktrees,nen/scanners}"
  echo "    ~/bin"
  echo ""
  echo "  Would run:"
  echo "    make build-skills"
  echo "    Build plugins: ${SELECTED_PLUGINS[*]}"
  echo "    Install binaries to ~/bin/"
  if contains "tracker" "${SELECTED_PLUGINS[@]}"; then
    echo "    tracker init"
  fi
  echo "    orchestrator doctor"
}

# ── Create Directories ───────────────────────────────────────────────────────

create_directories() {
  step "Creating ~/.alluka/ directories"
  mkdir -p \
    ~/.alluka/bin \
    ~/.alluka/missions \
    ~/.alluka/logs \
    ~/.alluka/workspaces \
    ~/.alluka/worktrees \
    ~/.alluka/nen/scanners
  mkdir -p ~/bin
  ok "~/.alluka/ layout ready"
  ok "~/bin ready"
}

# ── Build ────────────────────────────────────────────────────────────────────

build_item() {
  local name="$1" cmd="$2"

  if [[ "$JSON_OUTPUT" -eq 1 ]]; then
    if bash -c "$cmd" >/dev/null 2>&1; then
      INSTALLED_PLUGINS+=("$name")
    else
      FAILED_PLUGINS+=("$name")
    fi
    return
  fi

  if [[ "$USE_GUM" -eq 1 ]]; then
    if gum spin --spinner dot --title "  Building $name..." -- bash -c "$cmd" 2>/dev/null; then
      ok "$name"
      INSTALLED_PLUGINS+=("$name")
    else
      fail "$name  (build failed)"
      FAILED_PLUGINS+=("$name")
    fi
  else
    printf "  Building %-24s" "$name..."
    if bash -c "$cmd" >/dev/null 2>&1; then
      echo -e " ${GREEN}✓${RESET}"
      INSTALLED_PLUGINS+=("$name")
    else
      echo -e " ${RED}✗${RESET}"
      FAILED_PLUGINS+=("$name")
    fi
  fi
}

build_all() {
  local total=$(( ${#CORE_SKILLS[@]} + ${#SELECTED_PLUGINS[@]} ))
  local skipped_count=${#SKIPPED_PLUGINS[@]}

  step "Building ($total components${skipped_count:+, $skipped_count skipped})"

  cd "$REPO_ROOT"

  # Build skills
  local skill
  for skill in "${CORE_SKILLS[@]}"; do
    build_item "$skill" "cd '$REPO_ROOT' && make build-$skill"
  done

  # Build selected plugins
  local plugin
  for plugin in "${SELECTED_PLUGINS[@]}"; do
    local plugin_dir="$REPO_ROOT/plugins/$plugin"
    [[ -d "$plugin_dir" ]] || continue

    local build_cmd
    build_cmd=$(read_plugin_json "$plugin_dir" "build" 2>/dev/null || echo "")
    if [[ -z "$build_cmd" ]]; then
      [[ "$JSON_OUTPUT" -eq 0 ]] && info "  ${DIM}-${RESET} $plugin  (no build field)"
      continue
    fi

    # Go plugins need GOWORK=off; Rust plugins run cargo directly
    local full_cmd
    if echo "$build_cmd" | grep -q "^cargo"; then
      full_cmd="cd '$plugin_dir' && $build_cmd"
    else
      full_cmd="cd '$plugin_dir' && GOWORK=off $build_cmd"
    fi

    build_item "$plugin" "$full_cmd"
  done

  # Show skipped plugins
  local p
  for p in "${SKIPPED_PLUGINS[@]}"; do
    [[ "$JSON_OUTPUT" -eq 0 ]] && info "  ${DIM}-${RESET} $p  (skipped — missing $(plugin_required_prereqs "$p"))"
  done
}

# ── Install ──────────────────────────────────────────────────────────────────

install_all() {
  step "Installing binaries"

  mkdir -p ~/bin
  cd "$REPO_ROOT"

  # Install skills — link from repo bin/ to ~/bin/
  local skill
  for skill in "${CORE_SKILLS[@]}"; do
    if [[ -f "$REPO_ROOT/bin/$skill" ]]; then
      ln -sf "$REPO_ROOT/bin/$skill" ~/bin/"$skill"
      ok "$skill → ~/bin/$skill"
    fi
  done

  # Install plugins via plugin.json install field
  local plugin
  for plugin in "${SELECTED_PLUGINS[@]}"; do
    local plugin_dir="$REPO_ROOT/plugins/$plugin"
    [[ -d "$plugin_dir" ]] || continue

    # Skip plugins that failed to build
    if [[ ${#FAILED_PLUGINS[@]} -gt 0 ]] && contains "$plugin" "${FAILED_PLUGINS[@]}"; then
      continue
    fi

    local install_cmd
    install_cmd=$(read_plugin_json "$plugin_dir" "install" 2>/dev/null || echo "")
    [[ -z "$install_cmd" ]] && continue

    if (cd "$plugin_dir" && bash -c "$install_cmd") >/dev/null 2>&1; then
      ok "$plugin"
    else
      fail "$plugin  (install failed)"
    fi
  done

  export PATH="$HOME/bin:$PATH"
}

# ── launchd Setup (macOS only) ───────────────────────────────────────────────

# Writes the orchestrator-daemon and nen-daemon launchd plist files from the
# templates in scripts/launchd/, loads them via launchctl bootstrap (or
# launchctl load on macOS < 10.11), then verifies both services are running.
setup_launchd() {
  [[ "$(uname)" == "Darwin" ]] || return 0

  step "Setting up launchd agents"

  local launch_agents="$HOME/Library/LaunchAgents"
  mkdir -p "$launch_agents"

  local template_dir="$REPO_ROOT/scripts/launchd"
  local daemons=(orchestrator-daemon nen-daemon)
  local labels=()
  local plists=()

  for d in "${daemons[@]}"; do
    local label="com.nanika.$d"
    local src="$template_dir/$label.plist"
    local dst="$launch_agents/$label.plist"

    [[ -f "$src" ]] || { warn "Template not found: $src (skipping $d)"; continue; }

    # Substitute __HOME__ with the real home path (no ~, no env vars — launchd needs absolute paths)
    sed "s|__HOME__|$HOME|g" "$src" > "$dst"

    labels+=("$label")
    plists+=("$dst")
    [[ "$JSON_OUTPUT" -eq 0 ]] && ok "wrote $dst"
  done

  [[ ${#labels[@]} -eq 0 ]] && return 0

  # Detect whether to use bootstrap (macOS 10.11+) or the legacy load command.
  local macos_major macos_minor use_bootstrap=0
  macos_major=$(sw_vers -productVersion 2>/dev/null | cut -d. -f1)
  macos_minor=$(sw_vers -productVersion 2>/dev/null | cut -d. -f2)
  if [[ "$macos_major" -ge 11 ]] || { [[ "$macos_major" -eq 10 ]] && [[ "$macos_minor" -ge 11 ]]; }; then
    use_bootstrap=1
  fi

  local uid
  uid=$(id -u)

  local i
  for i in "${!labels[@]}"; do
    local label="${labels[$i]}"
    local plist="${plists[$i]}"

    # Unload any existing version first so re-install is idempotent.
    if [[ "$use_bootstrap" -eq 1 ]]; then
      launchctl bootout "gui/$uid/$label" 2>/dev/null || true
      if launchctl bootstrap "gui/$uid" "$plist" 2>/dev/null; then
        [[ "$JSON_OUTPUT" -eq 0 ]] && ok "bootstrapped $label"
      else
        warn "launchctl bootstrap failed for $label — try: launchctl bootstrap gui/$uid $plist"
      fi
    else
      launchctl unload -w "$plist" 2>/dev/null || true
      if launchctl load -w "$plist" 2>/dev/null; then
        [[ "$JSON_OUTPUT" -eq 0 ]] && ok "loaded $label"
      else
        warn "launchctl load failed for $label — try: launchctl load -w $plist"
      fi
    fi
  done

  # Verify both agents appear in the service list.
  echo ""
  [[ "$JSON_OUTPUT" -eq 0 ]] && info "Verifying services..."
  local all_ok=1
  for label in "${labels[@]}"; do
    if launchctl list "$label" >/dev/null 2>&1; then
      local pid_field
      pid_field=$(launchctl list "$label" 2>/dev/null | grep '"PID"' | grep -oE '[0-9]+' || echo "")
      if [[ -n "$pid_field" ]]; then
        [[ "$JSON_OUTPUT" -eq 0 ]] && ok "$label  running (PID $pid_field)"
      else
        [[ "$JSON_OUTPUT" -eq 0 ]] && ok "$label  registered (will start shortly)"
      fi
    else
      warn "$label  not found in launchctl list — check: launchctl list $label"
      all_ok=0
    fi
  done

  if [[ "$all_ok" -eq 1 ]]; then
    echo ""
    [[ "$JSON_OUTPUT" -eq 0 ]] && info "  Logs: ~/.alluka/logs/orchestrator-daemon.log"
    [[ "$JSON_OUTPUT" -eq 0 ]] && info "       ~/.alluka/logs/nen-daemon.log"
    [[ "$JSON_OUTPUT" -eq 0 ]] && info "  Uninstall: make uninstall  (or scripts/install.sh --uninstall)"
  fi
}

# ── Post-Install ─────────────────────────────────────────────────────────────

post_install() {
  # Tracker init (idempotent)
  if contains "tracker" "${SELECTED_PLUGINS[@]}" && \
     [[ ${#FAILED_PLUGINS[@]} -eq 0 || ! $(contains "tracker" "${FAILED_PLUGINS[@]}" && echo yes) ]]; then
    step "Initializing tracker"
    if command -v tracker >/dev/null 2>&1; then
      if tracker init 2>/dev/null; then
        ok "tracker init OK"
      else
        warn "tracker init returned non-zero (may already be initialized)"
      fi
    else
      warn "tracker not in PATH — run manually: tracker init"
    fi
  fi

  # Scheduler init (idempotent)
  if contains "scheduler" "${SELECTED_PLUGINS[@]}" && \
     command -v scheduler >/dev/null 2>&1; then
    step "Initializing scheduler"
    if scheduler init 2>/dev/null; then
      ok "scheduler init OK"
    else
      warn "scheduler init returned non-zero (may already be initialized)"
    fi
  fi

  # Run doctor on each installed plugin
  step "Running health checks"
  local plugin binary
  for plugin in "${INSTALLED_PLUGINS[@]}"; do
    binary=$(read_plugin_json "$REPO_ROOT/plugins/$plugin" "binary" 2>/dev/null || echo "")
    [[ -z "$binary" ]] && continue
    if command -v "$binary" >/dev/null 2>&1; then
      if "$binary" doctor --json >/dev/null 2>&1; then
        ok "$plugin    healthy"
      else
        warn "$plugin    needs configuration"
      fi
    fi
  done

  # Orchestrator doctor
  if command -v orchestrator >/dev/null 2>&1; then
    if orchestrator doctor 2>/dev/null; then
      ok "orchestrator    healthy"
    else
      warn "orchestrator    reported issues"
    fi
  else
    warn "orchestrator not in PATH — ensure ~/bin is in your PATH"
  fi

  # Post-install guidance
  header "Setup Complete"

  info "  Installed:"
  for plugin in "${INSTALLED_PLUGINS[@]}"; do
    info "    ${GREEN}✓${RESET} $(format_item "$plugin")"
  done
  echo ""

  # Show configuration needed
  local needs_config=0
  for plugin in "${INSTALLED_PLUGINS[@]}"; do
    local cfg
    cfg=$(plugin_configure_cmd "$plugin")
    if [[ -n "$cfg" ]]; then
      if [[ "$needs_config" -eq 0 ]]; then
        info "  Configure (optional):"
        needs_config=1
      fi
      local cmd desc
      cmd=$(echo "$cfg" | cut -d'|' -f1)
      desc=$(echo "$cfg" | cut -d'|' -f2)
      info "    ${YELLOW}→${RESET} $cmd  ${DIM}# $desc${RESET}"
    fi
  done
  [[ "$needs_config" -eq 1 ]] && echo ""

  # Show what's available but not installed
  local not_installed=()
  for plugin in "${ALL_PLUGINS[@]}"; do
    contains "$plugin" "${INSTALLED_PLUGINS[@]}" || not_installed+=("$plugin")
  done
  if [[ ${#not_installed[@]} -gt 0 ]]; then
    info "  Add more plugins later:"
    info "    ${DIM}scripts/install.sh --plugins ${not_installed[*]// /,}${RESET}"
    echo ""
  fi

  # Skills hint
  info "  Make your workers smarter — install Claude Code skills:"
  info "    ${DIM}Browse https://skill.sh for domain skills (Go, security, React, etc.)${RESET}"
  info "    ${DIM}Workers automatically use installed skills during missions.${RESET}"
  echo ""

  # Get started
  info "  Get started:"
  info "    ${CYAN}cd nanika && claude${RESET}"
  info "    ${DIM}\"research golang error handling best practices\"${RESET}"
  echo ""
  info "  After adding plugins or skills, update the routing index:"
  info "    ${CYAN}scripts/generate-agents-md.sh${RESET}"
}

# ── Repair Mode ──────────────────────────────────────────────────────────────

run_repair() {
  step "Checking installed plugins"

  local broken=() healthy=()

  # Check skills
  local skill
  for skill in "${CORE_SKILLS[@]}"; do
    if [[ -f ~/bin/"$skill" ]]; then
      # Check if source is newer
      local skill_dir="$REPO_ROOT/skills/$skill"
      if [[ -d "$skill_dir" ]] && find "$skill_dir" -name "*.go" -newer ~/bin/"$skill" 2>/dev/null | grep -q .; then
        warn "$skill    ~/bin/$skill (outdated — source newer than binary)"
        broken+=("skill:$skill")
      else
        ok "$skill    ~/bin/$skill"
        healthy+=("$skill")
      fi
    else
      fail "$skill    binary missing"
      broken+=("skill:$skill")
    fi
  done

  # Check plugins
  local plugin_dir name binary
  for plugin_dir in "$REPO_ROOT"/plugins/*/; do
    name=$(basename "$plugin_dir")
    [[ -f "$plugin_dir/plugin.json" ]] || continue

    binary=$(read_plugin_json "$plugin_dir" "binary" 2>/dev/null || echo "")
    [[ -z "$binary" ]] && continue

    if [[ -f ~/bin/"$binary" ]]; then
      local outdated=0
      local build_cmd
      build_cmd=$(read_plugin_json "$plugin_dir" "build" 2>/dev/null || echo "")
      if echo "$build_cmd" | grep -q "^cargo"; then
        [[ -d "$plugin_dir/src" ]] && \
          find "$plugin_dir/src" -name "*.rs" -newer ~/bin/"$binary" 2>/dev/null | grep -q . && outdated=1
      else
        find "$plugin_dir" -name "*.go" -newer ~/bin/"$binary" 2>/dev/null | grep -q . && outdated=1
      fi

      if [[ "$outdated" -eq 1 ]]; then
        warn "$name    ~/bin/$binary (outdated — source newer than binary)"
        broken+=("plugin:$name")
      else
        ok "$name    ~/bin/$binary"
        healthy+=("$name")
      fi
    elif [[ -d "$plugin_dir/bin" ]] || [[ -d "$plugin_dir/target" ]]; then
      # Had build artifacts but binary missing from ~/bin/
      fail "$name    ~/bin/$binary (binary missing)"
      broken+=("plugin:$name")
    fi
  done

  if [[ ${#broken[@]} -eq 0 ]]; then
    echo ""
    ok "All plugins healthy — nothing to repair"
    return 0
  fi

  echo ""
  step "Rebuilding ${#broken[@]} component(s)"
  cd "$REPO_ROOT"

  local entry type bname
  for entry in "${broken[@]}"; do
    type="${entry%%:*}"
    bname="${entry##*:}"

    if [[ "$type" == "skill" ]]; then
      build_item "$bname" "cd '$REPO_ROOT' && make build-$bname"
      [[ -f "$REPO_ROOT/bin/$bname" ]] && ln -sf "$REPO_ROOT/bin/$bname" ~/bin/"$bname"
    else
      local pdir="$REPO_ROOT/plugins/$bname"
      local build_cmd
      build_cmd=$(read_plugin_json "$pdir" "build" 2>/dev/null || echo "")
      [[ -z "$build_cmd" ]] && continue

      # Check prereqs before rebuilding
      local prereqs
      prereqs=$(plugin_required_prereqs "$bname")
      local prereq_ok=1
      local req
      for req in $prereqs; do
        if ! command -v "$req" >/dev/null 2>&1; then
          warn "Cannot rebuild $bname: $req not found"
          prereq_ok=0
        fi
      done
      [[ "$prereq_ok" -eq 0 ]] && continue

      local full_cmd
      if echo "$build_cmd" | grep -q "^cargo"; then
        full_cmd="cd '$pdir' && $build_cmd"
      else
        full_cmd="cd '$pdir' && GOWORK=off $build_cmd"
      fi
      build_item "$bname" "$full_cmd"

      # Re-link
      local install_cmd
      install_cmd=$(read_plugin_json "$pdir" "install" 2>/dev/null || echo "")
      [[ -n "$install_cmd" ]] && (cd "$pdir" && bash -c "$install_cmd") >/dev/null 2>&1
    fi
  done

  echo ""
  step "Running orchestrator doctor"
  if command -v orchestrator >/dev/null 2>&1; then
    if orchestrator doctor 2>/dev/null; then
      ok "All checks passed"
    else
      warn "Some checks failed"
    fi
  fi

  echo ""
  local repaired=""
  for entry in "${broken[@]}"; do
    bname="${entry##*:}"
    [[ -n "$repaired" ]] && repaired+=", "
    repaired+="$bname"
  done
  echo "  Repaired: $repaired"
}

# ── JSON Output ──────────────────────────────────────────────────────────────

emit_json() {
  local success="true"
  [[ ${#FAILED_PLUGINS[@]} -gt 0 ]] && success="false"

  local installed_json="[" first=1 p
  for p in "${INSTALLED_PLUGINS[@]}"; do
    [[ "$first" -eq 0 ]] && installed_json+=","
    first=0
    local ver cat
    ver=$(read_plugin_version "$p")
    cat=$(plugin_category "$p")
    [[ "$cat" == "unknown" ]] && cat="core"
    installed_json+="{\"name\":\"$p\",\"version\":\"$ver\",\"category\":\"$cat\"}"
  done
  installed_json+="]"

  local skipped_json="["
  first=1
  for p in "${SKIPPED_PLUGINS[@]}"; do
    [[ "$first" -eq 0 ]] && skipped_json+=","
    first=0
    skipped_json+="{\"name\":\"$p\",\"reason\":\"missing prerequisite: $(plugin_required_prereqs "$p")\"}"
  done
  skipped_json+="]"

  local not_selected_json="["
  first=1
  for p in "${ALL_PLUGINS[@]}"; do
    if ! contains "$p" "${SELECTED_PLUGINS[@]}"; then
      [[ ${#SKIPPED_PLUGINS[@]} -gt 0 ]] && contains "$p" "${SKIPPED_PLUGINS[@]}" && continue
      [[ "$first" -eq 0 ]] && not_selected_json+=","
      first=0
      not_selected_json+="\"$p\""
    fi
  done
  not_selected_json+="]"

  local prereqs_json="{"
  first=1
  local i
  for i in "${!PREREQ_NAMES[@]}"; do
    [[ "$first" -eq 0 ]] && prereqs_json+=","
    first=0
    local name="${PREREQ_NAMES[$i]}" status="${PREREQ_STATUS[$i]}"
    local version="${PREREQ_VERSION[$i]}" affects="${PREREQ_AFFECTS[$i]}"
    prereqs_json+="\"$name\":{\"status\":\"$status\""
    [[ -n "$version" ]] && prereqs_json+=",\"version\":\"$version\""
    [[ -n "$affects" ]] && prereqs_json+=",\"affects\":[\"$affects\"]"
    prereqs_json+="}"
  done
  prereqs_json+="}"

  local next_steps_json="["
  first=1
  for p in "${INSTALLED_PLUGINS[@]}"; do
    local cfg
    cfg=$(plugin_configure_cmd "$p")
    if [[ -n "$cfg" ]]; then
      [[ "$first" -eq 0 ]] && next_steps_json+=","
      first=0
      next_steps_json+="\"${cfg%%|*}\""
    fi
  done
  next_steps_json+="]"

  cat <<EOF
{
  "success": $success,
  "installed": $installed_json,
  "skipped": $skipped_json,
  "not_selected": $not_selected_json,
  "prerequisites": $prereqs_json,
  "next_steps": $next_steps_json
}
EOF
}

emit_json_error() {
  echo "{\"success\":false,\"error\":\"$1\"}"
}

# ── Summary ──────────────────────────────────────────────────────────────────

print_summary() {
  if [[ "$JSON_OUTPUT" -eq 1 ]]; then
    emit_json
    return
  fi

  header "Nanika installed successfully"

  # Installed
  local count=$(( ${#INSTALLED_PLUGINS[@]} + 1 ))  # +1 for decomposer
  echo "  Installed ($count):"
  local col=0 p
  for p in "${INSTALLED_PLUGINS[@]}"; do
    printf "    ${GREEN}✓${RESET} %-14s" "$p"
    col=$(( (col + 1) % 3 ))
    [[ $col -eq 0 ]] && echo ""
  done
  # Always add decomposer (knowledge-only)
  printf "    ${GREEN}✓${RESET} %-14s" "decomposer"
  col=$(( (col + 1) % 3 ))
  [[ $col -ne 0 ]] && echo ""
  echo ""

  # Failed
  if [[ ${#FAILED_PLUGINS[@]} -gt 0 ]]; then
    echo "  Failed (${#FAILED_PLUGINS[@]}):"
    for p in "${FAILED_PLUGINS[@]}"; do
      echo -e "    ${RED}✗${RESET} $p"
    done
    echo ""
  fi

  # Skipped
  if [[ ${#SKIPPED_PLUGINS[@]} -gt 0 ]]; then
    echo "  Skipped (${#SKIPPED_PLUGINS[@]}):"
    for p in "${SKIPPED_PLUGINS[@]}"; do
      echo "    - $p       missing prerequisite: $(plugin_required_prereqs "$p")"
    done
    echo ""
  fi

  # Not selected
  local not_selected=()
  for p in "${ALL_PLUGINS[@]}"; do
    if ! contains "$p" "${SELECTED_PLUGINS[@]}"; then
      [[ ${#SKIPPED_PLUGINS[@]} -gt 0 ]] && contains "$p" "${SKIPPED_PLUGINS[@]}" && continue
      not_selected+=("$p")
    fi
  done
  if [[ ${#not_selected[@]} -gt 0 ]]; then
    echo "  Not selected (${#not_selected[@]}):"
    col=0
    for p in "${not_selected[@]}"; do
      printf "    - %-14s" "$p"
      col=$(( (col + 1) % 3 ))
      [[ $col -eq 0 ]] && echo ""
    done
    [[ $col -ne 0 ]] && echo ""
    echo ""
  fi

  # Separator
  printf "  "
  printf '─%.0s' {1..56}
  echo ""
  echo ""

  # Next steps — only for installed plugins needing config
  local has_config=0
  for p in discord telegram scheduler; do
    [[ ${#INSTALLED_PLUGINS[@]} -gt 0 ]] && contains "$p" "${INSTALLED_PLUGINS[@]}" && has_config=1
  done

  if [[ "$has_config" -eq 1 ]]; then
    echo "  Next: configure plugins that need API keys or tokens"
    echo ""
    for p in discord telegram scheduler; do
      if [[ ${#INSTALLED_PLUGINS[@]} -gt 0 ]] && contains "$p" "${INSTALLED_PLUGINS[@]}"; then
        local cfg
        cfg=$(plugin_configure_cmd "$p")
        [[ -n "$cfg" ]] && printf "    %-32s %s\n" "${cfg%%|*}" "${cfg##*|}"
      fi
    done
    echo ""
  fi

  echo "  Run 'make doctor' to verify plugin health at any time."
  echo ""
}

# ── Main ─────────────────────────────────────────────────────────────────────

show_banner() {
  [[ "$JSON_OUTPUT" -eq 1 ]] && return
  [[ "$IS_TTY" -eq 0 ]] && return
  echo ""
  echo -e " \033[38;2;217;119;87m █████╗  ██╗   ██╗ ███████╗\033[0m    "
  echo -e " \033[38;2;221;127;92m██╔══██╗ ╚██╗ ██╔╝ ██╔════╝\033[0m    "
  echo -e " \033[38;2;225;135;97m███████║  ╚████╔╝  █████╗\033[0m      "
  echo -e " \033[38;2;229;143;102m██╔══██║   ╚██╔╝   ██╔══╝\033[0m      "
  echo -e " \033[38;2;232;149;106m██║  ██║    ██║    ███████╗ ██╗\033[0m"
  echo -e " \033[38;2;232;149;106m╚═╝  ╚═╝    ╚═╝    ╚══════╝ ╚═╝\033[0m"
  echo ""
}

main() {
  parse_flags "$@"
  show_banner

  if [[ "$REPAIR" -eq 1 ]]; then
    run_repair
    exit $?
  fi

  select_plugins
  check_prerequisites

  if [[ "$DRY_RUN" -eq 1 ]]; then
    print_dry_run
    exit 0
  fi

  create_directories
  build_all
  install_all
  setup_launchd
  post_install
  print_summary

  if [[ ${#FAILED_PLUGINS[@]} -gt 0 ]]; then
    exit 2
  fi
  exit 0
}

main "$@"
