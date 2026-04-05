package main

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sync"
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

// --- reserveDispatchSlot tests ---

func TestReserveDispatchSlot_ConcurrentCallers(t *testing.T) {
	// Use a file-based SQLite DB so the WAL+busy_timeout can mediate concurrent
	// writers (in-memory :memory: databases are per-connection and don't share state).
	dbPath := filepath.Join(t.TempDir(), "proposals.db")
	setup, err := sql.Open("sqlite", dbPath+"?_journal_mode=WAL&_busy_timeout=5000")
	if err != nil {
		t.Fatalf("open setup db: %v", err)
	}
	for _, stmt := range []string{
		`CREATE TABLE IF NOT EXISTS proposals (dedup_key TEXT PRIMARY KEY, last_proposed_at DATETIME NOT NULL)`,
		`CREATE TABLE IF NOT EXISTS dispatches (
			id INTEGER PRIMARY KEY AUTOINCREMENT,
			issue_id TEXT NOT NULL,
			mission_file TEXT NOT NULL,
			workspace_id TEXT NOT NULL DEFAULT '',
			started_at DATETIME NOT NULL,
			finished_at DATETIME,
			outcome TEXT NOT NULL DEFAULT ''
		)`,
	} {
		if _, err := setup.Exec(stmt); err != nil {
			t.Fatalf("migrate: %v", err)
		}
	}
	setup.Close()

	openDB := func(t *testing.T) *sql.DB {
		t.Helper()
		db, err := sql.Open("sqlite", dbPath+"?_journal_mode=WAL&_busy_timeout=5000")
		if err != nil {
			t.Fatalf("open db: %v", err)
		}
		return db
	}

	limits := dispatchLimits{MaxConcurrent: 1, MaxPerHour: 6}
	now := time.Now().UTC()

	type result struct {
		rowID    int64
		decision throttleDecision
		err      error
	}

	results := make([]result, 2)
	var wg sync.WaitGroup
	wg.Add(2)

	for i := range results {
		i := i
		go func() {
			defer wg.Done()
			db := openDB(t)
			defer db.Close()
			rowID, decision, err := reserveDispatchSlot(db, limits, "TRK-1", "/mission.md", now)
			results[i] = result{rowID, decision, err}
		}()
	}
	wg.Wait()

	var allowCount int
	for _, r := range results {
		if r.err != nil {
			t.Fatalf("unexpected error: %v", r.err)
		}
		if r.decision == throttleAllow {
			allowCount++
			if r.rowID <= 0 {
				t.Error("expected positive rowID for the allowed dispatch")
			}
		}
	}
	if allowCount != 1 {
		t.Fatalf("expected exactly 1 allowed dispatch, got %d", allowCount)
	}
}

// --- runOrchestrator timeout tests ---

func TestRunOrchestrator_TimeoutKillsChild(t *testing.T) {
	// Build a tiny Go binary that sleeps 10s so exec.CommandContext can kill
	// the actual process (not a grandchild of a shell interpreter).
	srcDir := t.TempDir()
	src := filepath.Join(srcDir, "main.go")
	if err := os.WriteFile(src, []byte(`package main
import "time"
func main() { time.Sleep(10 * time.Second) }
`), 0o644); err != nil {
		t.Fatalf("write stub src: %v", err)
	}

	binDir := t.TempDir()
	stub := filepath.Join(binDir, "orchestrator")
	buildOut, buildErr := exec.Command("go", "build", "-o", stub, src).CombinedOutput()
	if buildErr != nil {
		t.Fatalf("build stub: %v\n%s", buildErr, buildOut)
	}

	t.Setenv("PATH", binDir+":"+os.Getenv("PATH"))

	ctx, cancel := context.WithTimeout(context.Background(), 1*time.Second)
	defer cancel()

	_, err := runOrchestrator(ctx, "/fake/mission.md")
	if err == nil {
		t.Fatal("expected error, got nil")
	}
	if ctx.Err() != context.DeadlineExceeded {
		t.Fatalf("expected ctx.Err()=DeadlineExceeded, got %v", ctx.Err())
	}
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("expected error to wrap context.DeadlineExceeded, got: %v", err)
	}
}

// --- runDispatch integration tests ---

func TestRunDispatch_TrackerRevertFailureRecordedAsStuck(t *testing.T) {
	// Set up a temp file SQLite DB so runDispatch can open+close it normally.
	dbPath := filepath.Join(t.TempDir(), "proposals.db")
	{
		db, err := sql.Open("sqlite", dbPath+"?_journal_mode=WAL&_busy_timeout=5000")
		if err != nil {
			t.Fatalf("open temp db: %v", err)
		}
		for _, stmt := range []string{
			`CREATE TABLE IF NOT EXISTS proposals (dedup_key TEXT PRIMARY KEY, last_proposed_at DATETIME NOT NULL)`,
			`CREATE TABLE IF NOT EXISTS dispatches (
				id INTEGER PRIMARY KEY AUTOINCREMENT,
				issue_id TEXT NOT NULL,
				mission_file TEXT NOT NULL,
				workspace_id TEXT NOT NULL DEFAULT '',
				started_at DATETIME NOT NULL,
				finished_at DATETIME,
				outcome TEXT NOT NULL DEFAULT ''
			)`,
		} {
			if _, err := db.Exec(stmt); err != nil {
				t.Fatalf("migrate: %v", err)
			}
		}
		db.Close()
	}

	// Override global seams; restore after test.
	origOpenDB := openProposalsDBFn
	origOrch := orchestratorRunner
	origTracker := trackerUpdater
	origSelect := selectDispatchableFn
	t.Cleanup(func() {
		openProposalsDBFn = origOpenDB
		orchestratorRunner = origOrch
		trackerUpdater = origTracker
		selectDispatchableFn = origSelect
	})

	openProposalsDBFn = func() (*sql.DB, error) {
		return sql.Open("sqlite", dbPath+"?_journal_mode=WAL&_busy_timeout=5000")
	}

	labels := "auto"
	seqID := int64(99)
	fakeIssue := &trackerIssue{
		ID:     "trk-99",
		SeqID:  &seqID,
		Status: "in-progress",
		Labels: &labels,
	}
	selectDispatchableFn = func(ctx context.Context) (*trackerIssue, string, error) {
		return fakeIssue, "/fake/mission.md", nil
	}
	orchestratorRunner = func(ctx context.Context, missionFile string) (string, error) {
		return "", fmt.Errorf("orchestrator exploded")
	}
	trackerUpdater = func(issueID, status string) error {
		return fmt.Errorf("tracker offline")
	}

	// runDispatch returns nil even on orchestrator failure (failure captured in result).
	_ = runDispatch([]string{})

	// Reopen DB to verify outcome.
	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("reopen db: %v", err)
	}
	defer db.Close()

	var outcome string
	if err := db.QueryRow(`SELECT outcome FROM dispatches ORDER BY id DESC LIMIT 1`).Scan(&outcome); err != nil {
		t.Fatalf("query outcome: %v", err)
	}
	if outcome != "failure-stuck" {
		t.Errorf("expected outcome=failure-stuck, got %q", outcome)
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
