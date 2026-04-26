#!/usr/bin/env bash
# flow-video.sh — Generate YouTube video clips via Google Flow (keyframes → clips → concat)
# Usage: flow-video.sh <slug> [--minutes N] [--stage 0|1|1b|2|3] [--portrait] [--style name]
# Stages: 0=create project, 1=keyframe images, 1b=re-upload KFs, 2=video clips, 3=concat

set -euo pipefail

LIB_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=flow-lib.sh
source "$LIB_DIR/flow-lib.sh"

SLUG=""; STAGE=""; MINUTES=8; PORTRAIT=false; STYLE="claudius"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --stage)   STAGE="${2:?--stage requires 0|1|1b|2|3}"; shift 2 ;;
    --minutes) MINUTES="${2:?--minutes requires a number}"
               [[ "$MINUTES" =~ ^[0-9]+(\.[0-9]+)?$ ]] || { echo "ERROR: --minutes must be numeric" >&2; exit 1; }
               shift 2 ;;
    --portrait) PORTRAIT=true; shift ;;
    --style)    STYLE="${2:?--style requires a value}"; shift 2 ;;
    -*)         echo "ERROR: unknown flag: $1" >&2; exit 1 ;;
    *)          [[ -z "$SLUG" ]] && SLUG="$1" || { echo "ERROR: unexpected arg: $1" >&2; exit 1; }; shift ;;
  esac
done
[[ -z "$SLUG" ]] && { echo "Usage: flow-video.sh <slug> [--minutes N] [--stage 0|1|1b|2|3] [--portrait]" >&2; exit 1; }
[[ -n "$STAGE" && "$STAGE" != "0" && "$STAGE" != "1" && "$STAGE" != "1b" && "$STAGE" != "2" && "$STAGE" != "3" ]] && \
  { echo "ERROR: invalid stage '$STAGE'" >&2; exit 1; }

EXPECTED_CLIPS=$(echo "$MINUTES * 60 / 8" | bc)
EXPECTED_KFS=$(( EXPECTED_CLIPS + 1 ))

flow_load_config "$STYLE" "flow_video_project_id"
for cmd in jq ffmpeg; do
  command -v "$cmd" >/dev/null 2>&1 || { echo "ERROR: $cmd required (brew install $cmd)" >&2; exit 1; }
done

BASE_DIR="/tmp/illustrate/$SLUG"; KF_DIR="$BASE_DIR/keyframes"; CLIPS_DIR="$BASE_DIR/clips"
CONFIG_FILE="$BASE_DIR/config.json"; VEO_FILE="$BASE_DIR/veo-prompts-clean.txt"
[[ -d "$KF_DIR" ]] || { echo "ERROR: $KF_DIR not found" >&2; exit 1; }
mkdir -p "$CLIPS_DIR"

should_run() { [[ -z "$STAGE" || "$STAGE" == "$1" ]]; }

KF_FOLDERS=()
while IFS= read -r d; do KF_FOLDERS+=("$d"); done \
  < <(find "$KF_DIR" -mindepth 1 -maxdepth 1 -type d -name "KF-*" | sort)
KF_COUNT="${#KF_FOLDERS[@]}"
[[ "$KF_COUNT" -lt 2 ]] && { echo "ERROR: need >= 2 KF-XX folders, found $KF_COUNT" >&2; exit 1; }
[[ "$KF_COUNT" -ne "$EXPECTED_KFS" ]] && \
  echo "WARNING: expected $EXPECTED_KFS KFs for ${MINUTES}min, found $KF_COUNT — proceeding" >&2

echo "==> flow-video.sh: $SLUG | ${MINUTES}min | $KF_COUNT KFs / $((KF_COUNT-1)) clips${STAGE:+ | stage $STAGE only}"

# ===========================================================================
# Stage 0: Create or load project
# ===========================================================================
if should_run "0"; then
  echo ""; echo "==> Stage 0: Project setup"
  PROJECT_ID=$(create_or_load_project "$CONFIG_FILE" "$BASE_DIR")
  echo "    Project ID: $PROJECT_ID"
  portrait_bool=$([[ "$PORTRAIT" == "true" ]] && echo "true" || echo "false")
  jq --argjson m "$MINUTES" --argjson c "$EXPECTED_CLIPS" --argjson k "$EXPECTED_KFS" --argjson p "$portrait_bool" \
    '. + {minutes: $m, expected_clips: $c, expected_kfs: $k, portrait: $p}' \
    "$CONFIG_FILE" > "$CONFIG_FILE.tmp" && mv "$CONFIG_FILE.tmp" "$CONFIG_FILE"
else
  PROJECT_ID=$(jq -r '.flow_video_project_id // empty' "$CONFIG_FILE" 2>/dev/null || true)
  [[ -z "$PROJECT_ID" ]] && { echo "ERROR: no project ID in $CONFIG_FILE — run stage 0 first" >&2; exit 1; }
fi

flow_init_ref_tracking "flow-video" "$SLUG" "$PROJECT_ID"

if should_run "1" || should_run "1b" || should_run "2"; then
  echo "==> Opening Flow project $PROJECT_ID"
  agent-browser open "https://labs.google/fx/tools/flow/project/$PROJECT_ID" >/dev/null
  sleep 6
fi

# ===========================================================================
# Stage 1: Generate keyframe images
# ===========================================================================
if should_run "1"; then
  echo ""; echo "==> Stage 1: Generating $KF_COUNT keyframe images"
  load_uploaded_refs
  ensure_image_mode
  [[ "$PORTRAIT" == "true" ]] && select_aspect_ratio "portrait"

  kf_baseline=$(agent-browser eval "
  (async function() {
    var resp = await fetch('/fx/api/trpc/flow.projectInitialData?input=' +
      encodeURIComponent(JSON.stringify({ json: { projectId: '$PROJECT_ID' } })));
    var data = await resp.json();
    return data.result.data.json.projectContents.media
      .filter(function(m) { return m.image && m.image.generatedImage; }).length;
  })()
  " 2>/dev/null | tr -d '"' || echo "0")
  kf_baseline="${kf_baseline//[!0-9]/}"; kf_baseline="${kf_baseline:-0}"

  KF_SUBMITTED=0
  for folder in "${KF_FOLDERS[@]}"; do
    kf_name=$(basename "$folder"); kf_output="$KF_DIR/${kf_name}.png"
    if [[ -f "$kf_output" ]]; then
      sz=$(stat -f%z "$kf_output" 2>/dev/null || echo "0")
      [[ "$sz" -gt 10240 ]] && { echo "    SKIP $kf_name ($sz bytes)"; continue; }
    fi
    echo "    Processing $kf_name..."
    # Clear contenteditable before pasting refs (matches flow-gif.sh pattern)
    agent-browser eval "
      var boxes = document.querySelectorAll('div[contenteditable=\"true\"]');
      var box = boxes[boxes.length - 1];
      if (box) { box.click(); box.focus(); document.execCommand('selectAll'); document.execCommand('delete'); }
    " >/dev/null 2>&1
    sleep 0.3
    expected_refs=$(find "$folder" -type f \( -name "mascot.*" -o -name "ref-*.png" -o -name "ref-*.jpg" -o -name "ref-*.jpeg" -o -name "ref-*.webp" \) | wc -l | tr -d ' ')
    while IFS= read -r ref_file; do paste_ref_as_ingredient "$ref_file"; done \
      < <(find "$folder" -type f \( -name "mascot.*" -o -name "ref-*.png" -o -name "ref-*.jpg" -o -name "ref-*.jpeg" -o -name "ref-*.webp" \) | sort)
    sleep 1
    actual_refs=$(agent-browser eval "
      (function() {
        var box = document.querySelectorAll('div[contenteditable=\"true\"]');
        var last = box[box.length - 1]; if (!last) return 0;
        var imgs = last.querySelectorAll('img'); if (imgs.length > 0) return imgs.length;
        var chips = last.parentElement ? last.parentElement.querySelectorAll('img') : [];
        return chips.length;
      })()" 2>/dev/null | tr -d '"' || echo "0") || :
    actual_refs="${actual_refs//[!0-9]/}"; actual_refs="${actual_refs:-0}"
    [[ "$actual_refs" -ge "$expected_refs" && "$expected_refs" -gt 0 ]] \
      && echo "      Ingredients: $actual_refs/$expected_refs" \
      || echo "      WARNING: expected $expected_refs, got $actual_refs — proceeding"
    [[ -f "$folder/prompt.md" ]] || { echo "      ERROR: no prompt.md — skipping"; continue; }
    submit_image "$folder/prompt.md"
    KF_SUBMITTED=$((KF_SUBMITTED + 1))
  done

  if [[ "$KF_SUBMITTED" -eq 0 ]]; then
    echo "    All keyframes already exist"
  else
    kf_target=$((kf_baseline + KF_SUBMITTED))
    echo "  Waiting for $KF_SUBMITTED images (target: $kf_target)..."
    flow_poll_generated_images_api "$PROJECT_ID" "$kf_target" "$FLOW_IMAGE_TIMEOUT" || \
      { echo "    ERROR: timed out" >&2; exit 1; }
    sleep 2

    KF_FAILED=0; kf_idx=0
    for folder in "${KF_FOLDERS[@]}"; do
      kf_name=$(basename "$folder"); kf_output="$KF_DIR/${kf_name}.png"
      if [[ -f "$kf_output" ]]; then
        sz=$(stat -f%z "$kf_output" 2>/dev/null || echo "0")
        [[ "$sz" -gt 10240 ]] && { kf_idx=$((kf_idx + 1)); continue; }
      fi
      lb=$kf_baseline; li=$kf_idx
      uuid=$(agent-browser eval "
      (async function() {
        var resp = await fetch('/fx/api/trpc/flow.projectInitialData?input=' +
          encodeURIComponent(JSON.stringify({ json: { projectId: '$PROJECT_ID' } })));
        var data = await resp.json();
        var media = data.result.data.json.projectContents.media
          .filter(function(m) { return m.image && m.image.generatedImage; });
        media.sort(function(a,b){
          var ta=a.image.generatedImage.createTime||'', tb=b.image.generatedImage.createTime||'';
          return ta<tb?-1:ta>tb?1:0;
        });
        var e=media[$lb+$li]; return e?e.name:'';
      })()" 2>/dev/null | tr -d '"')
      if [[ -z "$uuid" ]]; then
        echo "    FAIL $kf_name — no image at position $((lb + li))"
        KF_FAILED=$((KF_FAILED + 1)); kf_idx=$((kf_idx + 1)); continue
      fi
      echo "    Downloading $kf_name ($uuid)..."
      download_image "$uuid" "$kf_output" || KF_FAILED=$((KF_FAILED + 1))
      kf_idx=$((kf_idx + 1))
    done
    [[ "$KF_FAILED" -gt 0 ]] && { echo "    ERROR: $KF_FAILED KFs failed" >&2; exit 1; }
  fi
fi

# ===========================================================================
# Stage 1b: Re-upload KF-XX.png with searchable names
# ===========================================================================
if should_run "1b"; then
  echo ""; echo "==> Stage 1b: Re-uploading keyframes"
  load_uploaded_refs
  for folder in "${KF_FOLDERS[@]}"; do
    kf_name=$(basename "$folder"); kf_output="$KF_DIR/${kf_name}.png"
    [[ -f "$kf_output" ]] || { echo "    SKIP $kf_name — run stage 1 first"; continue; }
    upload_ref_if_needed "$kf_output"
  done
  echo "    Waiting for uploads to index..."; sleep 10
  agent-browser open "https://labs.google/fx/tools/flow/project/$PROJECT_ID" >/dev/null; sleep 6
fi

# ===========================================================================
# Stage 2: Generate video clips
# ===========================================================================
if should_run "2"; then
  echo ""; echo "==> Stage 2: Generating video clips"
  [[ -f "$VEO_FILE" ]] || { echo "ERROR: $VEO_FILE not found" >&2; exit 1; }

  VEO_PROMPTS=(); current_prompt=""
  while IFS= read -r line; do
    if [[ "$line" == "---" ]]; then
      [[ -n "$current_prompt" ]] && VEO_PROMPTS+=("$(echo "$current_prompt" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')")
      current_prompt=""
    else
      [[ -n "$current_prompt" ]] && current_prompt+="
$line" || current_prompt="$line"
    fi
  done < "$VEO_FILE"
  [[ -n "$current_prompt" ]] && VEO_PROMPTS+=("$(echo "$current_prompt" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')")

  CLIP_COUNT=$((KF_COUNT - 1)); PROMPT_COUNT="${#VEO_PROMPTS[@]}"
  echo "    $CLIP_COUNT clips / $PROMPT_COUNT Veo prompts"
  [[ "$PROMPT_COUNT" -lt "$CLIP_COUNT" ]] && echo "    WARNING: fewer prompts than clips — last reused"

  activate_video_mode; sleep 1
  [[ "$PORTRAIT" == "true" ]] && select_aspect_ratio "portrait"

  CLIP_FILES=()
  declare -A CLIP_UUIDS
  known_video_uuids=$(agent-browser eval "
  (async function() {
    var resp = await fetch('/fx/api/trpc/flow.projectInitialData?input=' +
      encodeURIComponent(JSON.stringify({ json: { projectId: '$PROJECT_ID' } })));
    var data = await resp.json();
    return data.result.data.json.projectContents.media
      .filter(function(m) { return m.video; }).map(function(m) { return m.name; }).join(',');
  })()" 2>/dev/null | tr -d '"') || :

  # Phase 1: Submit all clips
  for c in $(seq 1 "$CLIP_COUNT"); do
    clip_num=$(printf '%02d' "$c"); clip_file="$CLIPS_DIR/clip-${clip_num}.mp4"
    if [[ -f "$clip_file" ]]; then
      sz=$(stat -f%z "$clip_file" 2>/dev/null || echo "0")
      if [[ "$sz" -gt 10240 ]]; then
        echo "    SKIP clip-$clip_num ($sz bytes)"; CLIP_FILES+=("$clip_file"); CLIP_UUIDS[$c]="skip"; continue
      fi
    fi
    start_kf=$(printf '%02d' "$c"); end_kf=$(printf '%02d' $((c+1)))
    echo "    Clip $c/$CLIP_COUNT: KF-$start_kf → KF-$end_kf"
    select_frame "Start" "KF-${start_kf}.png" || { echo "    ERROR: Start failed — skipping"; continue; }
    select_frame "End"   "KF-${end_kf}.png"   || { echo "    ERROR: End failed — skipping";   continue; }

    pi=$((c - 1)); [[ "$pi" -ge "$PROMPT_COUNT" ]] && pi=$((PROMPT_COUNT - 1))
    echo "${VEO_PROMPTS[$pi]}" | pbcopy
    agent-browser eval "
      var b=document.querySelectorAll('div[contenteditable=\"true\"]');
      var box=b[b.length-1];
      if(box){box.click();box.focus();document.execCommand('selectAll');document.execCommand('delete');}
    " >/dev/null 2>&1; sleep 0.3
    agent-browser press "Meta+v" >/dev/null 2>&1; sleep 0.5

    plen=$(agent-browser eval "
      var b=document.querySelectorAll('div[contenteditable=\"true\"]');
      b[b.length-1].textContent.length;" 2>/dev/null | tr -d '"' || echo "0")
    plen="${plen//[!0-9]/}"; plen="${plen:-0}"
    [[ "$plen" -lt 20 ]] && { echo "      ERROR: paste failed ($plen chars) — skipping"; continue; }

    pre=$(agent-browser snapshot 2>/dev/null)
    sb=$(echo "$pre" | grep -c -e '"text: Start"' || :); sb="${sb//[!0-9]/}"; sb="${sb:-0}"
    eb=$(echo "$pre" | grep -c -e '"text: End"' || :);   eb="${eb//[!0-9]/}"; eb="${eb:-0}"
    [[ "$sb" -gt 0 || "$eb" -gt 0 ]] && { echo "      ERROR: slot not filled — skipping"; continue; }

    cr=$(agent-browser eval "
      var btn=Array.from(document.querySelectorAll('button')).find(function(b){
        return b.textContent.includes('arrow_forward')&&b.textContent.includes('Create');
      });
      if(btn){btn.click();'submitted';}else{'no create btn';}" 2>/dev/null | tr -d '"')
    [[ "$cr" != "submitted" ]] && { echo "      ERROR: Create not found — skipping"; continue; }

    echo "      Waiting for UUID..."
    video_uuid=$(flow_poll_video_uuid "$PROJECT_ID" "$known_video_uuids" 90) || \
      { echo "      TIMEOUT — skipping"; continue; }
    CLIP_UUIDS[$c]="$video_uuid"
    known_video_uuids="${known_video_uuids:+$known_video_uuids,}$video_uuid"
  done

  # Phase 2: Wait for all clips to render (parallel)
  echo "  Waiting for all clips to render..."
  pending_uuids=""
  for c in $(seq 1 "$CLIP_COUNT"); do
    [[ "${CLIP_UUIDS[$c]:-}" == "skip" || -z "${CLIP_UUIDS[$c]:-}" ]] && continue
    pending_uuids="${pending_uuids:+$pending_uuids,}${CLIP_UUIDS[$c]}"
  done
  [[ -n "$pending_uuids" ]] && flow_wait_all_renders "$PROJECT_ID" "$pending_uuids" 600

  # Phase 3: Download all completed clips
  echo "  Downloading clips..."
  for c in $(seq 1 "$CLIP_COUNT"); do
    clip_num=$(printf '%02d' "$c"); clip_file="$CLIPS_DIR/clip-${clip_num}.mp4"
    [[ "${CLIP_UUIDS[$c]:-}" == "skip" ]] && continue
    video_uuid="${CLIP_UUIDS[$c]:-}"
    [[ -z "$video_uuid" ]] && { echo "    SKIP clip-$clip_num — no UUID"; continue; }
    gcs_url=$(agent-browser eval "
      (async function(){var r=await fetch('/fx/api/trpc/media.getMediaUrlRedirect?name=$video_uuid');return r.url;})()" \
      2>/dev/null | tr -d '"')
    if [[ -z "$gcs_url" ]] || echo "$gcs_url" | grep -qE "undefined|null"; then
      echo "    ERROR: no URL for clip-$clip_num"; continue
    fi
    curl -sL -o "$clip_file" "$gcs_url" 2>/dev/null
    sz=$(stat -f%z "$clip_file" 2>/dev/null || echo "0")
    if [[ "$sz" -gt "${FLOW_MIN_VIDEO_BYTES:-10000}" ]]; then
      echo "    OK clip-$clip_num ($sz bytes)"; CLIP_FILES+=("$clip_file")
    else
      echo "    ERROR: clip-$clip_num is $sz bytes"; rm -f "$clip_file"
    fi
  done
  echo "    Downloaded: ${#CLIP_FILES[@]}/$CLIP_COUNT"
fi

# ===========================================================================
# Stage 3: Concat clips → combined.mp4
# ===========================================================================
if should_run "3"; then
  echo ""; echo "==> Stage 3: Concatenating clips"
  CLIP_FILES=("${CLIP_FILES[@]+"${CLIP_FILES[@]}"}")
  if [[ ${#CLIP_FILES[@]} -eq 0 ]]; then
    while IFS= read -r f; do CLIP_FILES+=("$f"); done < <(find "$CLIPS_DIR" -name "clip-*.mp4" -size +10k | sort)
  fi
  [[ "${#CLIP_FILES[@]}" -eq 0 ]] && { echo "    ERROR: no clips in $CLIPS_DIR" >&2; exit 1; }

  COMBINED="$BASE_DIR/combined.mp4"
  if [[ "${#CLIP_FILES[@]}" -eq 1 ]]; then
    cp "${CLIP_FILES[0]}" "$COMBINED"; echo "    Single clip → combined.mp4"
  else
    FILELIST=$(mktemp)
    for cf in "${CLIP_FILES[@]}"; do echo "file '$cf'" >> "$FILELIST"; done
    ffmpeg -f concat -safe 0 -i "$FILELIST" -c copy -y "$COMBINED" 2>/dev/null; rm -f "$FILELIST"
  fi
  sz=$(stat -f%z "$COMBINED" 2>/dev/null || echo "0")
  [[ "$sz" -gt "${FLOW_MIN_VIDEO_BYTES:-10000}" ]] \
    && echo "    OK combined.mp4 ($sz bytes)" \
    || { echo "    FAIL combined.mp4 ($sz bytes)" >&2; rm -f "$COMBINED"; exit 1; }
fi

echo ""; echo "==> Done: $SLUG"
ls -la "$KF_DIR"/KF-*.png 2>/dev/null | awk '{printf "    KF %s (%s bytes)\n",$NF,$5}' || true
ls -la "$CLIPS_DIR"/clip-*.mp4 2>/dev/null | awk '{printf "    clip %s (%s bytes)\n",$NF,$5}' || true
[[ -f "$BASE_DIR/combined.mp4" ]] && echo "    Combined: $BASE_DIR/combined.mp4" || true
