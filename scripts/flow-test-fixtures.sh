#!/usr/bin/env bash
set -euo pipefail

# flow-test-fixtures.sh — Generate test folder structures for flow scripts
#
# Usage:
#   flow-test-fixtures.sh generate <slug> [--style kurzgesagt|claudius|pixel-art]
#   flow-test-fixtures.sh gif <slug> [--style claudius] [--gifs 1] [--keyframes 3]
#   flow-test-fixtures.sh video <slug> [--style claudius] [--minutes 1] [--keyframes 10]
#   flow-test-fixtures.sh all <slug> [--style kurzgesagt]
#   flow-test-fixtures.sh clean <slug>
#   flow-test-fixtures.sh list

ILLUSTRATE_DIR="/tmp/illustrate"

# --- Colors ---
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
NC='\033[0m'

log_info()    { echo -e "${CYAN}[fixture]${NC} $*"; }
log_success() { echo -e "${GREEN}[fixture]${NC} $*"; }
log_warn()    { echo -e "${YELLOW}[fixture]${NC} $*"; }
log_error()   { echo -e "${RED}[fixture]${NC} $*" >&2; }

usage() {
  cat <<'EOF'
flow-test-fixtures.sh — Scaffold test folders for Flow scripts

Commands:
  generate <slug>   Create article illustration folders (for flow-generate.sh)
  gif <slug>        Create GIF animation folders (for flow-gif.sh)
  video <slug>      Create video keyframe folders (for flow-video.sh)
  all <slug>        Create all three folder types
  clean <slug>      Remove test fixtures for a slug
  list              Show existing fixture slugs

Options:
  --style <name>    Visual style (kurzgesagt|claudius|pixel-art) [default: kurzgesagt]
  --gifs <n>        Number of GIF folders to create [default: 1]
  --keyframes <n>   Keyframes per GIF [default: 3]
  --minutes <n>     Video duration in minutes (determines KF count) [default: 1]
  --statics <n>     Number of static illustration folders [default: 3]

Examples:
  flow-test-fixtures.sh generate test-article --style kurzgesagt --statics 3
  flow-test-fixtures.sh gif test-article --style claudius --gifs 2 --keyframes 4
  flow-test-fixtures.sh video test-video --style claudius --minutes 2
  flow-test-fixtures.sh all test-full --style kurzgesagt
  flow-test-fixtures.sh clean test-article
EOF
  exit 0
}

# --- Sample prompts by style ---

sample_illustration_prompt() {
  local style="$1" tag="$2" _index="$3"
  local mascot="octopus developer"

  case "$style" in
    kurzgesagt)
      cat <<PROMPT
**Illustration:** ${tag}

Flat 2D vector illustration in Kurzgesagt style. Dark navy background (#0d0d1a).

Scene: A ${mascot} sits at a glowing terminal, tentacles spread across multiple keyboards. The screen displays a sprawling dependency graph with coral (#f47c3f) and teal (#4ecdc4) nodes connected by glowing curved lines. Small data particles float upward from the screen.

The ${mascot}'s skin shifts to a warm coral hue as it discovers a critical insight. Two tentacles hold coffee mugs. The workspace is cluttered with holographic sticky notes and floating code snippets.

Style: Flat shapes, subtle depth via soft glow gradients on nodes. No 3D rendering. No photorealism. Clean vector edges with occasional glow effects on interactive elements.
PROMPT
      ;;
    claudius)
      cat <<PROMPT
**Illustration:** ${tag}

Warm editorial illustration in ClaudiusPapirus style. Cream parchment background (#F0DFC0).

Scene: A small burnt-orange ${mascot} (#C05C28) sits at a wooden desk, dwarfed by towering stacks of leather-bound technical manuals. One tentacle holds a magnifying glass over a page of code. The golden desk lamp (#D4A84B) casts warm light across the scene.

The ${mascot} looks determined but slightly overwhelmed — classic scale contrast showing the challenge is bigger than the protagonist. Organic cream-colored blob shapes float in the background. Dark ink outlines (#1A1A2E) on all elements.

Style: Warm, personal, editorial. Negative space ~25%. NOT dark/space themed. NOT high-saturation neon.
PROMPT
      ;;
    pixel-art)
      cat <<PROMPT
**Illustration:** ${tag}

Strict 8-bit pixel art illustration. Dark background (#1a1c2c).

Scene: A pixel ${mascot} sprite (16x16 base, scaled up) sits at a chunky CRT monitor. The screen glows yellow (#faef5d) with scrolling green text. Two tentacle sprites rest on a blocky keyboard. A red (#ff004d) exclamation mark floats above.

Max 16 colors. Hard block edges. No anti-aliasing. No gradients. No rounded corners. Orthogonal pixel-perfect connection lines between UI elements. Retro game aesthetic.

Format: 1080x1080.
PROMPT
      ;;
  esac
}

sample_gif_keyframe_prompt() {
  local style="$1" kf_index="$2" total_kfs="$3" gif_name="$4"
  local mascot="octopus developer"
  # Script uses 1-based kf_num: tag="${gif_name}-kf-${kf_num}"
  local kf_num
  kf_num=$(printf '%02d' $((kf_index + 1)))
  local tag="${gif_name}-kf-${kf_num}"

  local scene
  case "$kf_index" in
    0) scene="A ${mascot} sits calmly at a desk, tentacles resting on keyboard. Eyes focused on screen. Coffee mug steaming in one tentacle. Workspace is tidy and organized. The mood is peaceful concentration." ;;
    1) scene="The same ${mascot} leans forward urgently. Eyes wide. Three tentacles now typing furiously. Coffee mug tipping. Screen shows a red error message. Papers scattered. The mood shifts to alarm — something broke." ;;
    *) scene="The ${mascot} leans back triumphantly. Eyes bright and relieved. One tentacle raised in victory. Screen shows green checkmarks. Coffee mug safely set down. Papers still scattered but the crisis is over. The mood is relief and accomplishment." ;;
  esac

  cat <<PROMPT
${tag}

${scene}
PROMPT
}

sample_gif_motion_prompt() {
  cat <<'PROMPT'
Smooth slow animation. The octopus character moves naturally with fluid tentacle motion. Subtle screen glow flickers. Coffee steam wisps drift upward. Papers shift slightly. Camera holds steady, no zoom or pan. 4 seconds.
PROMPT
}

sample_video_keyframe_prompt() {
  local kf_index="$1" total_kfs="$2"
  local mascot="octopus developer"
  local padded
  padded=$(printf '%02d' "$kf_index")

  cat <<PROMPT
**Illustration:** KF-${padded}

Keyframe ${kf_index} of ${total_kfs}. A ${mascot} in a scene that progresses the narrative. Frame ${kf_index}: the ${mascot} is at stage $((kf_index * 100 / total_kfs))% of its journey through the topic.

Maintain consistent character design, lighting, and color palette across all keyframes. The environment evolves gradually — each frame should feel like a natural continuation of the previous one.
PROMPT
}

sample_veo_prompt() {
  local clip_index="$1" total_clips="$2"
  echo "Smooth cinematic transition. The octopus character moves fluidly between keyframe states. Subtle environmental changes — lighting shifts, particles drift, screen content updates. Camera holds steady with gentle parallax on background layers. Clip ${clip_index} of ${total_clips}. 8 seconds."
}

# --- Fixture generators ---

generate_static_fixtures() {
  local slug="$1" style="$2" count="$3"
  local base_dir="${ILLUSTRATE_DIR}/${slug}"

  mkdir -p "$base_dir"

  local tags=("hero" "diagram" "comparison" "workflow" "architecture" "conclusion" "sidebar" "metrics")

  for i in $(seq 1 "$count"); do
    local tag="${tags[$((i - 1))]}"
    local folder_name
    folder_name=$(printf '%03d-%s' "$i" "$tag")
    local folder_path="${base_dir}/${folder_name}"

    mkdir -p "$folder_path"
    sample_illustration_prompt "$style" "$folder_name" "$i" > "${folder_path}/prompt.md"

    # Add a reference mascot file (tiny placeholder)
    printf 'PLACEHOLDER_MASCOT_IMAGE' > "${folder_path}/mascot.jpeg"

    log_info "  ${folder_name}/prompt.md (${style} style)"
  done

  log_success "Static fixtures: ${base_dir}/ (${count} folders)"
}

generate_gif_fixtures() {
  local slug="$1" style="$2" gif_count="$3" kf_count="$4"
  local gifs_dir="${ILLUSTRATE_DIR}/${slug}/gifs"

  local gif_names=("loading" "error-recovery" "deploy" "thinking" "celebrating" "debugging")

  for g in $(seq 1 "$gif_count"); do
    local gif_name="${gif_names[$((g - 1))]}"
    local gif_dir="${gifs_dir}/gif-${gif_name}"

    mkdir -p "$gif_dir"

    # Keyframe prompts (kf-00, kf-01, ..., kf-NN)
    for k in $(seq 0 $((kf_count - 1))); do
      local kf_file
      kf_file=$(printf 'kf-%02d-prompt.md' "$k")
      sample_gif_keyframe_prompt "$style" "$k" "$kf_count" "gif-${gif_name}" > "${gif_dir}/${kf_file}"
    done

    # Motion prompt
    sample_gif_motion_prompt > "${gif_dir}/prompt.txt"

    # Optional per-clip overrides for first clip
    local clip_count=$((kf_count - 1))
    if [ "$clip_count" -gt 1 ]; then
      echo "Fast energetic motion for the error moment. Screen flickers red. Tentacles scramble." > "${gif_dir}/clip-01-prompt.txt"
    fi

    log_info "  gif-${gif_name}/ (${kf_count} keyframes, ${clip_count} clips)"
  done

  log_success "GIF fixtures: ${gifs_dir}/ (${gif_count} gifs)"
}

generate_video_fixtures() {
  local slug="$1" style="$2" minutes="$3"
  local base_dir="${ILLUSTRATE_DIR}/${slug}"
  local kf_dir="${base_dir}/keyframes"

  local clip_count
  clip_count=$(echo "$minutes * 60 / 8" | bc)
  local kf_count=$((clip_count + 1))

  mkdir -p "$kf_dir"
  mkdir -p "${base_dir}/clips"

  # Keyframe folders with prompts
  for i in $(seq 1 "$kf_count"); do
    local padded
    padded=$(printf '%02d' "$i")
    local folder="${kf_dir}/KF-${padded}"

    mkdir -p "$folder"
    sample_video_keyframe_prompt "$i" "$kf_count" > "${folder}/prompt.md"

    # Add mascot reference to first KF
    if [ "$i" -eq 1 ]; then
      printf 'PLACEHOLDER_MASCOT_IMAGE' > "${folder}/mascot.jpeg"
    fi
  done

  # veo-prompts-clean.txt with --- delimiters
  local veo_file="${base_dir}/veo-prompts-clean.txt"
  : > "$veo_file"
  for c in $(seq 1 "$clip_count"); do
    if [ "$c" -gt 1 ]; then
      echo "---" >> "$veo_file"
    fi
    sample_veo_prompt "$c" "$clip_count" >> "$veo_file"
  done

  log_success "Video fixtures: ${base_dir}/ (${kf_count} keyframes, ${clip_count} clips, ${minutes}min)"
  log_info "  keyframes/KF-01/ through KF-$(printf '%02d' "$kf_count")/"
  log_info "  veo-prompts-clean.txt (${clip_count} prompts, --- delimited)"
}

clean_fixtures() {
  local slug="$1"
  local base_dir="${ILLUSTRATE_DIR}/${slug}"

  if [ ! -d "$base_dir" ]; then
    log_warn "No fixtures found for slug: ${slug}"
    return 0
  fi

  rm -rf "$base_dir"
  log_success "Cleaned: ${base_dir}"
}

list_fixtures() {
  if [ ! -d "$ILLUSTRATE_DIR" ]; then
    log_warn "No fixtures directory: ${ILLUSTRATE_DIR}"
    return 0
  fi

  echo "Fixture slugs in ${ILLUSTRATE_DIR}/:"
  echo ""

  for d in "${ILLUSTRATE_DIR}"/*/; do
    [ -d "$d" ] || continue
    local slug
    slug=$(basename "$d")
    local has_static="" has_gif="" has_video=""

    # Check for static illustration folders (NNN-name pattern)
    if ls "$d"/[0-9][0-9][0-9]-*/prompt.md >/dev/null 2>&1; then
      local count
      count=$(find "$d" -maxdepth 2 -name "prompt.md" -path "*/[0-9]*" 2>/dev/null | wc -l | tr -d ' ')
      has_static="${count} statics"
    fi

    # Check for GIF folders
    if [ -d "${d}gifs" ]; then
      local count
      count=$(find "${d}gifs" -maxdepth 1 -type d -name "gif-*" 2>/dev/null | wc -l | tr -d ' ')
      has_gif="${count} gifs"
    fi

    # Check for video keyframes
    if [ -d "${d}keyframes" ]; then
      local count
      count=$(find "${d}keyframes" -maxdepth 1 -type d -name "KF-*" 2>/dev/null | wc -l | tr -d ' ')
      has_video="${count} keyframes"
    fi

    local parts=()
    [ -n "$has_static" ] && parts+=("$has_static")
    [ -n "$has_gif" ] && parts+=("$has_gif")
    [ -n "$has_video" ] && parts+=("$has_video")

    if [ ${#parts[@]} -gt 0 ]; then
      local joined
      joined=$(IFS=', '; echo "${parts[*]}")
      echo "  ${slug} — ${joined}"
    else
      echo "  ${slug} — (empty)"
    fi
  done
}

# --- Main ---

COMMAND="${1:-}"
[ -z "$COMMAND" ] && usage

shift

case "$COMMAND" in
  generate|gif|video|all)
    SLUG="${1:-}"
    [ -z "$SLUG" ] && { log_error "Slug required"; usage; }
    shift
    ;;
  clean)
    SLUG="${1:-}"
    [ -z "$SLUG" ] && { log_error "Slug required"; usage; }
    clean_fixtures "$SLUG"
    exit 0
    ;;
  list)
    list_fixtures
    exit 0
    ;;
  --help|-h|help)
    usage
    ;;
  *)
    log_error "Unknown command: $COMMAND"
    usage
    ;;
esac

# Parse options
STYLE="kurzgesagt"
GIFS=1
KEYFRAMES=3
MINUTES=1
STATICS=3

while [ $# -gt 0 ]; do
  case "$1" in
    --style)    STYLE="${2:?--style requires a value}"; shift 2 ;;
    --gifs)     GIFS="${2:?--gifs requires a value}"; shift 2 ;;
    --keyframes) KEYFRAMES="${2:?--keyframes requires a value}"; shift 2 ;;
    --minutes)  MINUTES="${2:?--minutes requires a value}"; shift 2 ;;
    --statics)  STATICS="${2:?--statics requires a value}"; shift 2 ;;
    *)          log_error "Unknown option: $1"; exit 1 ;;
  esac
done

# Validate style
if [ ! -d "$HOME/.contentkit/styles/${STYLE}" ]; then
  log_warn "Style config not found: ~/.contentkit/styles/${STYLE}/config.json"
  log_warn "Fixtures will be created but flow scripts may fail without config"
fi

echo ""
log_info "Generating fixtures for slug: ${SLUG} (style: ${STYLE})"
echo ""

case "$COMMAND" in
  generate)
    generate_static_fixtures "$SLUG" "$STYLE" "$STATICS"
    echo ""
    echo "Test with:  ./scripts/flow-generate.sh ${SLUG} ${STYLE}"
    ;;
  gif)
    generate_gif_fixtures "$SLUG" "$STYLE" "$GIFS" "$KEYFRAMES"
    echo ""
    echo "Test with:  ./scripts/flow-gif.sh ${SLUG} --style ${STYLE}"
    ;;
  video)
    generate_video_fixtures "$SLUG" "$STYLE" "$MINUTES"
    echo ""
    echo "Test with:  ./scripts/flow-video.sh ${SLUG} --style ${STYLE} --minutes ${MINUTES}"
    ;;
  all)
    generate_static_fixtures "$SLUG" "$STYLE" "$STATICS"
    echo ""
    generate_gif_fixtures "$SLUG" "$STYLE" "$GIFS" "$KEYFRAMES"
    echo ""
    generate_video_fixtures "$SLUG" "$STYLE" "$MINUTES"
    echo ""
    echo "Test with:"
    echo "  ./scripts/flow-generate.sh ${SLUG} ${STYLE}"
    echo "  ./scripts/flow-gif.sh ${SLUG} --style ${STYLE}"
    echo "  ./scripts/flow-video.sh ${SLUG} --style ${STYLE} --minutes ${MINUTES}"
    ;;
esac

echo ""
log_success "Done. Run 'flow-test-fixtures.sh list' to see all fixtures."
