#!/usr/bin/env bash
# Window-1 soak report — runs 48h after the Window-1 cleanup merge
# (2026-04-24 3d7973ac) to capture the post-merge metric delta and flag
# any reviewer regressions. Read-only: no mission launches, no mutations.
#
# Reads:
#   - ~/nanika/scripts/experiment-snapshot.sh
#   - ~/.alluka/workspaces/*/checkpoint.json
#   - tracker CLI (local SQLite)
#
# Writes:
#   - ~/nanika/shared/artifacts/window-1-soak-48h.md
#   - macOS notification (best-effort)
#
# Self-disables the scheduler job after successful completion.

set -euo pipefail

REPORT="$HOME/nanika/shared/artifacts/window-1-soak-48h.md"
SNAPSHOT_SCRIPT="$HOME/nanika/scripts/experiment-snapshot.sh"
mkdir -p "$(dirname "$REPORT")"

SNAPSHOT=$(bash "$SNAPSHOT_SCRIPT" 2>&1 || true)

fail_count=0
fail_lines=""
for ws in "$HOME"/.alluka/workspaces/20260424-*/ \
          "$HOME"/.alluka/workspaces/20260425-*/ \
          "$HOME"/.alluka/workspaces/20260426-*/; do
  [[ -d "$ws" ]] || continue
  mission_id=$(basename "$ws")
  [[ "$mission_id" == "20260424-83604cd4" ]] && continue
  [[ -f "$ws/checkpoint.json" ]] || continue

  fails=$(python3 - "$ws/checkpoint.json" <<'PY'
import json, sys
try:
    d = json.load(open(sys.argv[1]))
    for ph in d.get('payload', {}).get('plan', {}).get('phases', []):
        name = ph.get('name', '') or ''
        persona = ph.get('persona', '') or ''
        status = ph.get('status', '') or ''
        is_review = ('staff-code-reviewer' in persona) or (name in ('review', 're-review', 'verify'))
        if status == 'failed' and is_review:
            err = (ph.get('error') or '').splitlines()[0] if ph.get('error') else ''
            print(f"{ph.get('id', '?')}|{name}|{err[:160]}")
except Exception:
    pass
PY
) || true

  while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    IFS='|' read -r pid pname perr <<< "$line"
    fail_lines="${fail_lines}- \`$mission_id\` $pid/$pname — \`$perr\`\n"
    fail_count=$((fail_count + 1))
  done <<< "$fails"
done

tracker_lines=""
for trk in TRK-490 TRK-612 TRK-616 TRK-617 TRK-618; do
  status=$(tracker show "$trk" 2>/dev/null | grep "^Status:" | head -1 | awk '{print $2}' || echo "?")
  tracker_lines="${tracker_lines}- $trk: $status\n"
done

generated=$(date -u +%Y-%m-%dT%H:%M:%SZ)

{
  echo "# Window-1 Soak — 48h Report"
  echo
  echo "**Generated**: $generated UTC"
  echo "**Window-1 merge**: 2026-04-24, commit 3d7973ac"
  echo "**Scope**: missions in \`~/.alluka/workspaces/\` dated 2026-04-24 through 2026-04-26 (excluding the cleanup mission \`20260424-83604cd4\`)"
  echo
  echo "## Snapshot output"
  echo
  echo '```'
  echo "$SNAPSHOT"
  echo '```'
  echo
  echo "## Reviewer-phase failures since Window-1 merge"
  echo
  if (( fail_count == 0 )); then
    echo "**0 failures** — all \`staff-code-reviewer\` review/re-review/verify phases green since the merge. TRK-490/612/616/617/618 fixes held."
  else
    echo "**$fail_count failures** surfaced below. Classify each:"
    echo "- TRK-612/616 regression → filename shape or preamble-parser miss; check workspace \`workers/staff-code-reviewer-*/review.md\`"
    echo "- TRK-617 regression → error mentions \`npm run build\` or missing script on bun/pnpm/yarn targets"
    echo "- TRK-618 regression → error mentions \`unbalanced --- markers\` on artifacts with body horizontal rules"
    echo "- TRK-490 regression → error mentions \`codex review\` exec-only flags"
    echo "- Legitimate fail → reviewer correctly caught a real bug in the implementation; not a review-loop bug"
    echo
    echo -e "$fail_lines"
  fi
  echo
  echo "## Tracker status (all should be \`done\`)"
  echo
  echo -e "$tracker_lines"
  echo
  echo "## Baseline for comparison (Window-1 close, 2026-04-23)"
  echo
  echo "| Metric | Baseline | Target | Current |"
  echo "|---|---|---|---|"
  echo "| Retries-avg | 0.11 | ≤0.05 | see snapshot above |"
  echo "| \$/phase | \$1.20 | ≤\$0.85 | see snapshot above |"
  echo "| \$/mission | \$5.62 | ≤\$4.00 | see snapshot above |"
  echo "| Gate-pass | 99.65% | ≥99.9% | see snapshot above |"
  echo "| Cache-read | 94.79% | stable (~94%) | see snapshot above |"
  echo
  echo "## Assessment heuristic"
  echo
  echo "- **Green** (retries-avg ≤ 0.05 AND failures = 0): fixes held. Window-2 planning unlocked."
  echo "- **Yellow** (retries-avg 0.05–0.08 OR 1–2 failures): partial. Investigate each failure; most likely a new edge case — consider whether it warrants a 619-series tracker."
  echo "- **Red** (retries-avg > 0.08 OR ≥3 failures): regression hypothesis. Inspect the workspace tails; at least one of the Window-1 fixes is not holding under the current workload shape. Do NOT plan Window-2 yet — file a follow-up tracker and cauterize first."
  echo
  echo "## Window-2 candidate (if green)"
  echo
  echo "**Cache-warm priming between sibling phases** — push 94.79% → 96%+ cache-read rate for compounded cost savings. Secondary candidates: Haiku-first research routing; persona max_turns tuning."
} > "$REPORT"

osascript -e "display notification \"Window-1 soak report at shared/artifacts/window-1-soak-48h.md — $fail_count reviewer failure(s) since merge\" with title \"Nanika Window-1 Soak\" sound name \"Glass\"" 2>/dev/null || true

if [[ "${1:-}" == "--scheduled" ]]; then
  self_id=$(scheduler jobs 2>/dev/null | awk '/window-1-soak-48h/ {print $1; exit}' || true)
  if [[ -n "${self_id:-}" ]]; then
    scheduler jobs disable "$self_id" >/dev/null 2>&1 || true
    echo "self-disabled scheduler job $self_id"
  fi
fi

echo "wrote: $REPORT"
