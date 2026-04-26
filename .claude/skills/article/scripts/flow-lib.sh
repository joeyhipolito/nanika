#!/usr/bin/env bash
# flow-lib.sh — Shared helpers for flow-generate.sh, flow-gif.sh, flow-video.sh
# Source this file; do not execute directly.
[[ -n "${_FLOW_LIB_LOADED:-}" ]] && return 0
_FLOW_LIB_LOADED=1

# ---------------------------------------------------------------------------
# Config loader
# ---------------------------------------------------------------------------
# flow_load_config <style> [project_key]
# Exports: FLOW_PROJECT_ID, FLOW_VIDEO_PROJECT_ID, FLOW_GIF_*, timeouts, thresholds
flow_load_config() {
  local style="$1"
  local project_key="${2:-flow_project_id}"
  local config="$HOME/.contentkit/styles/$style/config.json"

  if [[ ! -f "$config" ]]; then
    echo "ERROR: Unknown style '$style' — no config at $config" >&2
    echo "Available styles: $(ls "$HOME/.contentkit/styles/" 2>/dev/null | tr '\n' ' ')" >&2
    return 1
  fi
  if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq is required. Install with: brew install jq" >&2
    return 1
  fi

  FLOW_PROJECT_ID=$(jq -r ".${project_key} // \"\"" "$config")
  FLOW_VIDEO_PROJECT_ID=$(jq -r '.flow_video_project_id // ""' "$config")
  FLOW_GIF_SCALE_WIDTH=$(jq -r '.gif_settings.scale_width // 640' "$config")
  FLOW_GIF_FPS=$(jq -r '.gif_settings.fps // 12' "$config")
  FLOW_GIF_SCALE_WIDTH="${FLOW_GIF_SCALE_WIDTH//[!0-9]/}"; FLOW_GIF_SCALE_WIDTH="${FLOW_GIF_SCALE_WIDTH:-640}"
  FLOW_GIF_FPS="${FLOW_GIF_FPS//[!0-9]/}"; FLOW_GIF_FPS="${FLOW_GIF_FPS:-12}"

  # Timeouts and thresholds — optional config keys, with sensible defaults
  FLOW_IMAGE_TIMEOUT=$(jq -r '.timeouts.image_generation_s // 300' "$config")
  FLOW_VIDEO_TIMEOUT=$(jq -r '.timeouts.video_generation_s // 600' "$config")
  FLOW_IMAGE_POLL_INTERVAL=$(jq -r '.timeouts.image_poll_interval_s // 10' "$config")
  FLOW_VIDEO_POLL_INTERVAL=$(jq -r '.timeouts.video_poll_interval_s // 15' "$config")
  FLOW_MIN_IMAGE_BYTES=$(jq -r '.thresholds.min_image_bytes // 1000' "$config")
  FLOW_MIN_VIDEO_BYTES=$(jq -r '.thresholds.min_video_bytes // 10000' "$config")
  FLOW_IMAGE_TIMEOUT="${FLOW_IMAGE_TIMEOUT//[!0-9]/}"; FLOW_IMAGE_TIMEOUT="${FLOW_IMAGE_TIMEOUT:-300}"
  FLOW_VIDEO_TIMEOUT="${FLOW_VIDEO_TIMEOUT//[!0-9]/}"; FLOW_VIDEO_TIMEOUT="${FLOW_VIDEO_TIMEOUT:-600}"
  FLOW_IMAGE_POLL_INTERVAL="${FLOW_IMAGE_POLL_INTERVAL//[!0-9]/}"; FLOW_IMAGE_POLL_INTERVAL="${FLOW_IMAGE_POLL_INTERVAL:-10}"
  FLOW_VIDEO_POLL_INTERVAL="${FLOW_VIDEO_POLL_INTERVAL//[!0-9]/}"; FLOW_VIDEO_POLL_INTERVAL="${FLOW_VIDEO_POLL_INTERVAL:-15}"
  FLOW_MIN_IMAGE_BYTES="${FLOW_MIN_IMAGE_BYTES//[!0-9]/}"; FLOW_MIN_IMAGE_BYTES="${FLOW_MIN_IMAGE_BYTES:-1000}"
  FLOW_MIN_VIDEO_BYTES="${FLOW_MIN_VIDEO_BYTES//[!0-9]/}"; FLOW_MIN_VIDEO_BYTES="${FLOW_MIN_VIDEO_BYTES:-10000}"

  # Chrome debug port — override via config or env var
  local _cfg_port
  _cfg_port=$(jq -r '.chrome_debug_port // ""' "$config" 2>/dev/null || echo "")
  if [[ -n "$_cfg_port" && "$_cfg_port" != "null" ]]; then
    FLOW_CHROME_PORT="$_cfg_port"
  fi

  export FLOW_PROJECT_ID FLOW_VIDEO_PROJECT_ID FLOW_GIF_SCALE_WIDTH FLOW_GIF_FPS
  export FLOW_IMAGE_TIMEOUT FLOW_VIDEO_TIMEOUT FLOW_IMAGE_POLL_INTERVAL FLOW_VIDEO_POLL_INTERVAL
  export FLOW_MIN_IMAGE_BYTES FLOW_MIN_VIDEO_BYTES FLOW_CHROME_PORT
}

# flow_validate_style_config <style> <key>
# Returns 1 if the key is missing, empty, null, or REPLACE_ME.
flow_validate_style_config() {
  local style="$1" key="$2"
  local value
  value=$(jq -r ".${key} // \"\"" "$HOME/.contentkit/styles/$style/config.json" 2>/dev/null || echo "")
  if [[ -z "$value" || "$value" == "null" || "$value" == "REPLACE_ME" ]]; then
    echo "ERROR: Style '$style' missing required config key '$key'" >&2
    return 1
  fi
}

# flow_ensure_project <style> <key>
# If the style config is missing the project key, create a new Flow project
# and write the ID back to the style's config.json. Exports the variable.
flow_ensure_project() {
  local style="$1" key="$2"
  local config="$HOME/.contentkit/styles/$style/config.json"

  # Check if key already has a valid value
  if flow_validate_style_config "$style" "$key" 2>/dev/null; then
    return 0
  fi

  echo "  Style '$style' has no '$key' — creating new Flow project..." >&2
  local project_id
  project_id=$(flow_create_project) || return 1

  # Write back to style config
  local tmp_file
  tmp_file=$(mktemp "${config}.XXXXXX")
  jq --arg id "$project_id" --arg k "$key" '.[$k] = $id' "$config" > "$tmp_file"
  mv "$tmp_file" "$config"

  # Update exported variable
  if [[ "$key" == "flow_project_id" ]]; then
    FLOW_PROJECT_ID="$project_id"
    export FLOW_PROJECT_ID
  elif [[ "$key" == "flow_video_project_id" ]]; then
    FLOW_VIDEO_PROJECT_ID="$project_id"
    export FLOW_VIDEO_PROJECT_ID
  fi

  echo "  Created project $project_id → saved to $config" >&2
}

# ---------------------------------------------------------------------------
# Browser connection
# ---------------------------------------------------------------------------
# FLOW_CHROME_PORT — port of the Chrome debug instance (default 9222).
# Set via config.json key "chrome_debug_port" or env var FLOW_CHROME_PORT.
FLOW_CHROME_PORT="${FLOW_CHROME_PORT:-9222}"

# flow_ensure_browser
# Connects agent-browser to the Chrome instance on FLOW_CHROME_PORT.
# If the port is unreachable, falls back to letting agent-browser launch
# its own headless instance (old behaviour).
_FLOW_BROWSER_CONNECTED=0
flow_ensure_browser() {
  [[ "$_FLOW_BROWSER_CONNECTED" -eq 1 ]] && return 0
  if curl -s --max-time 2 "http://localhost:${FLOW_CHROME_PORT}/json/version" >/dev/null 2>&1; then
    agent-browser connect "$FLOW_CHROME_PORT" >/dev/null 2>&1
    # `connect` only registers session state for the calling process. Each
    # subsequent `agent-browser` invocation is its own process and needs to be
    # told to reuse the running Chrome — otherwise `agent-browser open` spawns
    # a fresh headless instance. Export AGENT_BROWSER_AUTO_CONNECT so every
    # downstream call in this shell auto-discovers the debug port.
    export AGENT_BROWSER_AUTO_CONNECT=1
    _FLOW_BROWSER_CONNECTED=1
  else
    echo "WARNING: Chrome debug port $FLOW_CHROME_PORT unreachable — agent-browser will launch headless" >&2
  fi
}

# ---------------------------------------------------------------------------
# Browser / UI helpers
# ---------------------------------------------------------------------------
find_ref() {
  agent-browser snapshot 2>/dev/null \
    | grep "$1" \
    | grep -o 'ref=e[0-9]*' \
    | head -1 \
    | sed 's/ref=//' || echo ""
}

# dismiss_modals <ready_check>
# Dismisses consent/announcement modals (Next, Continue, I agree, Get started)
# until the ready_check selector is found in the DOM, or max 5 attempts.
# ready_check: JS expression that returns truthy when the main UI is visible.
dismiss_modals() {
  local ready_check="${1:-document.querySelector('div[role=\"textbox\"]')}"
  local _try
  for _try in 1 2 3 4 5; do
    local is_ready
    is_ready=$(agent-browser eval "$ready_check ? 'ready' : 'blocked'" 2>/dev/null | tr -d '"' || echo "blocked")
    [[ "$is_ready" == "ready" ]] && return 0

    agent-browser eval "
      var buttons = document.querySelectorAll('button');
      buttons.forEach(function(b) {
        var t = b.textContent.trim();
        if (t === 'Next' || t === 'Continue' || t === 'I agree' || t === 'Get started') {
          b.click();
        }
      });
    " >/dev/null 2>&1
    sleep 3
  done
}

click_mode_button() {
  local ref _try
  for _try in 1 2 3; do
    ref=$(find_ref 'button.*crop_')
    [[ -n "$ref" ]] && break
    sleep 2
  done
  if [[ -n "$ref" ]]; then
    agent-browser click "@$ref" >/dev/null 2>&1
    sleep 0.5
  else
    echo "    WARNING: mode button not found"
  fi
}

# setup_image_mode — always forces Image + Landscape tabs (flow-generate.sh)
setup_image_mode() {
  echo "==> Setting mode: Image + Landscape"
  click_mode_button
  sleep 0.5

  local snap image_ref landscape_ref
  snap=$(agent-browser snapshot 2>/dev/null)

  image_ref=$(echo "$snap" | grep 'tab "image Image"' | grep -o 'ref=e[0-9]*' | head -1 | sed 's/ref=//' || echo "")
  if [[ -n "$image_ref" ]]; then
    agent-browser click "@$image_ref" >/dev/null 2>&1
    sleep 0.5
    snap=$(agent-browser snapshot 2>/dev/null)
  fi

  landscape_ref=$(echo "$snap" | grep -iE 'landscape|crop_16_9' | grep 'tab' | grep -o 'ref=e[0-9]*' | head -1 | sed 's/ref=//' || echo "")
  if [[ -n "$landscape_ref" ]]; then
    agent-browser click "@$landscape_ref" >/dev/null 2>&1
    sleep 0.3
  fi

  click_mode_button
  sleep 0.3
  echo "    Mode: Image, Landscape, x1"
}

# ensure_image_mode — no-op if already in image mode (flow-gif.sh, flow-video.sh)
ensure_image_mode() {
  local has_swap
  has_swap=$(agent-browser eval "
    Array.from(document.querySelectorAll('button')).some(function(b) {
      return b.textContent.includes('Swap');
    }) ? '1' : '0';
  " 2>/dev/null | tr -d '"' || echo "0")

  if [[ "$has_swap" == "1" ]]; then
    echo "    Switching to Image mode..."
    click_mode_button
    local image_ref
    image_ref=$(find_ref 'tab "image Image"')
    if [[ -n "$image_ref" ]]; then
      agent-browser click "@$image_ref" >/dev/null 2>&1
      sleep 0.5
    else
      echo "    WARNING: Image tab not found"
    fi
    click_mode_button
    echo "    Image mode set"
  else
    echo "    Image mode already active"
  fi
}

activate_video_mode() {
  local has_swap
  has_swap=$(agent-browser eval "
    Array.from(document.querySelectorAll('button')).some(function(b) {
      return b.textContent.includes('Swap');
    }) ? '1' : '0';
  " 2>/dev/null | tr -d '"' || echo "0")

  if [[ "$has_swap" == "1" ]]; then
    echo "    Video mode already active"
    return 0
  fi

  echo "    Activating video mode..."
  click_mode_button

  local video_ref
  video_ref=$(find_ref 'tab "videocam Video"')
  if [[ -n "$video_ref" ]]; then
    agent-browser click "@$video_ref" >/dev/null 2>&1
    sleep 0.5
  else
    echo "    WARNING: Video tab not found"
  fi

  local frames_ref frames_line
  frames_ref=$(find_ref 'tab "crop_free Frames"')
  if [[ -n "$frames_ref" ]]; then
    frames_line=$(agent-browser snapshot 2>/dev/null | grep 'tab "crop_free Frames"' || echo "")
    if ! echo "$frames_line" | grep -q "selected"; then
      agent-browser click "@$frames_ref" >/dev/null 2>&1
      sleep 0.5
    fi
  fi

  click_mode_button
  sleep 0.5

  has_swap=$(agent-browser eval "
    Array.from(document.querySelectorAll('button')).some(function(b) {
      return b.textContent.includes('Swap');
    }) ? '1' : '0';
  " 2>/dev/null | tr -d '"' || echo "0")

  if [[ "$has_swap" == "1" ]]; then
    echo "    Video mode activated"
  else
    echo "    WARNING: could not confirm video mode (Swap button not found)"
  fi
}

# select_aspect_ratio "portrait"|"landscape" — no-op for landscape
select_aspect_ratio() {
  local orientation="$1"
  [[ "$orientation" != "portrait" ]] && return 0

  echo "    Selecting portrait (9:16) aspect ratio..."
  local snap dropdown_ref ar_ref
  snap=$(agent-browser snapshot 2>/dev/null)
  dropdown_ref=$(echo "$snap" | grep -iE 'crop_16_9|crop_9_16' | grep 'button' | grep -o 'ref=e[0-9]*' | head -1 | sed 's/ref=//' || echo "")

  if [[ -z "$dropdown_ref" ]]; then
    echo "    WARNING: aspect ratio dropdown not found — skipping"
    return 0
  fi

  agent-browser click "@$dropdown_ref" >/dev/null 2>&1
  sleep 1

  snap=$(agent-browser snapshot 2>/dev/null)
  ar_ref=$(echo "$snap" | grep -iE 'crop_9_16|portrait' | grep 'tab' | grep -o 'ref=e[0-9]*' | head -1 | sed 's/ref=//' || echo "")

  if [[ -z "$ar_ref" ]]; then
    echo "    WARNING: portrait tab not found — skipping"
    agent-browser click "@$dropdown_ref" >/dev/null 2>&1
    return 0
  fi

  agent-browser click "@$ar_ref" >/dev/null 2>&1
  sleep 1
}

# ---------------------------------------------------------------------------
# Prompt + media helpers
# ---------------------------------------------------------------------------
# submit_image <prompt_file>
# NOTE: Caller must clear contenteditable BEFORE attaching refs.
# This function only focuses, pastes prompt text, and clicks Create.
submit_image() {
  local prompt_file="$1"
  local prompt_text
  prompt_text=$(cat "$prompt_file")

  # Fill prompt via role=textbox (fires proper React input events)
  agent-browser fill "div[role=textbox]" "$prompt_text" >/dev/null 2>&1
  sleep 0.5

  # Click Create via direct DOM — icon buttons use i.google-symbols with text content
  agent-browser eval "
    var icons = document.querySelectorAll('i.google-symbols');
    icons.forEach(function(i) {
      if (i.textContent.trim() === 'arrow_forward') i.closest('button').click();
    });
  " >/dev/null 2>&1
  sleep 2
}

# flow_wait_all_renders <project_id> <uuids_csv> [max_wait_s]
# Polls until all UUIDs in the CSV list reach SUCCESSFUL status.
flow_wait_all_renders() {
  local project_id="$1" uuids_csv="$2"
  local max_wait="${3:-600}"
  local poll_interval="${FLOW_VIDEO_POLL_INTERVAL:-15}"
  local elapsed=0

  while [[ "$elapsed" -lt "$max_wait" ]]; do
    sleep "$poll_interval"
    elapsed=$((elapsed + poll_interval))
    local all_media
    all_media=$(agent-browser eval "
    (async function() {
      var resp = await fetch('/fx/api/trpc/flow.projectInitialData?input=' +
        encodeURIComponent(JSON.stringify({ json: { projectId: '$project_id' } })));
      var data = await resp.json();
      return data.result.data.json.projectContents.media
        .filter(function(m) { return m.video; }).map(function(m) {
          var s = m.mediaMetadata ? m.mediaMetadata.mediaStatus.mediaGenerationStatus : 'UNKNOWN';
          return m.name + ':' + s;
        }).join(',');
    })()
    " 2>/dev/null | tr -d '"') || :

    local pending=0 done_count=0 uuid
    for uuid in $(echo "$uuids_csv" | tr ',' '\n'); do
      [[ -z "$uuid" ]] && continue
      local status
      status=$(echo "$all_media" | tr ',' '\n' | grep "^${uuid}:" | cut -d: -f2 || echo "UNKNOWN")
      status="${status:-UNKNOWN}"
      [[ "$status" == *"SUCCESSFUL"* ]] && done_count=$((done_count + 1)) || pending=$((pending + 1))
    done
    echo "    ${elapsed}s — done: $done_count, pending: $pending"
    [[ "$pending" -eq 0 ]] && return 0
  done
  return 1
}

# download_image <uuid> <outfile>
download_image() {
  local uuid="$1" outfile="$2"
  local min_bytes="${FLOW_MIN_IMAGE_BYTES:-1000}"

  local gcs_url
  gcs_url=$(agent-browser eval "
    fetch('/fx/api/trpc/media.getMediaUrlRedirect?name=$uuid')
      .then(function(r) { return r.url; });
  " 2>/dev/null | tr -d '"')

  if [[ -z "$gcs_url" ]] || echo "$gcs_url" | grep -qE "undefined|null"; then
    echo "      FAIL — no download URL for $uuid"
    return 1
  fi

  curl -sL -o "$outfile" "$gcs_url"
  local size
  size=$(stat -f%z "$outfile" 2>/dev/null || echo "0")
  if [[ "$size" -lt "$min_bytes" ]]; then
    echo "      FAIL — only $size bytes"
    rm -f "$outfile"
    return 1
  fi
  echo "      OK $(basename "$outfile") ($size bytes)"
}

# select_frame <slot_label> <search_name>
# IMPORTANT: agent-browser eval returns empty for if/else blocks — use ternary only.
select_frame() {
  local slot_label="$1" search_name="$2"

  local slot_coords
  slot_coords=$(agent-browser eval "
    var _el = Array.from(document.querySelectorAll('[aria-haspopup=\"dialog\"]')).find(function(e) {
      return e.textContent.trim() === '$slot_label';
    });
    var _r = _el ? _el.getBoundingClientRect() : null;
    _r ? (String(Math.round(_r.left + _r.width/2)) + ',' + String(Math.round(_r.top + _r.height/2))) : '';
  " 2>/dev/null | tr -d '"')
  if [[ -z "$slot_coords" ]]; then
    echo "      WARNING: $slot_label slot not found"
    return 1
  fi
  agent-browser mouse move "${slot_coords%%,*}" "${slot_coords##*,}" >/dev/null 2>&1
  agent-browser mouse down left >/dev/null 2>&1
  agent-browser mouse up left >/dev/null 2>&1
  sleep 1

  local has_dialog
  has_dialog=$(agent-browser eval "document.querySelector('[role=\"dialog\"]') ? '1' : '0';" 2>/dev/null | tr -d '"')
  if [[ "$has_dialog" != "1" ]]; then
    echo "      WARNING: dialog did not open for $slot_label"
    return 1
  fi

  local search_input_ref
  search_input_ref=$(agent-browser snapshot 2>/dev/null | grep 'Search for Assets' | grep -o 'ref=e[0-9]*' | head -1 | sed 's/ref=//' || :)
  if [[ -n "$search_input_ref" ]]; then
    agent-browser fill "@$search_input_ref" "$search_name" >/dev/null 2>&1
  else
    agent-browser eval "var _inp = document.querySelector('[role=\"dialog\"] input'); if (_inp) { _inp.focus(); }" >/dev/null 2>&1
    agent-browser type "$search_name" >/dev/null 2>&1
  fi
  sleep 3

  local img_coords
  img_coords=$(agent-browser eval "
    var _d = document.querySelector('[role=\"dialog\"]');
    var _dr = _d ? _d.getBoundingClientRect() : null;
    var _imgs = _dr ? Array.from(document.querySelectorAll('img')) : [];
    var _match = _imgs.find(function(img) {
      var _ir = img.getBoundingClientRect();
      return img.alt === '$search_name'
        && _ir.left >= _dr.left && _ir.right <= _dr.right
        && _ir.top >= _dr.top && _ir.bottom <= _dr.bottom
        && _ir.width > 20;
    });
    var _mr = _match ? _match.getBoundingClientRect() : null;
    _mr ? (String(Math.round(_mr.left + _mr.width/2)) + ',' + String(Math.round(_mr.top + _mr.height/2))) : '';
  " 2>/dev/null | tr -d '"')
  if [[ -z "$img_coords" ]]; then
    echo "      WARNING: $search_name not found in dialog"
    agent-browser press "Escape" >/dev/null 2>&1 || true
    return 1
  fi
  agent-browser mouse move "${img_coords%%,*}" "${img_coords##*,}" >/dev/null 2>&1
  agent-browser mouse down left >/dev/null 2>&1
  agent-browser mouse up left >/dev/null 2>&1
  sleep 1.5

  local dialog_still_open
  dialog_still_open=$(agent-browser eval "document.querySelector('[role=\"dialog\"]') ? '1' : '0';" 2>/dev/null | tr -d '"' || echo "1")
  if [[ "$dialog_still_open" == "1" ]]; then
    echo "      WARNING: dialog still open — $slot_label frame not set"
    agent-browser press "Escape" >/dev/null 2>&1 || true
    return 1
  fi
  echo "      Selected $slot_label: $search_name"
}

# ---------------------------------------------------------------------------
# Reference image tracking
# ---------------------------------------------------------------------------
# paste_ref_as_ingredient <file_path>
# Pastes image into Flow contenteditable. Calls mark_ref_uploaded if REFS_FILE is set.
paste_ref_as_ingredient() {
  local file_path="$1"
  local name ext_lower
  name=$(basename "$file_path")
  ext_lower=$(echo "${name##*.}" | tr '[:upper:]' '[:lower:]')

  echo "      Pasting: $name"

  if [[ "$ext_lower" == "jpg" || "$ext_lower" == "jpeg" ]]; then
    osascript -e "set the clipboard to (read (POSIX file \"$file_path\") as JPEG picture)" 2>/dev/null || true
  else
    osascript -e "set the clipboard to (read (POSIX file \"$file_path\") as «class PNGf»)" 2>/dev/null || true
  fi

  agent-browser eval "
    var boxes = document.querySelectorAll('div[contenteditable=\"true\"]');
    var box = boxes[boxes.length - 1];
    if (box) { box.click(); box.focus(); }
  " >/dev/null 2>&1
  sleep 0.3
  agent-browser press "Meta+v" >/dev/null 2>&1
  sleep 1.5
  mark_ref_uploaded "$name"
}

declare -A UPLOADED_REFS 2>/dev/null || true

# flow_init_ref_tracking <tool_name> <slug> <project_id>
# Sets up REFS_PERSIST_DIR, REFS_FILE, and loads previously uploaded refs.
flow_init_ref_tracking() {
  local tool="$1" slug="$2" project_id="$3"
  REFS_PERSIST_DIR="$HOME/.contentkit/$tool/$slug/$project_id"
  export REFS_FILE="$REFS_PERSIST_DIR/uploaded-refs.json"
  load_uploaded_refs
}

load_uploaded_refs() {
  if [[ ! -f "$REFS_FILE" ]]; then
    mkdir -p "$REFS_PERSIST_DIR"
    echo '{}' > "$REFS_FILE"
  fi
  UPLOADED_REFS=()
  while IFS= read -r key; do
    UPLOADED_REFS["$key"]=true
  done < <(jq -r 'keys[]' "$REFS_FILE")
}

upload_ref_if_needed() {
  local file_path="$1"
  local name
  name=$(basename "$file_path")
  if [[ "${UPLOADED_REFS[$name]:-}" == "true" ]]; then
    echo "      Already uploaded: $name"
    return 0
  fi
  echo "      Uploading: $name"
  agent-browser upload "input[type='file']" "$file_path" >/dev/null 2>&1
  sleep 3
  mark_ref_uploaded "$name"
}

mark_ref_uploaded() {
  local name="$1"
  UPLOADED_REFS["$name"]=true
  [[ -z "${REFS_FILE:-}" ]] && return 0
  if [[ ! -f "$REFS_FILE" ]]; then
    mkdir -p "$REFS_PERSIST_DIR"
    echo '{}' > "$REFS_FILE"
  fi
  local tmp_file
  tmp_file=$(mktemp /tmp/uploaded-refs-XXXXXX.json)
  jq --arg key "$name" '.[$key] = true' "$REFS_FILE" > "$tmp_file"
  mv "$tmp_file" "$REFS_FILE"
}

# ---------------------------------------------------------------------------
# Reference management — upload once, attach by search
# ---------------------------------------------------------------------------

# flow_attach_or_upload_refs <folder>
# For each ref in the folder:
#   - Not in UPLOADED_REFS → paste (uploads to library + attaches)
#   - In UPLOADED_REFS → search dialog + attach from library
# Tracks uploads so the same ref is never pasted twice across prompts.
flow_attach_or_upload_refs() {
  local folder="$1"
  local attached=0 expected=0 pasted_new=0
  local -a new_ref_names=() all_ref_names=()

  # Pass 1: collect all refs, upload new ones to library
  for ref in "$folder"/mascot.* "$folder"/ref-*.png "$folder"/ref-*.jpg "$folder"/ref-*.jpeg "$folder"/ref-*.webp; do
    [[ -f "$ref" ]] || continue
    expected=$((expected + 1))
    local name
    name=$(basename "$ref")
    all_ref_names+=("$name")

    if [[ "${UPLOADED_REFS[$name]:-}" != "true" ]]; then
      # New ref — open attachment panel, click "Upload image", then upload file.
      # The file input only exists after clicking the upload button.
      echo "      Uploading: $name"

      # Open the + panel if not already open (direct DOM check, no snapshot)
      local panel_open
      panel_open=$(agent-browser eval "document.querySelector('input[placeholder=\"Search for Assets\"]') ? 'open' : 'closed'" 2>/dev/null | tr -d '"' || echo "closed")
      if [[ "$panel_open" != "open" ]]; then
        agent-browser eval "
          var icons = document.querySelectorAll('i.google-symbols');
          icons.forEach(function(i) {
            if (i.textContent.trim() === 'add_2') i.closest('button').click();
          });
        " >/dev/null 2>&1
        sleep 1
      fi

      # Click "Upload image" via its icon text
      agent-browser eval "
        var icons = document.querySelectorAll('i.google-symbols');
        icons.forEach(function(i) {
          if (i.textContent.trim() === 'upload') i.closest('[onclick], [class]').click();
        });
      " >/dev/null 2>&1
      sleep 0.5

      agent-browser upload "input[type='file']" "$ref" >/dev/null 2>&1
      sleep 2
      pasted_new=$((pasted_new + 1))
      new_ref_names+=("$name")
    fi
  done

  if [[ "$expected" -eq 0 ]]; then
    return 0
  fi

  # If any new uploads, wait for Flow asset indexing before searching.
  # Newly uploaded assets are not immediately searchable — 5s was insufficient
  # in testing; 15s amortized once covers all refs in the folder.
  if [[ "${#new_ref_names[@]}" -gt 0 ]]; then
    echo "      Waiting for asset indexing (${#new_ref_names[@]} new upload(s))..."
    sleep 15
  fi

  # Pass 2: attach all refs from library via search
  for name in "${all_ref_names[@]}"; do
    # Attach from library via search (needed for both new and existing —
    # agent-browser upload adds to library but does NOT auto-attach to prompt)
    attach_ref_to_prompt "$name" && attached=$((attached + 1))
  done

  echo "    Refs: ${attached}/${expected} attached"
  if [[ "$attached" -lt "$expected" ]]; then
    echo "    WARNING: only ${attached}/${expected} refs attached — continuing anyway"
  fi
  # Validate refs — soft check (warn only, don't abort)
  flow_validate_ref_count "$expected" || true

  # Mark new refs as uploaded after successful attachment
  for name in "${new_ref_names[@]}"; do
    mark_ref_uploaded "$name"
  done
}

# flow_auto_populate_refs <folder>
# If the folder has fewer than 3 ref files (mascot.* + ref-*.png/jpg/jpeg/webp),
# runs `contentkit registry search` using the prompt.md content as the query and
# copies up to (3 - existing) matching refs into the folder as ref-{name}.png.
# Non-fatal: logs a warning and returns 0 if no results are found.
flow_auto_populate_refs() {
  local folder="$1"
  local ref_count=0 ref

  for ref in "$folder"/mascot.* "$folder"/ref-*.png "$folder"/ref-*.jpg "$folder"/ref-*.jpeg "$folder"/ref-*.webp; do
    [[ -f "$ref" ]] && ref_count=$((ref_count + 1))
  done
  [[ "$ref_count" -ge 3 ]] && return 0

  local name needed
  name=$(basename "$folder")
  needed=$((3 - ref_count))
  echo "    Auto-populating $name: have $ref_count ref(s), need $needed from registry..."

  # Strip markdown punctuation; cap at 300 chars so the CLI gets a clean query
  local query
  query=$(sed 's/[*_#`>|]//g; s/\[//g; s/\]//g; s/  */ /g' "$folder/prompt.md" \
    | tr '\n' ' ' | cut -c1-300)

  local populated=0 src ref_name dest
  while IFS= read -r line; do
    [[ "$line" =~ Path:[[:space:]]*(.+)$ ]] || continue
    src="${BASH_REMATCH[1]}"
    [[ -f "$src" ]] || continue
    ref_name=$(basename "${src%.*}")
    dest="$folder/ref-${ref_name}.png"
    [[ -f "$dest" ]] && continue
    /bin/cp "$src" "$dest"
    echo "      Copied ref-${ref_name}.png"
    populated=$((populated + 1))
    [[ "$populated" -ge "$needed" ]] && break
  done < <(contentkit registry search "$query" 2>/dev/null)

  if [[ "$populated" -gt 0 ]]; then
    echo "==> Auto-populated $populated ref(s) in $name"
  else
    echo "    WARNING: registry search returned no results for $name — continuing without additional refs"
  fi
}

# flow_validate_ref_count <expected>
# Checks that the ingredient chip count in the prompt area matches expected.
# Retries up to 3 times with 2s waits (refs may still be loading).
flow_validate_ref_count() {
  local expected="$1"
  [[ "$expected" -eq 0 ]] && return 0

  # Flow renders ingredient chips as buttons with alt text
  # "A piece of media generated or uploaded by you..."
  # Each chip has one such img inside — count the imgs.
  local attempt count
  for attempt in 1 2; do
    count=$(agent-browser eval "
      var imgs = document.querySelectorAll('img[alt*=\"piece of media\"]');
      String(imgs.length);
    " 2>/dev/null | tr -d '"' || echo "0")
    count="${count//[!0-9]/}"; count="${count:-0}"

    if [[ "$count" -ge "$expected" ]]; then
      echo "    Refs validated: ${count}/${expected}"
      return 0
    fi

    if [[ "$attempt" -lt 2 ]]; then
      echo "    Refs loading: ${count}/${expected} — waiting..."
      sleep 2
    fi
  done

  echo "    WARNING: refs ${count}/${expected} — continuing (chip selector may be stale)"
  return 1
}

# attach_ref_to_prompt <name> — opens ingredient panel, searches, clicks result
# All interactions via direct DOM selectors — no snapshot ref lookup.
attach_ref_to_prompt() {
  local name="$1"

  # Open the + panel if not already open. Icon buttons use i.google-symbols.
  # The + button icon text is "add_2". It's a toggle — only click if panel is closed.
  local panel_open
  panel_open=$(agent-browser eval "document.querySelector('input[placeholder=\"Search for Assets\"]') ? 'open' : 'closed'" 2>/dev/null | tr -d '"' || echo "closed")

  if [[ "$panel_open" != "open" ]]; then
    agent-browser eval "
      var icons = document.querySelectorAll('i.google-symbols');
      icons.forEach(function(i) {
        if (i.textContent.trim() === 'add_2') i.closest('button').click();
      });
    " >/dev/null 2>&1
    sleep 1
  fi

  # Fill the search input directly by placeholder selector
  agent-browser fill "input[placeholder='Search for Assets']" "$name" >/dev/null 2>&1
  sleep 3

  # Click the matching asset via img[alt="filename"]
  local attempt
  for attempt in 1 2; do
    local clicked
    clicked=$(agent-browser eval "
      var img = document.querySelector('img[alt=\"$name\"]');
      if (img) { img.closest('[data-item-index]').click(); 'clicked'; } else { 'not found'; }
    " 2>/dev/null | tr -d '"' || echo "error")

    if [[ "$clicked" == "clicked" ]]; then
      sleep 1
      echo "      Attached: $name"
      return 0
    fi

    if [[ "$attempt" -lt 2 ]]; then
      echo "      $name not found (attempt $attempt/2) — retrying..."
      sleep 3
    fi
  done

  echo "      FAIL: $name not found after 2 attempts"
  return 1
}

# ---------------------------------------------------------------------------
# Project management
# ---------------------------------------------------------------------------
# flow_create_project
# Creates a new Flow project and prints the UUID to stdout.
flow_create_project() {
  flow_ensure_browser
  agent-browser open "https://labs.google/fx/tools/flow/" >/dev/null
  sleep 4
  agent-browser wait --load networkidle >/dev/null 2>&1 || true

  # Dismiss consent/announcement modals until "New project" is visible.
  dismiss_modals "document.querySelector('button') && Array.from(document.querySelectorAll('button')).some(function(b) { return b.textContent.includes('New project'); })"

  local new_ref
  new_ref=$(find_ref "New project")
  if [[ -z "$new_ref" ]]; then
    echo "  ERROR: 'New project' button not found — is Chrome debug running?" >&2
    return 1
  fi

  agent-browser click "@$new_ref" >/dev/null 2>&1 || true
  sleep 3

  # Fallback: JS click if CDP click didn't navigate
  local check_url
  check_url=$(agent-browser eval "window.location.href" 2>/dev/null | tr -d '"')
  if [[ "$check_url" != *"/project/"* ]]; then
    echo "  CDP click didn't navigate — trying JS click..." >&2
    agent-browser eval "document.querySelectorAll('button').forEach(function(b) { if (b.textContent.includes('New project')) b.click(); })" >/dev/null 2>&1
    sleep 5
  fi

  local current_url project_id
  current_url=$(agent-browser eval "window.location.href" 2>/dev/null | tr -d '"')
  project_id=$(echo "$current_url" \
    | grep -oE '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}' \
    | head -1)

  if [[ -z "$project_id" ]]; then
    echo "  ERROR: could not extract project UUID from URL: $current_url" >&2
    return 1
  fi

  echo "$project_id"
}

# flow_slug_project <illustrate_dir> <file_suffix> [explicit_project_id]
# Per-slug project: reuse existing or create new. Sets PROJECT_ID.
# file_suffix: ".flow-project-id" or ".flow-video-project-id"
flow_slug_project() {
  local illustrate_dir="$1" file_suffix="$2" explicit_id="${3:-}"
  local project_file="$illustrate_dir/$file_suffix"
  mkdir -p "$illustrate_dir"

  if [[ -n "$explicit_id" ]]; then
    PROJECT_ID="$explicit_id"
  elif [[ -f "$project_file" ]]; then
    PROJECT_ID=$(cat "$project_file")
    echo "==> Reusing project $PROJECT_ID ($(basename "$project_file"))"
  else
    echo "==> Creating new Flow project..."
    PROJECT_ID=$(flow_create_project) || return 1
    echo "$PROJECT_ID" > "$project_file"
    echo "    Created: $PROJECT_ID"
  fi
  export PROJECT_ID
}

# create_or_load_project <config_file> <base_dir>
# Reads existing project or creates a new one. Prints project UUID to stdout.
create_or_load_project() {
  local config_file="$1" base_dir="$2"

  if [[ -f "$config_file" ]]; then
    local existing_id
    existing_id=$(jq -r '.flow_video_project_id // empty' "$config_file" 2>/dev/null || true)
    if [[ -n "$existing_id" ]]; then
      echo "  Found existing project: $existing_id" >&2
      echo "$existing_id"
      return 0
    fi
  fi

  echo "  Creating new Flow project..." >&2
  flow_ensure_browser
  agent-browser open "https://labs.google/fx/tools/flow/" >/dev/null
  agent-browser wait --load networkidle >/dev/null 2>&1

  local new_ref
  new_ref=$(find_ref "New project")
  if [[ -z "$new_ref" ]]; then
    echo "  ERROR: 'New project' button not found" >&2
    return 1
  fi

  agent-browser click "@$new_ref" >/dev/null 2>&1
  agent-browser wait --url "**/project/**" >/dev/null 2>&1

  local current_url project_id
  current_url=$(agent-browser get url 2>/dev/null)
  project_id=$(echo "$current_url" \
    | grep -oE '/project/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}' \
    | head -1 \
    | sed 's|/project/||')

  if [[ -z "$project_id" ]]; then
    echo "  ERROR: could not extract project UUID from URL: $current_url" >&2
    return 1
  fi

  mkdir -p "$base_dir"
  local tmp_file
  tmp_file=$(mktemp "${config_file}.XXXXXX")
  if [[ -f "$config_file" ]]; then
    jq --arg id "$project_id" '.flow_video_project_id = $id' "$config_file" > "$tmp_file"
  else
    jq -n --arg id "$project_id" '{"flow_video_project_id": $id}' > "$tmp_file"
  fi
  mv "$tmp_file" "$config_file"
  echo "  Created project: $project_id" >&2
  echo "$project_id"
}

# ---------------------------------------------------------------------------
# Polling helpers
# ---------------------------------------------------------------------------
# _flow_check_poll_result <result_csv> <elapsed> <target>
# Parses "count,failed" CSV, logs status, returns: 0=done, 1=all-failed, 2=keep-waiting.
_flow_check_poll_result() {
  local result="$1" elapsed="$2" target="$3"
  local count="${result%%,*}"
  local failed="${result##*,}"
  count="${count//[!0-9]/}"; count="${count:-0}"
  failed="${failed//[!0-9]/}"; failed="${failed:-0}"
  local total=$((count + failed))

  if [[ "$failed" -gt 0 ]]; then
    echo "    ${elapsed}s — $count/$target images, $failed failed"
  else
    echo "    ${elapsed}s — $count/$target images"
  fi

  if [[ "$total" -ge "$target" ]]; then
    [[ "$failed" -gt 0 ]] && echo "    WARNING: $failed generation(s) failed (policy violation or error)"
    [[ "$count" -gt 0 ]] && return 0
    echo "    ERROR: all $failed generation(s) failed" && return 1
  fi
  return 2
}

# flow_poll_generated_images_api <project_id> <target> [max_wait_s]
# Counts both successful and failed generations to avoid hanging on policy violations.
flow_poll_generated_images_api() {
  local project_id="$1" target="$2"
  local max_wait="${3:-${FLOW_IMAGE_TIMEOUT:-300}}"
  local poll_interval="${FLOW_IMAGE_POLL_INTERVAL:-10}"
  local elapsed=0

  while [[ "$elapsed" -lt "$max_wait" ]]; do
    sleep "$poll_interval"
    elapsed=$((elapsed + poll_interval))
    local result
    result=$(agent-browser eval "
    (async function() {
      var resp = await fetch('/fx/api/trpc/flow.projectInitialData?input=' +
        encodeURIComponent(JSON.stringify({ json: { projectId: '$project_id' } })));
      var data = await resp.json();
      var media = data.result.data.json.projectContents.media;
      var done = 0, failed = 0;
      media.forEach(function(m) {
        if (m.image && m.image.generatedImage) { done++; }
        else if (m.mediaMetadata && m.mediaMetadata.mediaStatus
          && /FAILED|ERROR/.test(m.mediaMetadata.mediaStatus.mediaGenerationStatus || '')) { failed++; }
      });
      return String(done) + ',' + String(failed);
    })()
    " 2>/dev/null | tr -d '"' || echo "0,0")
    _flow_check_poll_result "$result" "$elapsed" "$target"
    local rc=$?
    [[ "$rc" -le 1 ]] && return "$rc"
  done
  return 1
}

# flow_poll_generated_images_dom <target> [max_wait_s] — DOM polling for flow-generate.sh
# Counts both successful and failed generations to avoid hanging on policy violations.
flow_poll_generated_images_dom() {
  local target="$1"
  local max_wait="${2:-${FLOW_IMAGE_TIMEOUT:-180}}"
  local poll_interval="${FLOW_IMAGE_POLL_INTERVAL:-10}"
  local elapsed=0

  while [[ "$elapsed" -lt "$max_wait" ]]; do
    sleep "$poll_interval"
    elapsed=$((elapsed + poll_interval))
    local result
    result=$(agent-browser eval "
      var done = document.querySelectorAll('img[alt=\"Generated image\"]').length;
      var failed = document.querySelectorAll('[data-error], [aria-label*=\"failed\"], [aria-label*=\"Failed\"]').length
        + Array.from(document.querySelectorAll('p, span, div')).filter(function(el) {
          return el.children.length === 0 && /Failed.*Something went wrong/.test(el.textContent || '');
        }).length;
      String(done) + ',' + String(failed);
    " 2>/dev/null | tr -d '"' || echo "0,0")
    _flow_check_poll_result "$result" "$elapsed" "$target"
    local rc=$?
    [[ "$rc" -le 1 ]] && return "$rc"
  done
  return 1
}

# flow_poll_video_uuid <project_id> <known_csv> [max_wait_s]
# Prints new UUID to stdout on success. Returns 1 on timeout.
flow_poll_video_uuid() {
  local project_id="$1" known="$2"
  local max_wait="${3:-90}"
  local poll_interval="${FLOW_VIDEO_POLL_INTERVAL:-15}"
  local elapsed=0

  while [[ "$elapsed" -lt "$max_wait" ]]; do
    sleep "$poll_interval"
    elapsed=$((elapsed + poll_interval))
    local after_uuids uuid
    after_uuids=$(agent-browser eval "
    (async function() {
      var resp = await fetch('/fx/api/trpc/flow.projectInitialData?input=' +
        encodeURIComponent(JSON.stringify({ json: { projectId: '$project_id' } })));
      var data = await resp.json();
      return data.result.data.json.projectContents.media
        .filter(function(m) { return m.video; }).map(function(m) { return m.name; }).join(',');
    })()
    " 2>/dev/null | tr -d '"') || :
    for uuid in $(echo "$after_uuids" | tr ',' '\n'); do
      if [[ -n "$uuid" ]] && ! echo "$known" | grep -q "$uuid"; then
        echo "      ${elapsed}s — UUID: ${uuid:0:8}..." >&2
        echo "$uuid"
        return 0
      fi
    done
  done
  return 1
}

# flow_poll_video_render <project_id> <uuid> [max_wait_s]
flow_poll_video_render() {
  local project_id="$1" video_uuid="$2"
  local max_wait="${3:-${FLOW_VIDEO_TIMEOUT:-300}}"
  local poll_interval="${FLOW_VIDEO_POLL_INTERVAL:-15}"
  local elapsed=0

  while [[ "$elapsed" -lt "$max_wait" ]]; do
    local gen_status
    gen_status=$(agent-browser eval "
      (async function() {
        var resp = await fetch('/fx/api/trpc/flow.projectInitialData?input=' +
          encodeURIComponent(JSON.stringify({ json: { projectId: '$project_id' } })));
        var data = await resp.json();
        var media = data.result.data.json.projectContents.media;
        var v = media.find(function(m) { return m.name === '$video_uuid'; });
        return v ? v.mediaMetadata.mediaStatus.mediaGenerationStatus : 'NOT_FOUND';
      })()
    " 2>/dev/null | tr -d '"')
    echo "      ${elapsed}s — $gen_status"
    [[ "$gen_status" == "MEDIA_GENERATION_STATUS_SUCCESSFUL" ]] && return 0
    if echo "$gen_status" | grep -qiE "FAILED|ERROR"; then
      echo "      ERROR: generation failed — $gen_status" >&2
      return 1
    fi
    sleep "$poll_interval"
    elapsed=$((elapsed + poll_interval))
  done
  return 1
}

# ---------------------------------------------------------------------------
# flow_make_clip — end-to-end sequential clip generation (used by flow-gif.sh)
# ---------------------------------------------------------------------------
# flow_make_clip <project_id> <start_name> <end_name> <clip_prompt> <known_uuids_var> <outfile>
#   known_uuids_var: name of the variable holding known UUIDs (updated in place)
#   outfile: path where the downloaded MP4 will be saved
# Returns 0 on success (outfile written), 1 on any failure.
flow_make_clip() {
  local project_id="$1" start_name="$2" end_name="$3"
  local clip_prompt="$4" known_var="$5" outfile="$6"

  select_frame "Start" "$start_name" || return 1
  select_frame "End" "$end_name"     || return 1

  echo "$clip_prompt" | pbcopy
  agent-browser eval "
    var boxes = document.querySelectorAll('div[contenteditable=\"true\"]');
    var box = boxes[boxes.length - 1];
    if (box) { box.click(); box.focus(); document.execCommand('selectAll'); document.execCommand('delete'); }
  " >/dev/null 2>&1
  sleep 0.3
  agent-browser press "Meta+v" >/dev/null 2>&1
  sleep 0.5

  local paste_len
  paste_len=$(agent-browser eval "
    var boxes = document.querySelectorAll('div[contenteditable=\"true\"]');
    boxes[boxes.length - 1].textContent.length;
  " 2>/dev/null | tr -d '"' || echo "0")
  paste_len="${paste_len//[!0-9]/}"; paste_len="${paste_len:-0}"
  if [[ "$paste_len" -lt 20 ]]; then
    echo "      ERROR: motion prompt paste failed ($paste_len chars)" >&2
    return 1
  fi

  local pre_snap start_bare end_bare
  pre_snap=$(agent-browser snapshot 2>/dev/null)
  start_bare=$(echo "$pre_snap" | grep -c '"text: Start"' || echo "0")
  end_bare=$(echo "$pre_snap" | grep -c '"text: End"' || echo "0")
  start_bare="${start_bare//[!0-9]/}"; start_bare="${start_bare:-0}"
  end_bare="${end_bare//[!0-9]/}"; end_bare="${end_bare:-0}"
  if [[ "$start_bare" -gt 0 || "$end_bare" -gt 0 ]]; then
    echo "      ERROR: slot not filled (start=$start_bare end=$end_bare)" >&2
    return 1
  fi

  local create_result
  create_result=$(agent-browser eval "
    var btn = Array.from(document.querySelectorAll('button')).find(function(b) {
      return b.textContent.includes('arrow_forward') && b.textContent.includes('Create');
    });
    if (btn) { btn.click(); 'submitted'; } else { 'no create btn'; }
  " 2>/dev/null | tr -d '"')
  if [[ "$create_result" != "submitted" ]]; then
    echo "      ERROR: Create button not found" >&2
    return 1
  fi

  local known_now video_uuid
  known_now="${!known_var}"
  echo "      Polling for video UUID..."
  video_uuid=$(flow_poll_video_uuid "$project_id" "$known_now" 300) || {
    echo "      TIMEOUT — no UUID appeared" >&2; return 1
  }
  # Update the caller's known_uuids variable
  printf -v "$known_var" '%s' "${known_now:+$known_now,}$video_uuid"

  echo "      Waiting for render..."
  flow_poll_video_render "$project_id" "$video_uuid" "$FLOW_VIDEO_TIMEOUT" || return 1

  local gcs_url
  gcs_url=$(agent-browser eval "
    (async function() {
      var resp = await fetch('/fx/api/trpc/media.getMediaUrlRedirect?name=$video_uuid');
      return resp.url;
    })()
  " 2>/dev/null | tr -d '"')
  if [[ -z "$gcs_url" ]] || echo "$gcs_url" | grep -qE "undefined|null"; then
    echo "      ERROR: no download URL" >&2; return 1
  fi

  curl -sL -o "$outfile" "$gcs_url" 2>/dev/null
  local size
  size=$(stat -f%z "$outfile" 2>/dev/null || echo "0")
  if [[ "$size" -lt "${FLOW_MIN_VIDEO_BYTES:-10000}" ]]; then
    echo "      ERROR: only $size bytes" >&2
    rm -f "$outfile"; return 1
  fi
  echo "      OK $(basename "$outfile") ($size bytes)"
}
