#!/usr/bin/env bash
#
# generate-agents-md.sh - Generate Nanika skills routing index
#
# Scans .claude/skills/ for SKILL.md files (symlinks or real dirs).
#
# Extracts descriptions + commands from each SKILL.md and generates:
#   - CLAUDE.md  — injects compact routing table only (no Domain Detection,
#                  no Orchestration Triggers — workers don't need them)
#   - AGENTS.md  — full content including domain/trigger sections (human ref)
#
# Usage:
#   ./scripts/generate-agents-md.sh              # Generate + inject
#   ./scripts/generate-agents-md.sh --dry-run    # Print routing table only
#   ./scripts/generate-agents-md.sh --diff       # Show diff vs current CLAUDE.md
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DOTCLAUDE_SKILLS_DIR="$ROOT_DIR/.claude/skills"
PLUGINS_DIR="$ROOT_DIR/plugins"
CLAUDE_MD="$ROOT_DIR/CLAUDE.md"
AGENTS_MD="$ROOT_DIR/AGENTS.md"

START_MARKER="<!-- NANIKA-AGENTS-MD-START -->"
END_MARKER="<!-- NANIKA-AGENTS-MD-END -->"

# Max commands to extract per skill
MAX_COMMANDS=14

# --- Helpers ---

die() { echo "error: $1" >&2; exit 1; }

# Extract frontmatter field from SKILL.md
# Handles plain values, YAML block scalars (> and |), and quoted strings (' and ")
extract_field() {
    local file="$1"
    local field="$2"

    local value
    value=$(awk '
        /^---$/ { flag=!flag; if(!flag) exit; next }
        flag && /^'"$field"':/ {
            # Strip "field:" prefix
            sub(/^'"$field"': */, "")
            val = $0

            # Case 1: block scalar (>, >-, |, |-) — read indented continuation lines
            if (val ~ /^[>|]-? *$/) {
                val = ""
                while ((getline) > 0) {
                    if ($0 !~ /^[ \t]/) break   # stop at non-indented line
                    line = $0
                    sub(/^[ \t]+/, "", line)     # strip leading whitespace
                    if (val != "") val = val " "
                    val = val line
                }
            }

            # Case 2: quoted string — strip outer quotes
            if (val ~ /^'\''[^'\'']*'\''$/) {
                val = substr(val, 2, length(val)-2)
            } else if (val ~ /^"[^"]*"$/) {
                val = substr(val, 2, length(val)-2)
            }

            print val
            exit
        }
    ' "$file")

    echo "$value"
}

extract_description() {
    local raw
    raw="$(extract_field "$1" "description")"
    # Trim "Use when..." suffix and truncate at first sentence for compact routing
    echo "$raw" \
        | sed 's/\. *Use when.*//' \
        | sed 's/\. .*//'
}

extract_name() { extract_field "$1" "name"; }

# Extract commands from bash code blocks.
# First tries ## Commands section; falls back to any bash block in the file.
extract_commands() {
    local file="$1"
    local toolname="$2"

    # Try ## Commands section first
    local cmds
    cmds=$(awk '
        /^## Commands/{found=1; next}
        found && /^## [^#]/{exit}
        found && /^```bash/{inblock=1; next}
        found && /^```/{inblock=0; next}
        found && inblock && /^[a-z]/' "$file" \
    | grep "^${toolname}[ $]" \
    | sed 's/\\$//' \
    | sed 's/ *$//' \
    | _strip_inline_comments \
    | head -n "$MAX_COMMANDS")

    # Fall back to any bash block if ## Commands yielded nothing
    if [ -z "$cmds" ]; then
        cmds=$(awk '
            /^```bash/{inblock=1; next}
            /^```/{inblock=0; next}
            inblock && /^[a-z]/' "$file" \
        | grep "^${toolname}[ $]" \
        | sed 's/\\$//' \
        | sed 's/ *$//' \
        | _strip_inline_comments \
        | head -n "$MAX_COMMANDS")
    fi

    echo "$cmds"
}

# Strip trailing # comments that are not inside quotes
_strip_inline_comments() {
    awk '{
        in_sq=0; in_dq=0
        for(i=1;i<=length($0);i++){
            c=substr($0,i,1)
            if(c=="\"" && !in_sq) in_dq=!in_dq
            else if(c=="\x27" && !in_dq) in_sq=!in_sq
            else if(c=="#" && !in_sq && !in_dq){
                j=i-1
                while(j>0 && substr($0,j,1)==" ") j--
                print substr($0,1,j)
                next
            }
        }
        print
    }'
}

# Build one routing line for a skill
# Usage: build_skill_line <skill_name> <description> <skill_rel_path> <SKILL.md_path> [fallback_tool_name]
# Header-only — command tails stripped after eval found zero worker hits in 992 output.md files.
build_skill_line() {
    local name="$1"
    local description="$2"
    local skill_rel="$3"
    # $4 (skill_file) and $5 (fallback) accepted for caller compatibility, unused.
    echo "|${name} — ${description}:{${skill_rel}}|"
}

# --- Main generation ---

# generate_routing: compact skills table only (injected into CLAUDE.md for workers)
generate_routing() {
    local output=""
    output+="$START_MARKER"$'\n'
    output+="[Nanika Skills Index][root: .claude/skills]IMPORTANT: Prefer retrieval-led reasoning over pre-training-led reasoning. Read skill files before making assumptions."$'\n'

    # Scan .claude/skills/ for SKILL.md files
    if [ -d "$DOTCLAUDE_SKILLS_DIR" ]; then
        for dir in "$DOTCLAUDE_SKILLS_DIR"/*/; do
            [ -d "$dir" ] || continue
            local skill_name
            skill_name="$(basename "$dir")"
            # Skip hidden dirs (.templates etc)
            [[ "$skill_name" == .* ]] && continue

            local skill_file="${dir}SKILL.md"
            [ -f "$skill_file" ] || [ -L "$skill_file" ] || continue

            local description name skill_rel
            description="$(extract_description "$skill_file")"
            name="$(extract_name "$skill_file")"
            [ -z "$name" ] && name="$skill_name"
            skill_rel=".claude/skills/${skill_name}/SKILL.md"

            output+=$'\n'
            output+="$(build_skill_line "$name" "$description" "$skill_rel" "$skill_file" "$skill_name")"
        done
    fi

    # Scan plugins/*/skills/ for SKILL.md files
    if [ -d "$PLUGINS_DIR" ]; then
        for plugin_dir in "$PLUGINS_DIR"/*/; do
            [ -d "$plugin_dir" ] || continue
            local plugin_name
            plugin_name="$(basename "$plugin_dir")"
            [[ "$plugin_name" == .* ]] && continue

            local skill_dir="${plugin_dir}skills"
            [ -d "$skill_dir" ] || continue

            local skill_file="${skill_dir}/SKILL.md"
            [ -f "$skill_file" ] || continue

            local description name skill_rel
            description="$(extract_description "$skill_file")"
            name="$(extract_name "$skill_file")"
            [ -z "$name" ] && name="$plugin_name"
            skill_rel="plugins/${plugin_name}/skills/SKILL.md"

            output+=$'\n'
            output+="$(build_skill_line "$name" "$description" "$skill_rel" "$skill_file" "$plugin_name")"
        done
    fi

    output+=$'\n'
    output+="$END_MARKER"
    echo "$output"
}

# generate_agents_md: full content for AGENTS.md (includes domain/trigger sections for humans)
generate_agents_md() {
    local routing
    routing="$(generate_routing)"

    # Strip the END marker, append human-readable sections, re-add END marker
    local body="${routing%"$END_MARKER"}"

    body+=$'\n'
    body+="[Domain Detection]|dev:{personal-project,side-project,learning}|personal:{budget,journal,travel,email,calendar}|work:{work,company,corporate,production}|creative:{content,video,youtube,blog,social,diagram,screenshot,illustration}|academic:{research,paper,thesis,academic}|"
    body+=$'\n'$'\n'
    body+="[Orchestration Triggers]|complex,multi-step,plan-and-execute,research-and-write,multiple-agents:{invoke: orchestrator run}|"
    body+=$'\n'$'\n'
    body+="[Structure]|skills:{.claude/skills}|docs:{./docs}|"
    body+=$'\n'
    body+="$END_MARKER"

    echo "$body"
}

# Inject routing table into CLAUDE.md (idempotent)
inject_claude_md() {
    local block="$1"

    if grep -q "$START_MARKER" "$CLAUDE_MD" 2>/dev/null; then
        local block_file
        block_file="$(mktemp)"
        echo "$block" > "$block_file"

        awk -v start="$START_MARKER" -v end="$END_MARKER" -v bfile="$block_file" '
            $0 == start { while((getline line < bfile) > 0) print line; skip=1; next }
            $0 == end   { skip=0; next }
            !skip        { print }
        ' "$CLAUDE_MD" > "${CLAUDE_MD}.tmp"
        mv "${CLAUDE_MD}.tmp" "$CLAUDE_MD"
        rm -f "$block_file"
    else
        printf '\n%s\n' "$block" >> "$CLAUDE_MD"
    fi
}

# --- CLI ---

main() {
    local mode="generate"

    for arg in "$@"; do
        case "$arg" in
            --dry-run) mode="dry-run" ;;
            --diff)    mode="diff" ;;
            --help|-h)
                echo "Usage: $0 [--dry-run|--diff|--help]"
                echo ""
                echo "  --dry-run   Print routing table without writing"
                echo "  --diff      Show diff of what would change in CLAUDE.md"
                echo "  --help      Show this help"
                exit 0
                ;;
            *) die "Unknown flag: $arg" ;;
        esac
    done

    case "$mode" in
        dry-run)
            generate_routing
            ;;
        diff)
            local tmp
            tmp="$(mktemp)"
            cp "$CLAUDE_MD" "$tmp"
            inject_claude_md "$(generate_routing)"
            diff -u "$tmp" "$CLAUDE_MD" || true
            cp "$tmp" "$CLAUDE_MD"
            rm -f "$tmp"
            ;;
        generate)
            inject_claude_md "$(generate_routing)"
            generate_agents_md > "$AGENTS_MD"
            echo "✓ CLAUDE.md updated (routing table only)"
            echo "✓ AGENTS.md updated (full index + domain/trigger sections)"
            ;;
    esac
}

main "$@"
