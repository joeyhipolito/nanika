#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEFAULT="$REPO_ROOT/personas/default.md"
NOTES="$REPO_ROOT/personas/joey-notes.md"
VOICE="$REPO_ROOT/.claude/skills/article/voice.md"

TMPDIR_=$(mktemp -d)
trap 'rm -rf "$TMPDIR_"' EXIT

# Extract a top-level (#) section from a file.
# Prints from the matching heading line until the next top-level heading or
# bare --- separator, then strips trailing blank lines.
extract_section() {
    local file="$1"
    local heading="$2"
    awk -v h="$heading" '
        $0 == h                  { found=1; print; next }
        found && /^#/            { exit }
        found && /^---/          { exit }
        found && /<!-- mirror-of:/ { next }
        found                    { print }
    ' "$file" \
    | awk '
        { lines[NR] = $0 }
        END {
            n = NR
            while (n > 0 && lines[n] ~ /^[[:space:]]*$/) n--
            for (i = 1; i <= n; i++) print lines[i]
        }
    '
}

SECTIONS=("# Word Choices" "# Sentence Style" "# What Joey Never Does")
SLUGS=("word-choices" "sentence-style" "what-joey-never-does")

drift=0

for i in "${!SECTIONS[@]}"; do
    section="${SECTIONS[$i]}"
    slug="${SLUGS[$i]}"

    extract_section "$DEFAULT" "$section" > "$TMPDIR_/default-${slug}.txt"
    extract_section "$NOTES"   "$section" > "$TMPDIR_/notes-${slug}.txt"
    extract_section "$VOICE"   "$section" > "$TMPDIR_/voice-${slug}.txt"

    for other in notes voice; do
        case "$other" in
            notes) label="personas/joey-notes.md" ;;
            voice) label=".claude/skills/article/voice.md" ;;
        esac

        if ! diff -q "$TMPDIR_/default-${slug}.txt" "$TMPDIR_/${other}-${slug}.txt" > /dev/null 2>&1; then
            drift=1
            echo "DRIFT: '$section' in $label differs from personas/default.md (source of truth)"
            diff -u \
                --label "personas/default.md  ($section)" \
                --label "$label  ($section)" \
                "$TMPDIR_/default-${slug}.txt" \
                "$TMPDIR_/${other}-${slug}.txt" || true
            echo
        fi
    done
done

if [ "$drift" -eq 0 ]; then
    echo "OK: Word Choices, Sentence Style, and What Joey Never Does match across all three files"
    echo "hint: Word Choices / Sentence Style / What Joey Never Does are mirrored across personas/default.md (source), personas/joey-notes.md, .claude/skills/article/voice.md"
fi

exit "$drift"
