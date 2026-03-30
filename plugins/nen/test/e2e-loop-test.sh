#!/usr/bin/env bash
# e2e-loop-test.sh — End-to-end test of the nen self-improvement loop.
#
# Proves the full cycle works:
#   1. Insert a synthetic finding into findings.db (known ID)
#   2. Run shu propose → verify tracker issue created + mission file written
#   3. Approve the tracker issue (set status in-progress)
#   4. Run dispatch-approved.sh → verify it picks up the issue
#   5. Verify the finding is superseded in findings.db
#
# Uses ALLUKA_HOME isolation so the real findings.db is never touched.
# Creates a real tracker issue (cleaned up on exit).
# Stubs the orchestrator binary so no real mission runs.
#
# Usage:
#   bash plugins/nen/test/e2e-loop-test.sh
#
# Prerequisites: shu, tracker, sqlite3, jq must be in PATH.

set -euo pipefail

# ─── State ───────────────────────────────────────────────────────────────────

TEST_DIR=$(mktemp -d)
FINDING_ID="e2e-$(date +%s)-$(od -An -N2 -tx1 /dev/urandom | tr -d ' \n')"
TRACKER_ISSUE=""
MISSION_FILE=""
MOCK_BIN_DIR="$TEST_DIR/bin"
ORIGINAL_PATH="$PATH"

# ─── Helpers ─────────────────────────────────────────────────────────────────

pass() { printf "  \033[32m✓\033[0m  %s\n" "$*"; }
fail() { printf "  \033[31m✗\033[0m  %s\n" "$*" >&2; exit 1; }
step() { printf "\n[%s/5] %s\n" "$1" "$2"; }

# ─── Cleanup ─────────────────────────────────────────────────────────────────

cleanup() {
    local rc=$?
    export PATH="$ORIGINAL_PATH"
    # Remove the lock file so a failed test doesn't block future runs
    rm -f "${HOME}/.alluka/dispatch-approved.pid" 2>/dev/null || true
    # Close the tracker issue if it was created (idempotent)
    if [[ -n "$TRACKER_ISSUE" ]]; then
        tracker update "$TRACKER_ISSUE" --status done 2>/dev/null || true
    fi
    # Delete the mission file shu propose wrote to the real missions dir
    if [[ -n "$MISSION_FILE" && -f "$MISSION_FILE" ]]; then
        rm -f "$MISSION_FILE"
    fi
    rm -rf "$TEST_DIR"
    if [[ $rc -ne 0 ]]; then
        printf "\n\033[31mTEST FAILED\033[0m (exit %d)\n" "$rc" >&2
    fi
}
trap cleanup EXIT

# ─── Header ──────────────────────────────────────────────────────────────────

printf "\n\033[1m══════════════════════════════════════════════════════\033[0m\n"
printf "\033[1m  NEN Self-Improvement Loop — E2E Test\033[0m\n"
printf "\033[1m══════════════════════════════════════════════════════\033[0m\n"
printf "  TEST_DIR:    %s\n" "$TEST_DIR"
printf "  FINDING_ID:  %s\n" "$FINDING_ID"

# ─── Prerequisites ────────────────────────────────────────────────────────────

step "0" "Checking prerequisites"
command -v shu     &>/dev/null || fail "shu not found in PATH"
command -v tracker &>/dev/null || fail "tracker not found in PATH"
command -v sqlite3 &>/dev/null || fail "sqlite3 not found in PATH"
command -v jq      &>/dev/null || fail "jq not found in PATH"

DISPATCH_SCRIPT="${HOME}/.alluka/scripts/dispatch-approved.sh"
[[ -f "$DISPATCH_SCRIPT" ]] || fail "dispatch-approved.sh not found at $DISPATCH_SCRIPT — run: shu propose --init"

pass "shu, tracker, sqlite3, jq available"
pass "dispatch-approved.sh exists at $DISPATCH_SCRIPT"

# ─── Step 1: Insert synthetic finding ─────────────────────────────────────────

step "1" "Inserting synthetic finding into isolated findings.db"

# Point all nen/shu commands at a temp dir so the real findings.db is never touched.
export ALLUKA_HOME="$TEST_DIR"

mkdir -p "$TEST_DIR/nen" "$TEST_DIR/missions/remediation"
FINDINGS_DB="$TEST_DIR/nen/findings.db"

# found_at must be >24h in the past for a "high" severity finding to be proposable
# (threshold: age > 24h OR 2+ findings in same category).
# Use macOS date syntax with Linux fallback.
FOUND_AT=$(date -u -v-48H +%Y-%m-%dT%H:%M:%SZ 2>/dev/null \
         || date -u -d '-48 hours' +%Y-%m-%dT%H:%M:%SZ)
NOW=$(date -u +%Y-%m-%dT%H:%M:%SZ)

# Evidence must have at least one item with a non-empty source field.
EVIDENCE='[{"kind":"log","raw":"mock error detected in test component","source":"plugins/nen/test/e2e-loop-test.sh","captured_at":"'"${FOUND_AT}"'"}]'

sqlite3 "$FINDINGS_DB" "
CREATE TABLE IF NOT EXISTS findings (
    id            TEXT PRIMARY KEY,
    ability       TEXT NOT NULL,
    category      TEXT NOT NULL,
    severity      TEXT NOT NULL,
    title         TEXT NOT NULL,
    description   TEXT NOT NULL,
    scope_kind    TEXT NOT NULL,
    scope_value   TEXT NOT NULL,
    evidence      TEXT NOT NULL DEFAULT '[]',
    source        TEXT NOT NULL,
    found_at      DATETIME NOT NULL,
    expires_at    DATETIME,
    superseded_by TEXT NOT NULL DEFAULT '',
    created_at    DATETIME NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_findings_active ON findings(superseded_by, expires_at);
INSERT INTO findings (
    id, ability, category, severity, title, description,
    scope_kind, scope_value, evidence, source, found_at, created_at
) VALUES (
    '${FINDING_ID}',
    'test-ability',
    'test-category',
    'high',
    'E2E Test: mock component failure for loop validation',
    'Synthetic finding injected by e2e-loop-test.sh. Safe to ignore.',
    'mission',
    'test-mission-e2e-001',
    '${EVIDENCE}',
    'e2e-test',
    '${FOUND_AT}',
    '${NOW}'
);
"

ACTIVE=$(sqlite3 "$FINDINGS_DB" \
    "SELECT COUNT(*) FROM findings WHERE superseded_by = '' AND id = '${FINDING_ID}';")
[[ "$ACTIVE" -eq 1 ]] || fail "Finding not in DB after insert (count=$ACTIVE)"
pass "Finding $FINDING_ID inserted (severity=high, found_at=$FOUND_AT)"

# ─── Step 2: shu propose ──────────────────────────────────────────────────────

step "2" "Running: shu propose --json"

PROPOSE_OUT=$(shu propose --json 2>&1) \
    || fail "shu propose failed: $PROPOSE_OUT"

PROPOSAL_COUNT=$(printf '%s\n' "$PROPOSE_OUT" | jq -r '.proposed | length' 2>/dev/null || echo 0)
[[ "$PROPOSAL_COUNT" -ge 1 ]] \
    || fail "Expected >=1 proposal, got 0. Output: $PROPOSE_OUT"

TRACKER_ISSUE=$(printf '%s\n' "$PROPOSE_OUT" | jq -r '.proposed[0].tracker_issue')
MISSION_FILE=$(printf '%s\n'  "$PROPOSE_OUT" | jq -r '.proposed[0].mission_file')
PROPOSED_FINDING=$(printf '%s\n' "$PROPOSE_OUT" | jq -r '.proposed[0].finding_ids[0]')

[[ -n "$TRACKER_ISSUE" && "$TRACKER_ISSUE" != "null" ]] \
    || fail "No tracker_issue in proposal. Output: $PROPOSE_OUT"
[[ -n "$MISSION_FILE" && "$MISSION_FILE" != "null" ]] \
    || fail "No mission_file in proposal. Output: $PROPOSE_OUT"
[[ "$PROPOSED_FINDING" == "$FINDING_ID" ]] \
    || fail "Proposed finding_ids[0]='$PROPOSED_FINDING', want '$FINDING_ID'"

pass "Tracker issue created: $TRACKER_ISSUE"

[[ -f "$MISSION_FILE" ]] \
    || fail "Mission file not written: $MISSION_FILE"

# Verify mission frontmatter references the tracker issue and finding
grep -q "tracker_issue: ${TRACKER_ISSUE}" "$MISSION_FILE" \
    || fail "Mission file missing 'tracker_issue: ${TRACKER_ISSUE}' in frontmatter"
grep -q "finding_ids:" "$MISSION_FILE" \
    || fail "Mission file missing finding_ids in frontmatter"
grep -q "${FINDING_ID}" "$MISSION_FILE" \
    || fail "Mission file missing finding ID ${FINDING_ID}"

pass "Mission file written: $(basename "$MISSION_FILE")"
pass "Mission frontmatter contains tracker_issue + finding_id"

# ─── Step 3: Approve the tracker issue ───────────────────────────────────────

step "3" "Approving tracker issue (status → in-progress)"

tracker update "$TRACKER_ISSUE" --status in-progress \
    || fail "Failed to set $TRACKER_ISSUE to in-progress"

# Verify status was actually updated and capture the hash ID (used by dispatch)
ISSUE_JSON=$(tracker query items --json \
    | jq -c --arg issue "$TRACKER_ISSUE" '
        .items[]
        | select(
            .id == $issue
            or (.seq_id != null and ("TRK-\(.seq_id)") == $issue)
          )
      ' 2>/dev/null || echo "")
[[ -n "$ISSUE_JSON" ]] || fail "Could not find $TRACKER_ISSUE in tracker items"

ISSUE_STATUS=$(printf '%s\n' "$ISSUE_JSON" | jq -r '.status')
[[ "$ISSUE_STATUS" == "in-progress" ]] \
    || fail "Tracker issue status is '$ISSUE_STATUS', expected 'in-progress'"

# Hash ID (e.g. trk-A9F2) is what dispatch-approved.sh uses in its log output
HASH_ID=$(printf '%s\n' "$ISSUE_JSON" | jq -r '.id')

pass "Tracker issue $TRACKER_ISSUE (hash: $HASH_ID) is now in-progress"

# ─── Step 4: Run dispatch-approved.sh ────────────────────────────────────────

step "4" "Running dispatch-approved.sh (with stub orchestrator)"

# Stub orchestrator: exits 0 and emits a workspace-ID-shaped line so
# dispatch-approved.sh can extract it and call shu close correctly.
# The real orchestrator pattern is: [0-9]{8}-[a-f0-9]{8}
mkdir -p "$MOCK_BIN_DIR"
cat > "$MOCK_BIN_DIR/orchestrator" << 'STUB_EOF'
#!/usr/bin/env bash
# Stub orchestrator for e2e testing.
echo "orchestrator: launching workspace 20260330-e2e0abcd"
echo "orchestrator: all phases complete"
exit 0
STUB_EOF
chmod +x "$MOCK_BIN_DIR/orchestrator"

# Put stub first in PATH so it shadows the real orchestrator.
export PATH="$MOCK_BIN_DIR:$ORIGINAL_PATH"

# Remove any stale lock file before running
rm -f "${HOME}/.alluka/dispatch-approved.pid"

DISPATCH_OUT=$(bash "$DISPATCH_SCRIPT" 2>&1) || {
    DISPATCH_RC=$?
    export PATH="$ORIGINAL_PATH"
    fail "dispatch-approved.sh exited $DISPATCH_RC: $DISPATCH_OUT"
}

printf '%s\n' "$DISPATCH_OUT"

# Restore real PATH before further assertions
export PATH="$ORIGINAL_PATH"

# Dispatch log uses the hash ID (e.g. "trk-A9F2"), not the seq ID ("TRK-306").
# Accept either form so the assertion is robust to both tracker ID formats.
printf '%s\n' "$DISPATCH_OUT" | grep -qiE "(${TRACKER_ISSUE}|${HASH_ID})" \
    || fail "Dispatch output references neither $TRACKER_ISSUE nor $HASH_ID"

pass "dispatch-approved.sh ran and processed $TRACKER_ISSUE ($HASH_ID)"

# ─── Step 5: Verify finding superseded ───────────────────────────────────────

step "5" "Verifying finding superseded in findings.db"

SUPERSEDED_BY=$(sqlite3 "$FINDINGS_DB" \
    "SELECT superseded_by FROM findings WHERE id = '${FINDING_ID}';")

[[ -n "$SUPERSEDED_BY" ]] \
    || fail "Finding $FINDING_ID is NOT superseded (superseded_by is empty)"

pass "Finding $FINDING_ID superseded by: $SUPERSEDED_BY"

# Confirm the finding no longer appears as active
STILL_ACTIVE=$(sqlite3 "$FINDINGS_DB" \
    "SELECT COUNT(*) FROM findings WHERE superseded_by = '' AND id = '${FINDING_ID}';")
[[ "$STILL_ACTIVE" -eq 0 ]] \
    || fail "Finding $FINDING_ID still active (superseded_by='$SUPERSEDED_BY' but count=$STILL_ACTIVE)"

pass "Finding $FINDING_ID is no longer active"

# ─── Summary ─────────────────────────────────────────────────────────────────

printf "\n\033[1m══════════════════════════════════════════════════════\033[0m\n"
printf "\033[32m\033[1m  All 5 steps passed — self-improvement loop works\033[0m\n"
printf "\033[1m══════════════════════════════════════════════════════\033[0m\n\n"
