#!/usr/bin/env bash
# Regenerates the Gate 4 fixture learning store and its frozen Go oracle.
#
# The store is created by the accepted Go binary (authentic `initSchema`,
# including the `db.go:165-176` additive migrations, the FTS5 external-content
# table, and its three triggers) and then populated with rows whose timestamps
# are pinned to 2020, so every time-relative Go predicate — `-90 days`,
# `-60 days`, `-30 days`, and `Cleanup`'s `MaxAgeDays` cutoff — resolves the
# same way forever and the captured oracle never drifts.
#
#   ORCHESTRATOR_ACCEPTED_GO_BIN=/path/to/orchestrator ./regenerate.sh
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
go_bin="${ORCHESTRATOR_ACCEPTED_GO_BIN:-$HOME/.alluka/bin/orchestrator}"
[ -x "$go_bin" ] || { echo "accepted Go binary not executable: $go_bin" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
home="$work/home"; cfg="$work/cfg"; mkdir -p "$home" "$cfg"

go_run() {
  env -i HOME="$home" PATH=/usr/bin:/bin ORCHESTRATOR_CONFIG_DIR="$cfg" "$go_bin" "$@"
}

# Let Go create the schema.
go_run stats >/dev/null 2>&1 || true

sqlite3 "$cfg/learnings.db" <<'SQL'
INSERT INTO learnings
 (id,type,content,context,domain,worker_name,workspace_id,tags,seen_count,used_count,
  quality_score,created_at,last_used_at,embedding,injection_count,compliance_count,
  compliance_rate,archived)
VALUES
 -- healthy, injected, compliant: matches no prune or archive criterion
 ('l-001','decision','alpha decision content','alpha context','dev','alpha','ws-1','a,b',
  2,3,0.90,'2020-01-01T00:00:00Z','2020-06-01T00:00:00Z',x'0000803f',10,9,0.90,0),
 -- prune: below --min-score AND the age criterion; archive: criteria 1, 3 and 4
 ('l-002','insight','beta insight content','beta context','dev','','','',
  1,0,0.05,'2020-01-02T00:00:00Z',NULL,NULL,0,0,0.0,0),
 -- archive: chronic non-compliance (criterion 2)
 ('l-003','gotcha','gamma gotcha content','gamma context','work','','','',
  1,0,0.30,'2020-01-03T00:00:00Z',NULL,NULL,7,0,0.05,0),
 -- already archived: every archive criterion excludes archived = 1
 ('l-004','pattern','delta pattern content','delta context','work','beta','ws-2','',
  3,1,0.60,'2020-01-04T00:00:00Z','2020-07-01T00:00:00Z',x'0000003f',2,1,0.50,1),
 -- prune: below --min-score only (used_count = 0, quality 0.15 < 0.5 so also age)
 ('l-005','error','epsilon error content','epsilon context','creative','','','',
  1,0,0.15,'2020-01-05T00:00:00Z',NULL,NULL,0,0,0.0,0);
SQL

capture() {
  local name="$1"; shift
  local out="$here/oracle/$name"; mkdir -p "$out"
  set +e
  go_run "$@" >"$out/stdout" 2>"$out/stderr"
  echo -n "$?" >"$out/exit"
  set -e
  printf '%s\n' "$@" > "$out/argv"
}

# Dry-run captures only. No `--apply` is ever recorded here: running it would
# mutate the fixture, and B3 ports no `--apply` path to compare against.
capture stats                  stats
capture prune-dry              prune
capture prune-dry-flags        prune --max-age 30 --min-score 0.5 --max-count 1
capture archive-dry            archive
capture archive-dry-domain     archive --domain work
capture backfill-dry           backfill-embeddings
capture backfill-dry-limited   backfill-embeddings --limit 2 --batch-size 1 --rpm 30

# Byte-stability of the reference implementation's dry run, so the frozen
# capture above is known to be a stable target rather than a lucky sample.
for name in stats prune-dry archive-dry backfill-dry; do
  mapfile -t argv < "$here/oracle/$name/argv"
  second="$(go_run "${argv[@]}" 2>/dev/null || true)"
  if [ "$second" != "$(cat "$here/oracle/$name/stdout")" ]; then
    echo "Go dry-run for '$name' is not byte-stable; the fixture is time-sensitive" >&2
    exit 1
  fi
done

# Also record Go's own flag surface, so the port's parser is pinned against the
# reference rather than against this script's memory of it.
mkdir -p "$here/oracle/help"
for name in stats prune archive backfill-embeddings; do
  go_run "$name" --help > "$here/oracle/help/$name" 2>&1 || true
done

# As in the analytics fixture: convert away from WAL after the last Go
# invocation so the checked-in store is one file that a SQLITE_OPEN_READ_ONLY
# connection can open.
sqlite3 "$cfg/learnings.db" "PRAGMA wal_checkpoint(TRUNCATE);" >/dev/null
sqlite3 "$cfg/learnings.db" "VACUUM;" >/dev/null
sqlite3 "$cfg/learnings.db" "PRAGMA journal_mode=DELETE;" >/dev/null
mode="$(sqlite3 "$cfg/learnings.db" "PRAGMA journal_mode;")"
[ "$mode" = "delete" ] || { echo "fixture is journal_mode=$mode, expected delete" >&2; exit 1; }
rm -f "$cfg/learnings.db-wal" "$cfg/learnings.db-shm"

# The adapter derives `<fixture-root>/.alluka/learnings.db` itself
# (`knowledge_gateway.rs` LEARNINGS_DB_RELATIVE), so mirror that layout.
rm -rf "$here/store"
mkdir -p "$here/store/.alluka"
cp "$cfg/learnings.db" "$here/store/.alluka/learnings.db"

"$go_bin" --version > "$here/oracle/go-binary-version" 2>&1 || true
echo "regenerated learning fixture + oracle under $here"
