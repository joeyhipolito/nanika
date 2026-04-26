#!/usr/bin/env bash
# flow-gif.sh — Generate animated WebP via Google Flow (keyframes → clips → WebP)
#
# Usage:
#   flow-gif.sh <slug> [--style name]
#
# Style default: claudius. Folder structure: /tmp/illustrate/<slug>/gifs/gif-<name>/
#   kf-NN-prompt.md (>=2 required), prompt.txt, clip-NN-prompt.txt (optional per-clip)
# Output: animated .webp files (80-90% smaller than GIF at same quality)

set -euo pipefail

LIB_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=flow-lib.sh
source "$LIB_DIR/flow-lib.sh"

STYLE="claudius"
_pos=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --style) STYLE="${2:?--style requires a value}"; shift 2 ;;
    --*)     echo "ERROR: unknown flag $1" >&2; exit 1 ;;
    *)       _pos+=("$1"); shift ;;
  esac
done

SLUG="${_pos[0]:?Usage: flow-gif.sh <slug> [--style name]}"

flow_load_config "$STYLE"

for cmd in jq ffmpeg; do
  command -v "$cmd" >/dev/null 2>&1 || { echo "ERROR: $cmd required (brew install $cmd)" >&2; exit 1; }
done

GIFS_DIR="/tmp/illustrate/$SLUG/gifs"
GENERATED_DIR="$GIFS_DIR/generated"
[[ -d "$GIFS_DIR" ]] || { echo "ERROR: gifs directory not found: $GIFS_DIR" >&2; exit 1; }
mkdir -p "$GENERATED_DIR"

# Per-slug project to avoid cross-contamination
flow_slug_project "/tmp/illustrate/$SLUG" ".flow-video-project-id"

# Track uploaded refs so the same image isn't re-uploaded across folders
flow_init_ref_tracking "flow-gif" "$SLUG" "$PROJECT_ID"

echo "==> Opening Flow project $PROJECT_ID"
agent-browser open "https://labs.google/fx/tools/flow/project/$PROJECT_ID" >/dev/null
sleep 6

GIF_MADE=0

for folder in "$GIFS_DIR"/gif-*/; do
  [[ -d "$folder" ]] || continue
  name=$(basename "$folder")

  KF_PROMPTS=()
  while IFS= read -r f; do KF_PROMPTS+=("$f"); done \
    < <(find "$folder" -name "kf-*-prompt.md" | sort)

  if [[ "${#KF_PROMPTS[@]}" -lt 2 ]]; then
    echo "    SKIP $name — need >= 2 kf-*-prompt.md (found ${#KF_PROMPTS[@]})"
    continue
  fi

  KF_COUNT="${#KF_PROMPTS[@]}"
  CLIP_COUNT=$((KF_COUNT - 1))
  echo ""
  echo "==> $name: $KF_COUNT keyframes → $CLIP_COUNT clips"

  # --- Stage 1: Generate keyframe images ---
  ensure_image_mode
  KF_DIR="$folder/keyframes"
  mkdir -p "$KF_DIR"

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

  for i in $(seq 0 $((KF_COUNT - 1))); do
    echo "    Submitting KF-$(printf '%02d' $((i+1)))..."
    # Clear contenteditable
    agent-browser eval "
      var boxes = document.querySelectorAll('div[contenteditable=\"true\"]');
      var box = boxes[boxes.length - 1];
      if (box) { box.click(); box.focus(); document.execCommand('selectAll'); document.execCommand('delete'); }
    " >/dev/null 2>&1
    sleep 0.3
    # Attach or upload refs (first prompt uploads, subsequent prompts attach from library)
    if ! flow_attach_or_upload_refs "$folder"; then
      echo "    ABORT — refs not fully attached"
      break
    fi
    submit_image "${KF_PROMPTS[$i]}"
  done

  kf_target=$((kf_baseline + KF_COUNT))
  flow_poll_generated_images_api "$PROJECT_ID" "$kf_target" "$FLOW_IMAGE_TIMEOUT" || {
    echo "    ABORT $name — timed out"; continue
  }
  sleep 2

  KF_FAILED=0
  for i in $(seq 0 $((KF_COUNT - 1))); do
    kf_num=$(printf '%02d' $((i + 1)))
    tag="${name}-kf-${kf_num}"
    uuid=$(agent-browser eval "
    (async function() {
      var resp = await fetch('/fx/api/trpc/flow.projectInitialData?input=' +
        encodeURIComponent(JSON.stringify({ json: { projectId: '$PROJECT_ID' } })));
      var data = await resp.json();
      var matches = data.result.data.json.projectContents.media.filter(function(m) {
        return m.image && m.image.generatedImage && m.image.generatedImage.prompt &&
               m.image.generatedImage.prompt.indexOf('$tag') !== -1;
      });
      return matches.length > 0 ? matches[0].name : '';
    })()
    " 2>/dev/null | tr -d '"')
    if [[ -z "$uuid" ]]; then
      echo "    FAIL KF-$kf_num — no UUID for $tag"; KF_FAILED=$((KF_FAILED + 1)); continue
    fi
    download_image "$uuid" "$KF_DIR/KF-${kf_num}.png" || KF_FAILED=$((KF_FAILED + 1))
  done
  [[ "$KF_FAILED" -gt 0 ]] && { echo "    ABORT $name — $KF_FAILED KF downloads failed"; continue; }

  # --- Stage 1b: Re-upload keyframes with searchable names ---
  echo "  Stage 1b: Re-uploading keyframes"
  for i in $(seq 0 $((KF_COUNT - 1))); do
    kf_num=$(printf '%02d' $((i + 1)))
    upload_path="/tmp/${name}-kf-${kf_num}.png"
    cp "$KF_DIR/KF-${kf_num}.png" "$upload_path"
    agent-browser upload "input[type='file']" "$upload_path" >/dev/null 2>&1 || \
      echo "      WARNING: upload failed for KF-$kf_num"
    rm -f "$upload_path"
    sleep 3
  done
  echo "    Waiting for uploads to index..."; sleep 10
  agent-browser open "https://labs.google/fx/tools/flow/project/$PROJECT_ID" >/dev/null; sleep 6

  # --- Stage 2: Generate video clips (sequential) ---
  [[ -f "$folder/prompt.txt" && -s "$folder/prompt.txt" ]] || {
    echo "    ABORT $name — prompt.txt missing or empty"; continue
  }
  VEO_PROMPT=$(cat "$folder/prompt.txt")

  activate_video_mode; sleep 1

  CLIP_FILES=()
  # shellcheck disable=SC2034  # modified by flow_make_clip via printf -v
  known_uuids=$(agent-browser eval "
  (async function() {
    var resp = await fetch('/fx/api/trpc/flow.projectInitialData?input=' +
      encodeURIComponent(JSON.stringify({ json: { projectId: '$PROJECT_ID' } })));
    var data = await resp.json();
    return data.result.data.json.projectContents.media
      .filter(function(m) { return m.video; }).map(function(m) { return m.name; }).join(',');
  })()
  " 2>/dev/null | tr -d '"') || :

  for c in $(seq 1 "$CLIP_COUNT"); do
    start_name="${name}-kf-$(printf '%02d' "$c").png"
    end_name="${name}-kf-$(printf '%02d' $((c+1))).png"
    clip_prompt_file="$folder/clip-$(printf '%02d' "$c")-prompt.txt"
    clip_prompt="$VEO_PROMPT"
    [[ -f "$clip_prompt_file" && -s "$clip_prompt_file" ]] && clip_prompt=$(cat "$clip_prompt_file")
    clip_file="$GENERATED_DIR/${name}-clip-$(printf '%02d' "$c").mp4"

    echo "    Clip $c/$CLIP_COUNT: $start_name → $end_name"
    flow_make_clip "$PROJECT_ID" "$start_name" "$end_name" "$clip_prompt" known_uuids "$clip_file" \
      && CLIP_FILES+=("$clip_file") \
      || echo "    SKIP clip $c — generation failed"
  done

  [[ "${#CLIP_FILES[@]}" -eq 0 ]] && { echo "    ABORT $name — no clips"; continue; }

  # --- Stage 3: Concat + convert to animated WebP ---
  MP4_FILE="$GENERATED_DIR/${name}.mp4"
  WEBP_FILE="$GENERATED_DIR/${name}.webp"

  if [[ "${#CLIP_FILES[@]}" -eq 1 ]]; then
    cp "${CLIP_FILES[0]}" "$MP4_FILE"
  else
    FILELIST=$(mktemp)
    for cf in "${CLIP_FILES[@]}"; do echo "file '$cf'" >> "$FILELIST"; done
    ffmpeg -f concat -safe 0 -i "$FILELIST" -c copy -y "$MP4_FILE" 2>/dev/null
    rm -f "$FILELIST"
  fi

  ffmpeg -i "$MP4_FILE" \
    -vf "fps=$FLOW_GIF_FPS,scale=$FLOW_GIF_SCALE_WIDTH:-1:flags=lanczos" \
    -loop 0 -quality 80 -y "$WEBP_FILE" 2>/dev/null

  webp_size=$(stat -f%z "$WEBP_FILE" 2>/dev/null || echo "0")
  if [[ "$webp_size" -gt "${FLOW_MIN_VIDEO_BYTES:-10000}" ]]; then
    echo "    OK $name.webp ($webp_size bytes)"
    GIF_MADE=$((GIF_MADE + 1))
  else
    echo "    FAIL $name.webp (only $webp_size bytes)"; rm -f "$WEBP_FILE"
  fi
  for cf in "${CLIP_FILES[@]}"; do rm -f "$cf"; done
done

echo ""
echo "==> Results: $GIF_MADE animations created"
ls -la "$GENERATED_DIR"/*.webp 2>/dev/null | awk '{printf "    %-50s %s bytes\n", $NF, $5}' || echo "    (none)"
