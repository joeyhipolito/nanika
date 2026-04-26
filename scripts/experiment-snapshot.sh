#!/usr/bin/env bash
# experiment-snapshot.sh — compare live metrics against a baseline for the active experiment window
#
# Usage:
#   scripts/experiment-snapshot.sh                    # uses active window from cutovers log
#   scripts/experiment-snapshot.sh --since 2026-04-11T03:05:00
#   scripts/experiment-snapshot.sh --json             # machine-readable output

set -euo pipefail

DB="${ALLUKA_METRICS_DB:-$HOME/.alluka/metrics.db}"
CUTOVERS="$HOME/.alluka/missions/artifacts/skills-index-cutovers.md"

# --- defaults ---
SINCE=""
JSON=false
WATCH_TARGET=100

# --- parse args ---
while [[ $# -gt 0 ]]; do
  case "$1" in
    --since)  SINCE="$2"; shift 2 ;;
    --json)   JSON=true; shift ;;
    --target) WATCH_TARGET="$2"; shift 2 ;;
    -h|--help)
      echo "Usage: experiment-snapshot.sh [--since TIMESTAMP] [--target N] [--json]"
      exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 1 ;;
  esac
done

if [[ ! -f "$DB" ]]; then
  echo "error: metrics DB not found at $DB" >&2
  exit 1
fi

# --- resolve cutover timestamp ---
if [[ -z "$SINCE" ]]; then
  if [[ -f "$CUTOVERS" ]]; then
    # grab the last row with end_ts = "-" (active window)
    SINCE=$(grep '| - |' "$CUTOVERS" 2>/dev/null | tail -1 | awk -F'|' '{gsub(/^ +| +$/,"",$2); print $2}')
  fi
  if [[ -z "$SINCE" ]]; then
    echo "error: no active window found in $CUTOVERS and no --since provided" >&2
    exit 1
  fi
fi

# --- baseline values (from skills-index-baseline-2026-04-11.md, d7 slice) ---
B_GATE=98.85
B_FAILED=1.43
B_RETRIES=0.10
B_CACHE=94.17
B_COST=1.4300
B_TOKENS=1344385
B_DURATION=364.3

# --- option C values (from skills-index-option-C-2026-04-17.md, 111-phase window) ---
# Accepted 2026-04-16. Cost deltas vs Option E are confounded by mission-mix shift
# (engage/LinkedIn-heavy vs E's Dust-protocol-heavy workload) — cost numbers here are
# directional, not causal. Quality signal (gate/failed/retries) is clean.
C_GATE=100.00
C_FAILED=0.90
C_RETRIES=0.02
C_CACHE=93.74
C_COST=0.6535
C_TOKENS=728208
C_DURATION=176.5
C_MISSION_COST=3.2973

# Revert trigger check uses Option C as the accepted baseline for the next variant.
R_GATE="$C_GATE"
R_FAILED="$C_FAILED"
R_RETRIES="$C_RETRIES"

# --- queries ---
q() { sqlite3 -separator '|' "$DB" "$1"; }

# aggregate
read -r TOTAL GATE FAILED COST TOKENS DURATION RETRIES <<< "$(q "
  SELECT COUNT(*),
    ROUND(100.0*SUM(p.gate_passed)/COUNT(*),2),
    ROUND(100.0*SUM(CASE WHEN p.status='failed' THEN 1 ELSE 0 END)/COUNT(*),2),
    ROUND(AVG(p.cost_usd),4),
    ROUND(AVG(p.tokens_in),0),
    ROUND(AVG(p.duration_s),1),
    ROUND(AVG(p.retries),2)
  FROM phases p JOIN missions m ON p.mission_id=m.id
  WHERE p.tokens_in>0 AND m.started_at >= '$SINCE'
" | tr '|' ' ')"

# cache
read -r MISSIONS CACHE_READ MISSION_COST <<< "$(q "
  SELECT COUNT(*),
    ROUND(100.0*SUM(tokens_cache_read_total)/NULLIF(SUM(tokens_in_total),0),2),
    ROUND(AVG(cost_usd_total),4)
  FROM missions WHERE tokens_in_total>0 AND started_at >= '$SINCE'
" | tr '|' ' ')"

# skill invocations
read -r SKILL_TOTAL SKILL_DECLARED SKILL_PARSED <<< "$(q "
  SELECT COUNT(*),
    SUM(CASE WHEN source='declared' THEN 1 ELSE 0 END),
    SUM(CASE WHEN source='output_parse' THEN 1 ELSE 0 END)
  FROM skill_invocations WHERE invoked_at >= '$SINCE'
" | tr '|' ' ')"

# per-persona
PERSONA_DATA=$(q "
  SELECT persona, COUNT(*),
    ROUND(100.0*SUM(p.gate_passed)/COUNT(*),1),
    ROUND(AVG(p.cost_usd),4),
    ROUND(AVG(p.retries),2)
  FROM phases p JOIN missions m ON p.mission_id=m.id
  WHERE p.tokens_in>0 AND m.started_at >= '$SINCE'
  GROUP BY persona ORDER BY COUNT(*) DESC
")

# failed phases detail
FAILED_DETAIL=$(q "
  SELECT persona, p.name, SUBSTR(m.id,1,20)
  FROM phases p JOIN missions m ON p.mission_id=m.id
  WHERE p.tokens_in>0 AND m.started_at >= '$SINCE' AND p.status='failed'
  ORDER BY m.started_at DESC
")

# revert trigger checks
check_trigger() {
  local label="$1" baseline="$2" current="$3" threshold="$4" direction="$5"
  local delta
  delta=$(echo "$current - $baseline" | bc -l)
  local tripped="no"
  if [[ "$direction" == "drop" ]]; then
    tripped=$(echo "$delta < -$threshold" | bc -l)
  else
    tripped=$(echo "$delta > $threshold" | bc -l)
  fi
  [[ "$tripped" == "1" ]] && echo "YES" || echo "no"
}

T_GATE=$(check_trigger "gate" "$R_GATE" "$GATE" 10 "drop")
T_FAILED=$(check_trigger "failed" "$R_FAILED" "$FAILED" 5 "rise")
T_RETRIES_PCT=$(echo "($RETRIES - $R_RETRIES) / $R_RETRIES * 100" | bc -l 2>/dev/null || echo "999")
T_RETRIES=$([[ $(echo "$T_RETRIES_PCT > 20" | bc -l) == "1" ]] && echo "YES" || echo "no")

PROGRESS=$(( TOTAL * 100 / WATCH_TARGET ))

# --- JSON output ---
if $JSON; then
  cat <<ENDJSON
{
  "since": "$SINCE",
  "watch_target": $WATCH_TARGET,
  "phases": $TOTAL,
  "progress_pct": $PROGRESS,
  "missions": $MISSIONS,
  "aggregate": {
    "gate_pass_pct": $GATE,
    "failed_pct": $FAILED,
    "avg_cost": $COST,
    "avg_tokens_in": $TOKENS,
    "avg_duration_s": $DURATION,
    "avg_retries": $RETRIES,
    "cache_read_pct": $CACHE_READ,
    "avg_mission_cost": $MISSION_COST
  },
  "baseline": {
    "gate_pass_pct": $B_GATE,
    "failed_pct": $B_FAILED,
    "avg_cost": $B_COST,
    "avg_tokens_in": $B_TOKENS,
    "avg_duration_s": $B_DURATION,
    "avg_retries": $B_RETRIES,
    "cache_read_pct": $B_CACHE
  },
  "option_c": {
    "gate_pass_pct": $C_GATE,
    "failed_pct": $C_FAILED,
    "avg_cost": $C_COST,
    "avg_tokens_in": $C_TOKENS,
    "avg_duration_s": $C_DURATION,
    "avg_retries": $C_RETRIES,
    "cache_read_pct": $C_CACHE,
    "avg_mission_cost": $C_MISSION_COST
  },
  "trigger_reference": "option_c",
  "triggers": {
    "gate_pass": "$T_GATE",
    "failed": "$T_FAILED",
    "retries": "$T_RETRIES"
  },
  "skills": {
    "total": $SKILL_TOTAL,
    "declared": $SKILL_DECLARED,
    "output_parse": $SKILL_PARSED
  }
}
ENDJSON
  exit 0
fi

# --- human output ---
echo "# Experiment Snapshot — $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo ""
echo "Window start: $SINCE"
echo "Watch target: $WATCH_TARGET phases"
echo "Progress:     $TOTAL / $WATCH_TARGET ($PROGRESS%)"
echo "Missions:     $MISSIONS"
echo ""

echo "## Revert Triggers  (checked against: option-C)"
echo ""
printf "%-20s %10s %10s %10s %10s %8s\n" "Metric" "Baseline" "Option-C" "Current" "Δ vs C" "Tripped"
printf "%-20s %10s %10s %10s %10s %8s\n" "--------------------" "----------" "----------" "----------" "----------" "--------"

GATE_DELTA=$(printf "%+.2f pp" "$(echo "$GATE - $R_GATE" | bc -l)")
FAILED_DELTA=$(printf "%+.2f pp" "$(echo "$FAILED - $R_FAILED" | bc -l)")
RETRIES_DELTA=$(printf "%+.0f%%" "$(echo "($RETRIES - $R_RETRIES) / $R_RETRIES * 100" | bc -l 2>/dev/null || echo 0)")

printf "%-20s %9s%% %9s%% %9s%% %10s %8s\n" "Gate pass %" "$B_GATE" "$C_GATE" "$GATE" "$GATE_DELTA" "$T_GATE"
printf "%-20s %9s%% %9s%% %9s%% %10s %8s\n" "Failed %" "$B_FAILED" "$C_FAILED" "$FAILED" "$FAILED_DELTA" "$T_FAILED"
printf "%-20s %10s %10s %10s %10s %8s\n" "Retries avg" "$B_RETRIES" "$C_RETRIES" "$RETRIES" "$RETRIES_DELTA" "$T_RETRIES"
echo ""

echo "## Cache Health"
echo ""
printf "%-20s %10s %10s %10s\n" "Metric" "Baseline" "Option-C" "Current"
printf "%-20s %10s %10s %10s\n" "--------------------" "----------" "----------" "----------"
printf "%-20s %9s%% %9s%% %9s%%\n" "Cache read %" "$B_CACHE" "$C_CACHE" "$CACHE_READ"
printf "%-20s %10s %10s %10s\n" "Avg \$/phase" "\$$B_COST" "\$$C_COST" "\$$COST"
printf "%-20s %10s %10s %10s\n" "Avg \$/mission" "-" "\$$C_MISSION_COST" "\$$MISSION_COST"
printf "%-20s %10s %10s %10s\n" "Avg tokens_in" "$B_TOKENS" "$C_TOKENS" "$TOKENS"
printf "%-20s %9ss %9ss %9ss\n" "Avg duration" "$B_DURATION" "$C_DURATION" "$DURATION"
echo ""

# --- Plan Util (from usage_snapshots, which the OAuth probe populates
#     after each mission completion; see skills/orchestrator/internal/usage/).
#     Only prints when the table is non-empty — before the probe landed
#     (pre-2026-04-15) this section is silently skipped. ---
PLAN_UTIL=$(sqlite3 "$DB" "SELECT COUNT(*) FROM usage_snapshots WHERE captured_at > '$SINCE'" 2>/dev/null || echo "0")
if [[ "$PLAN_UTIL" -gt 0 ]]; then
  # Latest snapshot (for current readout).
  read -r PU_5H PU_5H_RESET PU_7D PU_7D_RESET PU_SONNET <<<"$(sqlite3 -separator ' ' "$DB" "SELECT
      ROUND(five_hour_util * 100, 2),
      COALESCE(five_hour_resets_at, '-'),
      ROUND(seven_day_util * 100, 2),
      COALESCE(seven_day_resets_at, '-'),
      ROUND(seven_day_sonnet_util * 100, 2)
    FROM usage_snapshots
    WHERE captured_at > '$SINCE'
    ORDER BY captured_at DESC LIMIT 1" 2>/dev/null || echo "- - - - -")"

  # Window-level deltas. first/last seven_day_util across the window —
  # if the window crossed a reset (later util < earlier util), the delta
  # understates real consumption; we flag this below.
  read -r WIN_FIRST_7D WIN_LAST_7D WIN_MIN_7D <<<"$(sqlite3 -separator ' ' "$DB" "SELECT
      ROUND((SELECT seven_day_util FROM usage_snapshots WHERE captured_at > '$SINCE' ORDER BY captured_at ASC LIMIT 1) * 100, 2),
      ROUND((SELECT seven_day_util FROM usage_snapshots WHERE captured_at > '$SINCE' ORDER BY captured_at DESC LIMIT 1) * 100, 2),
      ROUND((SELECT MIN(seven_day_util) FROM usage_snapshots WHERE captured_at > '$SINCE') * 100, 2)
    " 2>/dev/null || echo "- - -")"

  # Crossed-reset detection: if min < first during the window, utilization
  # dropped at some point → reset occurred. Clean cycle observable iff both
  # the start AND end of the window fell on the same side of a reset.
  CROSSED_RESET="no"
  if [[ "$WIN_MIN_7D" != "-" ]] && [[ "$WIN_FIRST_7D" != "-" ]]; then
    if awk "BEGIN{exit !($WIN_MIN_7D < $WIN_FIRST_7D)}"; then
      CROSSED_RESET="yes"
    fi
  fi

  # Hours until next seven_day reset (from the latest snapshot).
  HOURS_UNTIL_RESET="-"
  if [[ "$PU_7D_RESET" != "-" ]]; then
    HOURS_UNTIL_RESET=$(awk -v reset="$PU_7D_RESET" 'BEGIN{
      cmd="date -u -j -f \"%Y-%m-%dT%H:%M:%SZ\" \"" reset "\" +%s 2>/dev/null || date -u -d \"" reset "\" +%s"
      cmd | getline t
      close(cmd)
      cmd2="date -u +%s"
      cmd2 | getline now
      close(cmd2)
      printf "%.1f", (t - now) / 3600.0
    }')
  fi

  # Real plan-consumption per phase. Only meaningful when the window has not
  # crossed a 7d reset — otherwise the delta understates real burn.
  PLAN_DELTA_PP="-"
  PLAN_PER_PHASE="-"
  PHASES_PER_PCT="-"
  if [[ "$CROSSED_RESET" == "no" ]] && [[ "$WIN_FIRST_7D" != "-" ]] && [[ "$PU_7D" != "-" ]] && [[ "$TOTAL" -gt 0 ]]; then
    PLAN_DELTA_PP=$(awk -v s="$WIN_FIRST_7D" -v e="$PU_7D" 'BEGIN{printf "%+.2f pp", e - s}')
    PLAN_PER_PHASE=$(awk -v s="$WIN_FIRST_7D" -v e="$PU_7D" -v n="$TOTAL" 'BEGIN{d=e-s; printf "%.3f%%", (n>0 ? d/n : 0)}')
    PHASES_PER_PCT=$(awk -v s="$WIN_FIRST_7D" -v e="$PU_7D" -v n="$TOTAL" 'BEGIN{d=e-s; printf "%.0f", (d>0 ? n/d : 0)}')
  fi

  echo "## Plan Util (real, from Anthropic OAuth endpoint)"
  echo ""
  printf "%-28s %10s\n" "Metric" "Value"
  printf "%-28s %10s\n" "----------------------------" "----------"
  printf "%-28s %9s%%\n" "5h util (current)"       "$PU_5H"
  printf "%-28s %9s%%\n" "7d util (current)"       "$PU_7D"
  printf "%-28s %9s%%\n" "7d Sonnet util (current)" "$PU_SONNET"
  printf "%-28s %9s%%\n" "7d at window start"      "$WIN_FIRST_7D"
  printf "%-28s %10s\n" "7d util burned (window)"  "$PLAN_DELTA_PP"
  printf "%-28s %10s\n" "Plan % per phase"         "$PLAN_PER_PHASE"
  printf "%-28s %10s\n" "Phases per 1% of plan"    "$PHASES_PER_PCT"
  printf "%-28s %9s h\n" "Hours until 7d reset"    "$HOURS_UNTIL_RESET"
  printf "%-28s %10s\n" "Window crossed 7d reset?" "$CROSSED_RESET"
  if [[ "$CROSSED_RESET" == "no" ]] && [[ "$PLAN_UTIL" -lt 20 ]]; then
    printf "  note: window has not yet crossed a 7d reset — partial cycle, treat deltas as directional\n"
  fi
  echo ""
fi

echo "## Per-Persona"
echo ""
printf "%-28s %5s %9s %10s %8s\n" "Persona" "n" "Gate %" "Avg \$" "Retries"
printf "%-28s %5s %9s %10s %8s\n" "----------------------------" "-----" "---------" "----------" "--------"
echo "$PERSONA_DATA" | while IFS='|' read -r persona n gate cost retries; do
  printf "%-28s %5s %8s%% %10s %8s\n" "$persona" "$n" "$gate" "\$$cost" "$retries"
done
echo ""

echo "## Skill Invocations"
echo ""
echo "Total: $SKILL_TOTAL (declared: $SKILL_DECLARED, output_parse: $SKILL_PARSED)"
echo ""

# --- Barok Compliance (from ~/.alluka/barok-density/*.json, emitted by
#     worker.ObserveDensity on each Barok-eligible terminal phase artifact).
#     Aggregates by intensity tier so window-level ranking is honest across
#     persona mixes.
BAROK_DENSITY_DIR="${NANIKA_BAROK_DENSITY_DIR:-$HOME/.alluka/barok-density}"
echo "## Barok Compliance"
echo ""
if [[ -d "$BAROK_DENSITY_DIR" ]] && compgen -G "$BAROK_DENSITY_DIR/*.json" >/dev/null; then
  # Aggregate per-tier: n, median article_count, median linking_verb_count,
  # median fragment_ratio, median avg_sentence_len, median total_bytes.
  BAROK_ROWS=$(jq -rs '
    def median: sort as $s | ($s | length) as $n
      | if $n == 0 then 0
        elif $n % 2 == 1 then $s[$n/2|floor]
        else ($s[$n/2 - 1] + $s[$n/2]) / 2
        end;
    group_by(.intensity_tier)
    | map({
        tier: (.[0].intensity_tier // "unknown"),
        n: length,
        art:  ([.[].article_count]      | median),
        lv:   ([.[].linking_verb_count] | median),
        frag: ([.[].fragment_ratio]     | median),
        asl:  ([.[].avg_sentence_len]   | median),
        tb:   ([.[].total_bytes]        | median)
      })
    | .[]
    | [.tier, .n, .art, .lv, (.frag|tostring), (.asl|tostring), .tb] | @tsv
  ' "$BAROK_DENSITY_DIR"/*.json 2>/dev/null)

  if [[ -n "$BAROK_ROWS" ]]; then
    printf "%-16s %5s %8s %8s %10s %12s %10s\n" "Tier" "n" "art(med)" "lv(med)" "frag(med)" "sent_len(med)" "bytes(med)"
    printf "%-16s %5s %8s %8s %10s %12s %10s\n" "----------------" "-----" "--------" "--------" "----------" "------------" "----------"
    while IFS=$'\t' read -r tier n art lv frag asl tb; do
      printf "%-16s %5s %8s %8s %10s %12s %10s\n" "$tier" "$n" "$art" "$lv" "$frag" "$asl" "$tb"
    done <<< "$BAROK_ROWS"
  else
    echo "(no density records parseable in $BAROK_DENSITY_DIR)"
  fi
else
  echo "(no density data yet — $BAROK_DENSITY_DIR absent or empty)"
fi
echo ""

if [[ -n "$FAILED_DETAIL" ]]; then
  echo "## Failed Phases"
  echo ""
  printf "%-28s %-30s %s\n" "Persona" "Phase" "Mission"
  printf "%-28s %-30s %s\n" "----------------------------" "------------------------------" "--------------------"
  echo "$FAILED_DETAIL" | while IFS='|' read -r persona phase mission; do
    printf "%-28s %-30s %s\n" "$persona" "$phase" "$mission"
  done
fi
