package db

import (
	"context"
	"encoding/json"
	"path/filepath"
	"strings"
	"testing"
)

// openTestDB opens a fresh on-disk scheduler DB in a temp dir. We use a file
// (not :memory:) so the modernc SQLite driver behaves identically to prod,
// and each test gets its own path to avoid migration collisions.
func openTestDB(t *testing.T) *DB {
	t.Helper()
	dir := t.TempDir()
	d, err := Open(filepath.Join(dir, "scheduler.db"))
	if err != nil {
		t.Fatalf("opening test db: %v", err)
	}
	t.Cleanup(func() { _ = d.Close() })
	return d
}

func TestJobAudit_CreateRecordsRow(t *testing.T) {
	d := openTestDB(t)
	ctx := context.Background()

	id, err := d.CreateJob(ctx, "test-job", "echo hi", "0 * * * *", "/bin/sh", "", "cron", 0)
	if err != nil {
		t.Fatalf("CreateJob: %v", err)
	}

	rows, err := d.ListJobAudit(ctx, id, 10)
	if err != nil {
		t.Fatalf("ListJobAudit: %v", err)
	}
	if len(rows) != 1 {
		t.Fatalf("expected 1 audit row, got %d", len(rows))
	}
	r := rows[0]
	if r.Op != "create" {
		t.Errorf("op = %q, want create", r.Op)
	}
	if r.BeforeJSON != "" {
		t.Errorf("BeforeJSON should be empty for create, got %q", r.BeforeJSON)
	}
	if !strings.Contains(r.AfterJSON, `"name":"test-job"`) {
		t.Errorf("AfterJSON missing name: %s", r.AfterJSON)
	}
	if !strings.Contains(r.AfterJSON, `"command":"echo hi"`) {
		t.Errorf("AfterJSON missing command: %s", r.AfterJSON)
	}
	if r.Actor == "" {
		t.Errorf("actor should not be empty")
	}
}

func TestJobAudit_UpdateRecordsBeforeAndAfter(t *testing.T) {
	d := openTestDB(t)
	ctx := context.Background()

	id, err := d.CreateJob(ctx, "toggle-me", "echo hi", "0 * * * *", "/bin/sh", "", "cron", 0)
	if err != nil {
		t.Fatalf("CreateJob: %v", err)
	}

	if err := d.EnableJob(ctx, id, false); err != nil {
		t.Fatalf("EnableJob: %v", err)
	}

	rows, err := d.ListJobAudit(ctx, id, 10)
	if err != nil {
		t.Fatalf("ListJobAudit: %v", err)
	}
	// Newest first: [update, create]
	if len(rows) != 2 {
		t.Fatalf("expected 2 audit rows, got %d", len(rows))
	}
	update := rows[0]
	if update.Op != "update" {
		t.Errorf("op[0] = %q, want update", update.Op)
	}
	if !strings.Contains(update.BeforeJSON, `"enabled":true`) {
		t.Errorf("BeforeJSON should show enabled=true: %s", update.BeforeJSON)
	}
	if !strings.Contains(update.AfterJSON, `"enabled":false`) {
		t.Errorf("AfterJSON should show enabled=false: %s", update.AfterJSON)
	}
}

func TestJobAudit_DeleteRecordsBeforeOnly(t *testing.T) {
	d := openTestDB(t)
	ctx := context.Background()

	id, err := d.CreateJob(ctx, "doomed", "rm -rf /nowhere", "0 * * * *", "/bin/sh", "", "cron", 0)
	if err != nil {
		t.Fatalf("CreateJob: %v", err)
	}
	if err := d.DeleteJob(ctx, id); err != nil {
		t.Fatalf("DeleteJob: %v", err)
	}

	// This is the scenario from the 2026-04-06 persona drift investigation:
	// the job is gone, but the audit row lets us still reconstruct what
	// command it was running.
	rows, err := d.ListJobAudit(ctx, id, 10)
	if err != nil {
		t.Fatalf("ListJobAudit: %v", err)
	}
	if len(rows) != 2 {
		t.Fatalf("expected 2 rows (create + delete), got %d", len(rows))
	}
	del := rows[0]
	if del.Op != "delete" {
		t.Errorf("op[0] = %q, want delete", del.Op)
	}
	if del.AfterJSON != "" {
		t.Errorf("AfterJSON should be empty for delete, got %q", del.AfterJSON)
	}
	if !strings.Contains(del.BeforeJSON, `"command":"rm -rf /nowhere"`) {
		t.Errorf("BeforeJSON should preserve deleted command: %s", del.BeforeJSON)
	}
}

func TestJobAudit_ListAllAcrossJobs(t *testing.T) {
	d := openTestDB(t)
	ctx := context.Background()

	if _, err := d.CreateJob(ctx, "a", "echo a", "0 * * * *", "/bin/sh", "", "cron", 0); err != nil {
		t.Fatal(err)
	}
	if _, err := d.CreateJob(ctx, "b", "echo b", "0 * * * *", "/bin/sh", "", "cron", 0); err != nil {
		t.Fatal(err)
	}

	all, err := d.ListJobAudit(ctx, 0, 10)
	if err != nil {
		t.Fatalf("ListJobAudit all: %v", err)
	}
	if len(all) != 2 {
		t.Fatalf("expected 2 rows total, got %d", len(all))
	}
}

func TestJobAudit_SnapshotIsValidJSON(t *testing.T) {
	d := openTestDB(t)
	ctx := context.Background()

	id, err := d.CreateJob(ctx, "json-check", "echo hi", "*/5 * * * *", "/bin/sh", "", "cron", 120)
	if err != nil {
		t.Fatal(err)
	}
	rows, err := d.ListJobAudit(ctx, id, 1)
	if err != nil {
		t.Fatal(err)
	}
	if len(rows) == 0 {
		t.Fatal("no audit rows")
	}
	var decoded auditableJob
	if err := json.Unmarshal([]byte(rows[0].AfterJSON), &decoded); err != nil {
		t.Fatalf("after_json is not valid JSON: %v — raw: %s", err, rows[0].AfterJSON)
	}
	if decoded.TimeoutSec != 120 {
		t.Errorf("timeout_sec round-trip: got %d, want 120", decoded.TimeoutSec)
	}
}
