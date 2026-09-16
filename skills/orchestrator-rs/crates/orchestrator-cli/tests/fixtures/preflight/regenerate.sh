#!/bin/sh
# Regenerates the `preflight_helpers_go_matrix` oracle captures.
#
# Every expectation in that gate is a recorded observation of the frozen
# in-tree Go binary over a synthetic home this script builds, normalized by the
# two named rules the gate re-applies:
#
#   FIXTURE-ROOT — the synthetic home's absolute path becomes <FIXTURE>.
#   LAST-EVENT   — `last_event: <RFC3339>` becomes `last_event: <MASKED>`,
#                  the mission section's only clock-derived field.
#
# Nothing else is masked. The gate replays each capture against a home built
# the same way and compares the normalized bytes, so a change in the preflight
# registry — a section added, renamed, reordered, or re-prioritized — is red.
#
#   ORCHESTRATOR_ACCEPTED_GO_BIN=<frozen in-tree go binary> ./regenerate.sh
set -eu

here=$(cd "$(dirname "$0")" && pwd -P)
oracle="$here/oracle"
go_bin=${ORCHESTRATOR_ACCEPTED_GO_BIN:?ORCHESTRATOR_ACCEPTED_GO_BIN must name the frozen in-tree Go oracle}

fixture=$(cd "$(mktemp -d)" && pwd -P)
mkdir -p "$fixture/user" "$fixture/home/workspaces/ws-preflight" \
         "$fixture/personas" "$fixture/empty-path" "$fixture/kb"
printf 'a fixture mission\n' >"$fixture/home/workspaces/ws-preflight/mission.md"
cat >"$fixture/home/workspaces/ws-preflight/checkpoint.json" <<'JSON'
{"version":1,"payload":{"version":2,"workspace_id":"ws-preflight","domain":"dev","plan":{"id":"plan-preflight","task":"fixture task","phases":[{"id":"phase-1","name":"build","status":"completed"},{"id":"phase-2","name":"verify","status":"pending"}]},"status":"in_progress","started_at":"2026-07-13T00:00:00Z"}}
JSON

run() {
  ( cd "$fixture" && env -i \
      HOME="$fixture/user" \
      ORCHESTRATOR_CONFIG_DIR="$fixture/home" \
      ORCHESTRATOR_PERSONAS_DIR="$fixture/personas" \
      PATH="$fixture/empty-path" \
      TMPDIR="$fixture" \
      KB_DATA_DIR="$fixture/kb" \
      KB_INDEX_DB="$fixture/kb/index.db" \
      KB_VECTORS_FILE="$fixture/kb/vault-vectors.kbvec" \
      NANIKA_JOB_TRACKER_DB="$fixture/kb/job-tracker.db" \
      "$go_bin" "$@" )
}

normalize() {
  sed -e "s|$fixture|<FIXTURE>|g" \
      -e 's|last_event: [0-9TZ:.+-]*|last_event: <MASKED>|g'
}

emit() {
  name=$1
  shift
  directory="$oracle/$name"
  mkdir -p "$directory"
  : >"$directory/argv"
  for argument in "$@"; do printf '%s\n' "$argument" >>"$directory/argv"; done
  run "$@" >"$fixture/out" 2>"$fixture/err" && exit_code=0 || exit_code=$?
  normalize <"$fixture/out" >"$directory/stdout"
  normalize <"$fixture/err" >"$directory/stderr"
  printf '%s\n' "$exit_code" >"$directory/exit"
}

rm -rf "$oracle"
mkdir -p "$oracle"

emit default hooks preflight
emit json hooks preflight --format json
emit sections-mission-kb hooks preflight --sections mission,kb
emit sections-scheduler hooks preflight --sections scheduler
emit sections-unknown hooks preflight --sections no-such-section
emit budget-truncated hooks preflight --max-bytes 400
emit budget-single-section hooks preflight --max-bytes 120
emit budget-unlimited hooks preflight --max-bytes 0
emit help hooks preflight --help

rm -rf "$fixture"
printf 'regenerated %s captures\n' "$(find "$oracle" -name exit | wc -l | tr -d ' ')"
