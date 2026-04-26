#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SKILL_MD="${REPO_ROOT}/.claude/skills/article/SKILL.md"

# Find the line referencing voice.md
MATCH_LINE=$(grep -n 'voice\.md' "$SKILL_MD" | head -1)

if [[ -z "$MATCH_LINE" ]]; then
  echo "ERROR: No voice.md reference found in ${SKILL_MD}" >&2
  exit 1
fi

LINE_NUM=$(echo "$MATCH_LINE" | cut -d: -f1)
LINE_CONTENT=$(echo "$MATCH_LINE" | cut -d: -f2-)

# Extract the path expression — expect ${CLAUDE_SKILL_DIR}/voice.md
# CLAUDE_SKILL_DIR resolves to the directory containing the SKILL.md file
CLAUDE_SKILL_DIR="$(dirname "$SKILL_MD")"
RESOLVED_PATH="${CLAUDE_SKILL_DIR}/voice.md"

if [[ -f "$RESOLVED_PATH" ]]; then
  echo "OK: voice.md exists at ${RESOLVED_PATH}"
  exit 0
else
  echo "ERROR: voice.md reference found on line ${LINE_NUM} of ${SKILL_MD}:" >&2
  echo "  ${LINE_CONTENT}" >&2
  echo "Resolved path does not exist: ${RESOLVED_PATH}" >&2
  exit 1
fi
