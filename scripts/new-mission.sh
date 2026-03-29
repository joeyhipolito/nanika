#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <slug>" >&2
  exit 1
fi

slug="$1"
date_prefix="$(date +%F)"
missions_dir="${HOME}/.alluka/missions"
path="${missions_dir}/${date_prefix}-${slug}.md"

mkdir -p "$missions_dir"

if [[ -e "$path" ]]; then
  echo "mission already exists: $path" >&2
  exit 1
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
template="${script_dir}/../templates/mission.md"

if [[ -f "$template" ]]; then
  cp "$template" "$path"
else
  cat >"$path" <<'EOF'
---
linear_issue_id:
target:
status: active
---

# Mission: Title

## Objective

Describe the outcome and constraints.
EOF
fi

echo "$path"
