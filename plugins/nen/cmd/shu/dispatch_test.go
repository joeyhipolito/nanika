package main

import (
	"database/sql"
	"testing"
	"time"

	_ "modernc.org/sqlite"
)

// openMemDB opens an in-memory proposals.db with the dispatches schema.
func openMemDB(t *testing.T) *sql.DB {
	t.Helper()
	db, err := sql.Open("sqlite", ":memory:")
	if err != nil {
		t.Fatalf("open mem db: %v", err)
	}
	for _, stmt := range []string{
		`CREATE TABLE IF NOT EXISTS proposals (
			dedup_key        TEXT PRIMARY KEY,
			last_proposed_at DATETIME NOT NULL
		)`,
		`CREATE TABLE IF NOT EXISTS dispatches (
			id            INTEGER PRIMARY KEY AUTOINCREMENT,
			issue_id      TEXT     NOT NULL,
			mission_file  TEXT     NOT NULL,
			workspace_id  TEXT     NOT NULL DEFAULT '',
			started_at    DATETIME NOT NULL,
			finished_at   DATETIME,
			outcome       TEXT     NOT NULL DEFAULT ''
		)`,
	} {
		if _, err := db.Exec(stmt); err != nil {
			t.Fatalf("migrate mem db: %v", err)
		}
	}
	return db
}

// insertDispatch inserts a row directly into dispatches.
func insertDispatch(t *testing.T, db *sql.DB, issueID, missionFile, startedAt string, finishedAt *string, outcome string) {
	t.Helper()
	if finishedAt == nil {
		_, err := db.Exec(`INSERT INTO dispatches (issue_id, mission_file, started_at, outcome) VALUES (?, ?, ?, ?)`,
			issueID, missionFile, startedAt, outcome)
		if err != nil {
			t.Fatalf("insert dispatch: %v", err)
		}
	} else {
		_, err := db.Exec(`INSERT INTO dispatches (issue_id, mission_file, started_at, finished_at, outcome) VALUES (?, ?, ?, ?, ?)`,
			issueID, missionFile, startedAt, *finishedAt, outcome)
		if err != nil {
			t.Fatalf("insert dispatch: %v", err)
		}
	}
}

func strPtr(s string) *string { return &s }

// --- checkThrottle tests ---

func TestCheckThrottle_FirstDispatch(t *testing.T) {
	db := openMemDB(t)
	defer db.Close()

	decision, err := checkThrottle(db, dispatchLimits{MaxConcurrent: 1, MaxPerHour: 6}, time.Now())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if decision != throttleAllow {
		t.Fatalf("expected throttleAllow, got %v", decision)
	}
}

func TestCheckThrottle_AtConcurrentLimit(t *testing.T) {
	db := openMemDB(t)
	defer db.Close()

	now := time.Now().UTC()
	insertDispatch(t, db, "TRK-1", "/mission.md", now.Add(-1*time.Minute).Format(time.RFC3339), nil, "")

	decision, err := checkThrottle(db, dispatchLimits{MaxConcurrent: 1, MaxPerHour: 6}, now)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if decision != throttleDeferConcurrent {
		t.Fatalf("expected throttleDeferConcurrent, got %v", decision)
	}
}

func TestCheckThrottle_AtRateLimit(t *testing.T) {
	db := openMemDB(t)
	defer db.Close()

	now := time.Now().UTC()
	finishedAt := now.Add(-5 * time.Minute).Format(time.RFC3339)
	for i := 0; i < 6; i++ {
		startedAt := now.Add(-30 * time.Minute).Add(time.Duration(i) * time.Minute).Format(time.RFC3339)
		insertDispatch(t, db, "TRK-1", "/mission.md", startedAt, &finishedAt, "success")
	}

	decision, err := checkThrottle(db, dispatchLimits{MaxConcurrent: 1, MaxPerHour: 6}, now)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if decision != throttleDeferRate {
		t.Fatalf("expected throttleDeferRate, got %v", decision)
	}
}

func TestCheckThrottle_RollingWindow_OutOfWindow(t *testing.T) {
	db := openMemDB(t)
	defer db.Close()

	now := time.Now().UTC()
	// All 6 dispatches started 61+ minutes ago — outside the rolling window.
	outsideWindow := now.Add(-61 * time.Minute).Format(time.RFC3339)
	finishedAt := now.Add(-60 * time.Minute).Format(time.RFC3339)
	for i := 0; i < 6; i++ {
		insertDispatch(t, db, "TRK-1", "/mission.md", outsideWindow, &finishedAt, "success")
	}

	decision, err := checkThrottle(db, dispatchLimits{MaxConcurrent: 1, MaxPerHour: 6}, now)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if decision != throttleAllow {
		t.Fatalf("expected throttleAllow (rows outside window), got %v", decision)
	}
}

func TestCheckThrottle_CrashedActiveRow_CountsAsConcurrent(t *testing.T) {
	db := openMemDB(t)
	defer db.Close()

	// Row started 3 hours ago, not yet past 6h crash threshold: counts as active.
	now := time.Now().UTC()
	startedAt := now.Add(-3 * time.Hour).Format(time.RFC3339)
	insertDispatch(t, db, "TRK-1", "/mission.md", startedAt, nil, "")

	decision, err := checkThrottle(db, dispatchLimits{MaxConcurrent: 1, MaxPerHour: 6}, now)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if decision != throttleDeferConcurrent {
		t.Fatalf("expected throttleDeferConcurrent (crashed not yet recovered), got %v", decision)
	}
}

func TestRecoverCrashedDispatches(t *testing.T) {
	db := openMemDB(t)
	defer db.Close()

	now := time.Now().UTC()
	// Row started 7 hours ago — past the 6h watchdog threshold.
	startedAt := now.Add(-7 * time.Hour).Format(time.RFC3339)
	insertDispatch(t, db, "TRK-1", "/mission.md", startedAt, nil, "")

	if err := recoverCrashedDispatches(db, now); err != nil {
		t.Fatalf("recoverCrashedDispatches: %v", err)
	}

	// After recovery, the row should have outcome='crashed' and finished_at set.
	var outcome string
	var finishedAt sql.NullString
	if err := db.QueryRow(`SELECT outcome, finished_at FROM dispatches LIMIT 1`).Scan(&outcome, &finishedAt); err != nil {
		t.Fatalf("query after recover: %v", err)
	}
	if outcome != "crashed" {
		t.Errorf("expected outcome=crashed, got %q", outcome)
	}
	if !finishedAt.Valid || finishedAt.String == "" {
		t.Error("expected finished_at to be set after crash recovery")
	}

	// The row should no longer count as active.
	decision, err := checkThrottle(db, dispatchLimits{MaxConcurrent: 1, MaxPerHour: 6}, now)
	if err != nil {
		t.Fatalf("checkThrottle after recovery: %v", err)
	}
	if decision != throttleAllow {
		t.Fatalf("expected throttleAllow after crash recovery, got %v", decision)
	}
}

func TestRecordDispatchStart_ReturnsID(t *testing.T) {
	db := openMemDB(t)
	defer db.Close()

	now := time.Now().UTC()
	rowID, err := recordDispatchStart(db, "TRK-42", "/some/mission.md", now)
	if err != nil {
		t.Fatalf("recordDispatchStart: %v", err)
	}
	if rowID <= 0 {
		t.Fatalf("expected positive rowID, got %d", rowID)
	}

	var issueID, missionFile string
	var finishedAt sql.NullString
	if err := db.QueryRow(`SELECT issue_id, mission_file, finished_at FROM dispatches WHERE id = ?`, rowID).
		Scan(&issueID, &missionFile, &finishedAt); err != nil {
		t.Fatalf("query dispatch row: %v", err)
	}
	if issueID != "TRK-42" {
		t.Errorf("issue_id: got %q, want TRK-42", issueID)
	}
	if missionFile != "/some/mission.md" {
		t.Errorf("mission_file: got %q, want /some/mission.md", missionFile)
	}
	if finishedAt.Valid {
		t.Error("finished_at should be NULL after recordDispatchStart")
	}
}

func TestRecordDispatchFinish_UpdatesOutcome(t *testing.T) {
	db := openMemDB(t)
	defer db.Close()

	now := time.Now().UTC()
	rowID, err := recordDispatchStart(db, "TRK-1", "/m.md", now)
	if err != nil {
		t.Fatalf("recordDispatchStart: %v", err)
	}

	finishTime := now.Add(2 * time.Minute)
	if err := recordDispatchFinish(db, rowID, "success", "ws-abc123", finishTime); err != nil {
		t.Fatalf("recordDispatchFinish: %v", err)
	}

	var outcome, workspaceID, finishedAt string
	if err := db.QueryRow(`SELECT outcome, workspace_id, finished_at FROM dispatches WHERE id = ?`, rowID).
		Scan(&outcome, &workspaceID, &finishedAt); err != nil {
		t.Fatalf("query after finish: %v", err)
	}
	if outcome != "success" {
		t.Errorf("outcome: got %q, want success", outcome)
	}
	if workspaceID != "ws-abc123" {
		t.Errorf("workspace_id: got %q, want ws-abc123", workspaceID)
	}
	if finishedAt == "" {
		t.Error("finished_at should be set")
	}
}

// --- extractWorkspaceID tests ---

func TestExtractWorkspaceID_PrefixFormats(t *testing.T) {
	cases := []struct {
		output string
		want   string
	}{
		{"workspace: ws-abc123\ndone\n", "ws-abc123"},
		{"started workspace ws-xyz999\n", "ws-xyz999"},
		{"WORKSPACE_ID: ws-001\n", "ws-001"},
		{"no workspace info here", ""},
	}
	for _, tc := range cases {
		got := extractWorkspaceID(tc.output)
		if got != tc.want {
			t.Errorf("extractWorkspaceID(%q) = %q, want %q", tc.output, got, tc.want)
		}
	}
}
