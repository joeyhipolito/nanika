#!/usr/bin/env bash
# flow-generate.sh — Flow image generation + download + blog placement
# Usage: flow-generate.sh <slug> [type] [project-id] [--style name]
#   type: log | newsletter (empty = notes, skips placement). Default style: kurzgesagt

set -euo pipefail

LIB_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=flow-lib.sh
source "$LIB_DIR/flow-lib.sh"

STYLE="kurzgesagt"
_pos=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --style) STYLE="${2:?--style requires a value}"; shift 2 ;;
    --*)     echo "ERROR: unknown flag $1" >&2; exit 1 ;;
    *)       _pos+=("$1"); shift ;;
  esac
done

SLUG="${_pos[0]:?Usage: flow-generate.sh <slug> [type] [project-id] [--style name]}"
TYPE="${_pos[1]:-}"
EXPLICIT_PROJECT="${_pos[2]:-}"

flow_load_config "$STYLE" "flow_project_id"
command -v jq >/dev/null 2>&1 || { echo "ERROR: jq required (brew install jq)" >&2; exit 1; }

ILLUSTRATE_DIR="/tmp/illustrate/$SLUG"
GENERATED_DIR="$ILLUSTRATE_DIR/generated"
BLOG_DIR="${TYPE:+$HOME/dev/personal/joeyhipolito.dev/public/$TYPE/$SLUG}"
MAP_FILE="/tmp/flow-map-${SLUG}.txt"

mkdir -p "$GENERATED_DIR"
[[ -n "$BLOG_DIR" ]] && mkdir -p "$BLOG_DIR"

# Per-slug project to avoid cross-contamination from shared projects
flow_slug_project "$ILLUSTRATE_DIR" ".flow-project-id" "$EXPLICIT_PROJECT"

# Track uploaded refs so the same image isn't re-uploaded across folders
flow_init_ref_tracking "flow-generate" "$SLUG" "$PROJECT_ID"

echo "==> Opening Flow project $PROJECT_ID"
agent-browser open "https://labs.google/fx/tools/flow/project/$PROJECT_ID" >/dev/null
sleep 3

# Dismiss any consent/announcement modals blocking the project UI
dismiss_modals

BASELINE=$(agent-browser eval "
(async function() {
  var resp = await fetch('/fx/api/trpc/flow.projectInitialData?input=' +
    encodeURIComponent(JSON.stringify({ json: { projectId: '$PROJECT_ID' } })));
  var data = await resp.json();
  var media = data.result.data.json.projectContents.media;
  var count = 0;
  media.forEach(function(m) { if (m.image && m.image.generatedImage) count++; });
  return String(count);
})()
" 2>/dev/null | tr -d '"' || echo "0")
BASELINE="${BASELINE//[!0-9]/}"; BASELINE="${BASELINE:-0}"
echo "    Baseline: $BASELINE existing generated images"

setup_image_mode
PROMPT_COUNT=0

# Validate and auto-populate refs before submitting any prompts.
# Any folder with fewer than 3 ref files gets refs copied in from the registry.
for folder in "$ILLUSTRATE_DIR"/*/; do
  [[ -d "$folder" && -f "$folder/prompt.md" ]] || continue
  flow_auto_populate_refs "$folder" || true
done

for folder in "$ILLUSTRATE_DIR"/*/; do
  [[ -d "$folder" && -f "$folder/prompt.md" ]] || continue
  name=$(basename "$folder")
  echo "==> Submitting: $name"
  # Clear contenteditable
  agent-browser eval "
    var boxes = document.querySelectorAll('div[contenteditable=\"true\"]');
    var box = boxes[boxes.length - 1];
    if (box) { box.click(); box.focus(); document.execCommand('selectAll'); document.execCommand('delete'); }
  " >/dev/null 2>&1
  sleep 0.3
  # Attach or upload refs (first encounter uploads, subsequent attach from library)
  if ! flow_attach_or_upload_refs "$folder"; then
    echo "    SKIP — refs not fully attached"
    continue
  fi
  submit_image "$folder/prompt.md"
  PROMPT_COUNT=$((PROMPT_COUNT + 1))
  echo "    Submitted ($PROMPT_COUNT)"
done

[[ "$PROMPT_COUNT" -eq 0 ]] && { echo "ERROR: No illustration folders in $ILLUSTRATE_DIR" >&2; exit 1; }

TARGET=$((BASELINE + PROMPT_COUNT))
echo "==> Waiting for $TARGET total generated images..."
flow_poll_generated_images_api "$PROJECT_ID" "$TARGET" "$FLOW_IMAGE_TIMEOUT" || echo "WARNING: timed out — continuing"
sleep 5

echo "==> Fetching project media..."
VALID_TAGS=""
for folder in "$ILLUSTRATE_DIR"/*/; do
  [[ -d "$folder" && -f "$folder/prompt.md" ]] && VALID_TAGS="${VALID_TAGS}$(basename "$folder"),"
done
echo "    Valid tags: $VALID_TAGS"

VALID_TAGS_JS=$(echo "$VALID_TAGS" | sed 's/,$//')
agent-browser eval "
(async function() {
  var validTags = '${VALID_TAGS_JS}'.split(',').filter(function(t) { return t.length > 0; });
  var resp = await fetch('/fx/api/trpc/flow.projectInitialData?input=' +
    encodeURIComponent(JSON.stringify({ json: { projectId: '$PROJECT_ID' } })));
  var data = await resp.json();
  var media = data.result.data.json.projectContents.media;
  var lines = media.filter(function(m) {
    return m.image && m.image.generatedImage;
  }).map(function(m) {
    var gi = m.image.generatedImage;
    var prompt = (gi && typeof gi === 'object' ? gi.prompt : null)
      || (typeof gi === 'string' ? gi : '')
      || m.image.prompt || '';
    var match = prompt.match(/\*\*Illustration:\*\*\s*(\S+)/);
    return (match ? match[1] : 'unknown') + '|' + m.name;
  }).filter(function(l) { return validTags.indexOf(l.split('|')[0]) >= 0; });
  return lines.join('PIPE_SEP');
})()
" 2>/dev/null | tr -d '"' | sed 's/PIPE_SEP/\n/g' > "$MAP_FILE"

echo "==> Illustration mapping:"
while IFS='|' read -r tag uuid; do
  [[ -z "$tag" ]] && continue
  echo "    $tag -> $uuid"
done < "$MAP_FILE"

echo "==> Downloading images..."
declare -A TAG_TO_UUID
while IFS='|' read -r tag uuid; do
  [[ -z "$tag" || "$tag" == "unknown" ]] && continue
  TAG_TO_UUID[$tag]="$uuid"
done < "$MAP_FILE"

for tag in "${!TAG_TO_UUID[@]}"; do
  echo "    $tag..."
  download_image "${TAG_TO_UUID[$tag]}" "$GENERATED_DIR/${tag}.png" || true
done

PLACED=0
if [[ -n "$TYPE" ]]; then
  echo "==> Placing images in $BLOG_DIR"
  for f in "$GENERATED_DIR"/*.png; do
    [[ -f "$f" ]] || continue
    blog_name=$(basename "$f" .png | sed 's/^[0-9]*-//')
    /bin/cp -f "$f" "$BLOG_DIR/${blog_name}.png"
    echo "    Placed ${blog_name}.png"
    PLACED=$((PLACED + 1))
  done
else
  echo "==> Skipping placement (no TYPE)"
fi

echo ""
echo "==> Results:"
echo "    Generated: $(ls "$GENERATED_DIR"/*.png 2>/dev/null | wc -l | tr -d ' ') images"
if [[ -n "$TYPE" ]]; then
  echo "    Placed: $PLACED in $BLOG_DIR"
  ARTICLE=$(find "$HOME/dev/personal/joeyhipolito.dev/content/$TYPE" -name "*${SLUG}*" -type f 2>/dev/null | head -1)
  if [[ -n "$ARTICLE" ]]; then
    FIGURE_COUNT=$(grep -c '<Figure' "$ARTICLE" 2>/dev/null || :)
    FIGURE_COUNT="${FIGURE_COUNT//[!0-9]/}"; FIGURE_COUNT="${FIGURE_COUNT:-0}"
    echo "    Article: $ARTICLE ($FIGURE_COUNT <Figure> tags)"
    [[ "$PLACED" -ge "$FIGURE_COUNT" ]] && echo "    STATUS: PASS" || echo "    STATUS: INCOMPLETE ($PLACED/$FIGURE_COUNT)"
  fi
fi
