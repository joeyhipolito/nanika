#!/usr/bin/env bash
# Regenerates the Gate 3 fixture stores and their frozen Go oracle captures.
#
# The oracle is the *accepted Go binary*'s own stdout/stderr/exit status over a
# `metrics.db` that the same binary created (authentic schema, including the
# post-migration columns) and that this script then populated with deterministic
# rows. Nothing here is hand-written expected output.
#
#   ORCHESTRATOR_ACCEPTED_GO_BIN=/path/to/orchestrator ./regenerate.sh
#
# Every captured case is deliberately time-independent: no case uses a moving
# `datetime('now', '-N days')` window with a bound that could exclude a fixture
# row later, so a frozen capture stays valid indefinitely.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
go_bin="${ORCHESTRATOR_ACCEPTED_GO_BIN:-$HOME/.alluka/bin/orchestrator}"
[ -x "$go_bin" ] || { echo "accepted Go binary not executable: $go_bin" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# `go_run <config-dir> <args...>` captures one oracle observation.
capture() {
  local out_dir="$1" cfg="$2"; shift 2
  local home="$work/home"; mkdir -p "$home"
  mkdir -p "$out_dir"
  set +e
  env -i HOME="$home" PATH=/usr/bin:/bin ORCHESTRATOR_CONFIG_DIR="$cfg" \
    "$go_bin" "$@" >"$out_dir/stdout" 2>"$out_dir/stderr"
  echo -n "$?" >"$out_dir/exit"
  set -e
}

# ---------------------------------------------------------------- metrics.db --
# Let the Go binary create each database so the schema (and its additive
# migrations) is Go's, byte for byte.
init_db() {
  local cfg="$1"; mkdir -p "$cfg"
  env -i HOME="$work/home" PATH=/usr/bin:/bin ORCHESTRATOR_CONFIG_DIR="$cfg" \
    "$go_bin" metrics >/dev/null 2>&1 || true
}

populated="$work/populated"; init_db "$populated"
boundary="$work/boundary";   init_db "$boundary"
empty="$work/empty";         init_db "$empty"

sqlite3 "$populated/metrics.db" <<'SQL'
INSERT INTO missions (id,domain,task,started_at,finished_at,duration_s,phases_total,phases_completed,phases_failed,status,decomp_source) VALUES
 ('ws-alpha','dev','build the thing','2026-07-16T10:00:00Z','2026-07-16T10:00:42Z',42,3,3,0,'success','decomp.llm'),
 ('ws-beta','work','line one
line two','2026-07-15T09:00:00Z','2026-07-15T09:01:40Z',100,2,1,1,'failed','decomp.keyword'),
 ('ws-gamma','dev','a deliberately long task description that must be truncated by the fifty byte rule','2026-07-14T08:00:00Z','2026-07-14T08:00:07Z',7,5,2,0,'running','predecomposed');

-- persona != '' and selection_method != 'required_review' -> routing-methods rows.
-- The required_review phase and the empty-persona phase prove the two WHERE
-- clauses differ between `personas` and `routing-methods`.
INSERT INTO phases (id,mission_id,name,persona,selection_method,duration_s,status,retries,gate_passed,parsed_skills,worker_name) VALUES
 ('ws-alpha_design','ws-alpha','design','architect','llm',10,'success',0,1,'["rust-best-practices","domain-modeling"]','alpha'),
 ('ws-alpha_implement','ws-alpha','implement','developer','llm',20,'success',1,1,'["golang-testing"]','alpha'),
 ('ws-alpha_polish','ws-alpha','polish','developer','llm',5,'success',0,1,'','alpha'),
 ('ws-alpha_review','ws-alpha','review','reviewer','fallback',7,'success',0,1,'[]','alpha'),
 ('ws-alpha_gate','ws-alpha','gate','reviewer','required_review',3,'success',0,1,'not-json','alpha'),
 ('ws-alpha_orphan','ws-alpha','orphan','','llm',1,'success',0,1,'',''),
 ('ws-beta_implement','ws-beta','implement','developer','llm',60,'failed',2,0,'','' ),
 ('ws-beta_verify','ws-beta','verify','qa','keyword',30,'success',0,1,'',''),
 ('ws-beta_retry','ws-beta','retry','qa','fallback',10,'failed',3,0,'',''),
 ('ws-gamma_plan','ws-gamma','plan','architect','llm',2,'success',0,1,'',''),
 ('ws-gamma_write','ws-gamma','write','writer','fallback',3,'success',0,1,'',''),
 ('ws-gamma_edit','ws-gamma','edit','writer','fallback',2,'success',0,1,'','');

INSERT INTO skill_invocations (mission_id,phase,persona,skill_name,source,invoked_at) VALUES
 ('ws-alpha','design','architect','rust-best-practices','declared','2026-07-16T10:00:05Z'),
 ('ws-alpha','design','architect','rust-best-practices','declared','2026-07-16T10:00:06Z'),
 ('ws-alpha','implement','developer','golang-testing','output_parse','2026-07-16T10:00:20Z'),
 ('ws-beta','verify','qa','golang-testing','declared','2026-07-15T09:00:30Z');
SQL

# fallback share is exactly 30.0% here: 10 routing-eligible phases, 3 fallback.
# Go alerts only when the rate is strictly greater than the 30% threshold, so
# this database pins the boundary from below.
sqlite3 "$boundary/metrics.db" <<'SQL'
INSERT INTO missions (id,domain,task,started_at,finished_at,duration_s,phases_total,phases_completed,phases_failed,status,decomp_source) VALUES
 ('ws-edge','dev','boundary','2026-07-16T10:00:00Z','2026-07-16T10:00:10Z',10,10,10,0,'success','decomp.llm');
INSERT INTO phases (id,mission_id,name,persona,selection_method,duration_s,status,retries,gate_passed,parsed_skills,worker_name) VALUES
 ('ws-edge_1','ws-edge','p1','developer','llm',1,'success',0,1,'',''),
 ('ws-edge_2','ws-edge','p2','developer','llm',1,'success',0,1,'',''),
 ('ws-edge_3','ws-edge','p3','developer','llm',1,'success',0,1,'',''),
 ('ws-edge_4','ws-edge','p4','developer','llm',1,'success',0,1,'',''),
 ('ws-edge_5','ws-edge','p5','developer','llm',1,'success',0,1,'',''),
 ('ws-edge_6','ws-edge','p6','developer','llm',1,'success',0,1,'',''),
 ('ws-edge_7','ws-edge','p7','developer','keyword',1,'success',0,1,'',''),
 ('ws-edge_8','ws-edge','p8','developer','fallback',1,'success',0,1,'',''),
 ('ws-edge_9','ws-edge','p9','developer','fallback',1,'success',0,1,'',''),
 ('ws-edge_10','ws-edge','p10','developer','fallback',1,'success',0,1,'','');
SQL

# ------------------------------------------------------------- audits.jsonl --
# The third line is deliberately malformed: Go's store.go:72-77 skips an
# unparseable line silently rather than failing the read.
audits="$work/audits"; mkdir -p "$audits"
cat > "$audits/audits.jsonl" <<'JSONL'
{"workspace_id":"ws-alpha","domain":"dev","audited_at":"2026-07-16T01:02:03Z","scorecard":{"decomposition_quality":4,"persona_fit":5,"skill_utilization":3,"output_quality":4,"rule_compliance":5,"overall":4}}
{"workspace_id":"ws-beta","domain":"work","audited_at":"2026-07-15T01:02:03Z","scorecard":{"decomposition_quality":2,"persona_fit":1,"skill_utilization":2,"output_quality":3,"rule_compliance":1,"overall":2}}
{ this line is not json at all
{"workspace_id":"ws-gamma","domain":"dev","audited_at":"2026-07-14T01:02:03Z","scorecard":{"decomposition_quality":5,"persona_fit":5,"skill_utilization":5,"output_quality":5,"rule_compliance":5,"overall":5}}
JSONL

audits_empty="$work/audits-empty"; mkdir -p "$audits_empty"

# ------------------------------------------------------------------ capture --
rm -rf "$here/stores" "$here/oracle"
mkdir -p "$here/stores/populated" "$here/stores/boundary" "$here/stores/empty" \
         "$here/stores/audits" "$here/stores/audits-empty"

metrics_cases_populated=(
  "missions-default::metrics"
  "missions-last-2::metrics|--last|2"
  "missions-last-zero::metrics|--last|0"
  "missions-last-negative::metrics|--last|-5"
  "missions-domain-dev::metrics|--domain|dev"
  "missions-status-failed::metrics|--status|failed"
  "missions-decomp-source::metrics|--decomp-source|predecomposed"
  "missions-worker-alpha::metrics|--worker|alpha"
  "missions-worker-ephemeral::metrics|--worker|ephemeral"
  "missions-days-wide::metrics|--days|100000"
  "personas::metrics|personas"
  "skills::metrics|skills"
  "trends-wide::metrics|trends|--days|100000"
  "routing-methods::metrics|routing-methods"
  "phases-alpha::metrics|phases|ws-alpha"
  "phases-missing::metrics|phases|ws-nonexistent"
)
metrics_cases_boundary=(
  "routing-methods-boundary::metrics|routing-methods"
)
metrics_cases_empty=(
  "missions-empty::metrics"
  "personas-empty::metrics|personas"
  "skills-empty::metrics|skills"
  "trends-empty-zero-days::metrics|trends|--days|0"
  "routing-methods-empty::metrics|routing-methods"
  "phases-empty::metrics|phases|ws-alpha"
)
audit_cases=(
  "scorecard-text::audit|scorecard"
  "scorecard-json::audit|scorecard|--format=json"
  "scorecard-domain-dev::audit|scorecard|--domain|dev"
  "scorecard-last-1::audit|scorecard|--last|1"
  "scorecard-last-zero::audit|scorecard|--last|0"
  "scorecard-last-negative::audit|scorecard|--last|-1"
  "scorecard-local::audit|scorecard|--local"
)
audit_empty_cases=(
  "scorecard-empty-text::audit|scorecard"
  "scorecard-empty-json::audit|scorecard|--format=json"
)

run_group() {
  local cfg="$1" group="$2"; shift 2
  for case_spec in "$@"; do
    local name="${case_spec%%::*}" args="${case_spec#*::}"
    IFS='|' read -r -a argv <<< "$args"
    capture "$here/oracle/$group/$name" "$cfg" "${argv[@]}"
    printf '%s\n' "${argv[@]}" > "$here/oracle/$group/$name/argv"
  done
}

run_group "$populated" populated "${metrics_cases_populated[@]}"
run_group "$boundary"  boundary  "${metrics_cases_boundary[@]}"
run_group "$empty"     empty     "${metrics_cases_empty[@]}"
run_group "$audits"    audits    "${audit_cases[@]}"
run_group "$audits_empty" audits-empty "${audit_empty_cases[@]}"

# Go's InitDB opens metrics.db in WAL mode (db.go pragmas), and every oracle
# capture above re-applies that pragma. SQLite refuses a SQLITE_OPEN_READ_ONLY
# connection to a WAL database whose -shm/-wal side files are absent, so a
# checked-in single-file fixture must be converted to the rollback journal after
# the last Go invocation. VACUUM can reset the mode, so set it last and verify —
# a silently-WAL fixture then fails here rather than as an opaque CannotOpen in
# the gate.
for cfg in "$populated" "$boundary" "$empty"; do
  sqlite3 "$cfg/metrics.db" "PRAGMA wal_checkpoint(TRUNCATE);" >/dev/null
  sqlite3 "$cfg/metrics.db" "VACUUM;" >/dev/null
  sqlite3 "$cfg/metrics.db" "PRAGMA journal_mode=DELETE;" >/dev/null
  mode="$(sqlite3 "$cfg/metrics.db" "PRAGMA journal_mode;")"
  [ "$mode" = "delete" ] || { echo "fixture $cfg is journal_mode=$mode, expected delete" >&2; exit 1; }
  rm -f "$cfg/metrics.db-wal" "$cfg/metrics.db-shm"
done

cp "$populated/metrics.db" "$here/stores/populated/metrics.db"
cp "$boundary/metrics.db"  "$here/stores/boundary/metrics.db"
cp "$empty/metrics.db"     "$here/stores/empty/metrics.db"
cp "$audits/audits.jsonl"  "$here/stores/audits/audits.jsonl"
: > "$here/stores/audits-empty/.keep"

"$go_bin" --version > "$here/oracle/go-binary-version" 2>&1 || true
echo "regenerated fixtures + oracle under $here"
