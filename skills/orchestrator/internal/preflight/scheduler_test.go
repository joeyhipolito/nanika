package preflight

import (
	"context"
	"database/sql"
	"path/filepath"
	"strings"
	"testing"

	_ "modernc.org/sqlite"
)

// newTestSchedulerDB creates a temporary scheduler SQLite DB with the minimal
// schema required by schedulerSection. The caller cleans up via t.TempDir.
func newTestSchedulerDB(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	path := filepath.Join(dir, "scheduler.db")

	db, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatalf("open test scheduler db: %v", err)
	}
	defer db.Close()

	_, err = db.Exec(`
		CREATE TABLE jobs (
			id          INTEGER PRIMARY KEY AUTOINCREMENT,
			name        TEXT    NOT NULL UNIQUE,
			schedule    TEXT    NOT NULL DEFAULT '',
			command     TEXT    NOT NULL DEFAULT '',
			enabled     INTEGER NOT NULL DEFAULT 1,
			next_run_at TEXT
		);
		CREATE TABLE runs (
			id          INTEGER PRIMARY KEY AUTOINCREMENT,
			job_id      INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
			status      TEXT    NOT NULL DEFAULT 'pending',
			exit_code   INTEGER,
			started_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
		);
	`)
	if err != nil {
		t.Fatalf("create scheduler tables: %v", err)
	}
	return path
}

func insertSchedulerJob(t *testing.T, dbPath, name string, enabled int, nextRunAt string) int64 {
	t.Helper()
	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("open db for scheduler insert: %v", err)
	}
	defer db.Close()

	var nextVal interface{}
	if nextRunAt != "" {
		nextVal = nextRunAt
	}
	res, err := db.Exec(
		`INSERT INTO jobs (name, enabled, next_run_at) VALUES (?, ?, ?)`,
		name, enabled, nextVal,
	)
	if err != nil {
		t.Fatalf("insert scheduler job %s: %v", name, err)
	}
	id, _ := res.LastInsertId()
	return id
}

func insertSchedulerRun(t *testing.T, dbPath string, jobID int64, status string) {
	t.Helper()
	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("open db for run insert: %v", err)
	}
	defer db.Close()

	_, err = db.Exec(
		`INSERT INTO runs (job_id, status) VALUES (?, ?)`,
		jobID, status,
	)
	if err != nil {
		t.Fatalf("insert run for job %d: %v", jobID, err)
	}
}

func TestSchedulerSection_MissingDB(t *testing.T) {
	t.Setenv("SCHEDULER_DB", "/nonexistent/path/scheduler.db")

	sec := &schedulerSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("expected no error for missing db, got %v", err)
	}
	if blk.Body != "" {
		t.Errorf("expected empty body for missing db, got %q", blk.Body)
	}
	if blk.Title == "" {
		t.Error("expected non-empty title")
	}
}

func TestSchedulerSection_EmptyDB(t *testing.T) {
	path := newTestSchedulerDB(t)
	t.Setenv("SCHEDULER_DB", path)

	sec := &schedulerSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if blk.Body != "" {
		t.Errorf("expected empty body for empty db, got %q", blk.Body)
	}
}

func TestSchedulerSection_OverdueJob(t *testing.T) {
	path := newTestSchedulerDB(t)
	t.Setenv("SCHEDULER_DB", path)

	// past timestamp → overdue
	insertSchedulerJob(t, path, "stale-job", 1, "2000-01-01T00:00:00Z")

	sec := &schedulerSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(blk.Body, "stale-job") {
		t.Errorf("expected overdue job in output, got: %q", blk.Body)
	}
	if !strings.Contains(blk.Body, "overdue") {
		t.Errorf("expected 'overdue' tag in output, got: %q", blk.Body)
	}
}

func TestSchedulerSection_FailedJob(t *testing.T) {
	path := newTestSchedulerDB(t)
	t.Setenv("SCHEDULER_DB", path)

	// future next_run_at — not overdue, but last run failed
	id := insertSchedulerJob(t, path, "broken-job", 1, "2099-12-31T00:00:00Z")
	insertSchedulerRun(t, path, id, "failure")

	sec := &schedulerSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(blk.Body, "broken-job") {
		t.Errorf("expected failed job in output, got: %q", blk.Body)
	}
	if !strings.Contains(blk.Body, "failed") {
		t.Errorf("expected 'failed' tag in output, got: %q", blk.Body)
	}
}

func TestSchedulerSection_TimeoutJob(t *testing.T) {
	path := newTestSchedulerDB(t)
	t.Setenv("SCHEDULER_DB", path)

	id := insertSchedulerJob(t, path, "slow-job", 1, "2099-12-31T00:00:00Z")
	insertSchedulerRun(t, path, id, "timeout")

	sec := &schedulerSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(blk.Body, "slow-job") {
		t.Errorf("expected timeout job in output, got: %q", blk.Body)
	}
}

func TestSchedulerSection_OverdueAndFailed(t *testing.T) {
	path := newTestSchedulerDB(t)
	t.Setenv("SCHEDULER_DB", path)

	id := insertSchedulerJob(t, path, "bad-job", 1, "2000-01-01T00:00:00Z")
	insertSchedulerRun(t, path, id, "failure")

	sec := &schedulerSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(blk.Body, "overdue+failed") {
		t.Errorf("expected 'overdue+failed' tag, got: %q", blk.Body)
	}
}

func TestSchedulerSection_DisabledJobExcluded(t *testing.T) {
	path := newTestSchedulerDB(t)
	t.Setenv("SCHEDULER_DB", path)

	// disabled overdue job — must not appear
	insertSchedulerJob(t, path, "disabled-job", 0, "2000-01-01T00:00:00Z")

	sec := &schedulerSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if strings.Contains(blk.Body, "disabled-job") {
		t.Errorf("disabled job should not appear in output, got: %q", blk.Body)
	}
}

func TestSchedulerSection_HealthyJobExcluded(t *testing.T) {
	path := newTestSchedulerDB(t)
	t.Setenv("SCHEDULER_DB", path)

	// future next_run_at + successful last run — should not appear
	id := insertSchedulerJob(t, path, "healthy-job", 1, "2099-12-31T00:00:00Z")
	insertSchedulerRun(t, path, id, "success")

	sec := &schedulerSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if strings.Contains(blk.Body, "healthy-job") {
		t.Errorf("healthy job should not appear in output, got: %q", blk.Body)
	}
}

func TestSchedulerSection_OnlyLastRunCounts(t *testing.T) {
	path := newTestSchedulerDB(t)
	t.Setenv("SCHEDULER_DB", path)

	// previous run was a failure, but the most recent run succeeded
	id := insertSchedulerJob(t, path, "recovered-job", 1, "2099-12-31T00:00:00Z")
	insertSchedulerRun(t, path, id, "failure")
	insertSchedulerRun(t, path, id, "success")

	sec := &schedulerSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if strings.Contains(blk.Body, "recovered-job") {
		t.Errorf("recovered job should not appear — last run succeeded, got: %q", blk.Body)
	}
}

func TestSchedulerSection_Metadata(t *testing.T) {
	sec := &schedulerSection{}
	if sec.Name() != "scheduler" {
		t.Errorf("expected name 'scheduler', got %q", sec.Name())
	}
	if sec.Priority() <= 0 {
		t.Errorf("expected positive priority, got %d", sec.Priority())
	}
}
