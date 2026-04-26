package preflight

import (
	"context"
	"database/sql"
	"fmt"
	"path/filepath"
	"strings"
	"testing"

	_ "modernc.org/sqlite"
)

// newTestTrackerDB creates a temporary SQLite DB with the tracker schema and
// returns the path. The caller is responsible for cleanup via t.Cleanup.
func newTestTrackerDB(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	path := filepath.Join(dir, "tracker.db")

	db, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatalf("open test db: %v", err)
	}
	defer db.Close()

	_, err = db.Exec(`
		CREATE TABLE issues (
			id TEXT PRIMARY KEY,
			title TEXT NOT NULL,
			description TEXT,
			status TEXT NOT NULL DEFAULT 'open',
			priority TEXT,
			labels TEXT,
			assignee TEXT,
			created_at TEXT NOT NULL,
			updated_at TEXT NOT NULL,
			parent_id TEXT,
			seq_id INTEGER
		)
	`)
	if err != nil {
		t.Fatalf("create issues table: %v", err)
	}
	return path
}

func insertIssue(t *testing.T, dbPath, id, title, status, priority, assignee string) {
	t.Helper()
	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("open db for insert: %v", err)
	}
	defer db.Close()
	_, err = db.Exec(
		`INSERT INTO issues (id, title, status, priority, assignee, created_at, updated_at)
		 VALUES (?, ?, ?, ?, ?, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')`,
		id, title, status, priority, assignee,
	)
	if err != nil {
		t.Fatalf("insert issue %s: %v", id, err)
	}
}

func TestTrackerSection_MissingDB(t *testing.T) {
	t.Setenv("TRACKER_DB", "/nonexistent/path/tracker.db")

	sec := &trackerSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("expected no error for missing db, got %v", err)
	}
	if blk.Body != "" {
		t.Errorf("expected empty body for missing db, got %q", blk.Body)
	}
}

func TestTrackerSection_EmptyDB(t *testing.T) {
	path := newTestTrackerDB(t)
	t.Setenv("TRACKER_DB", path)

	sec := &trackerSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if blk.Body != "" {
		t.Errorf("expected empty body for empty db, got %q", blk.Body)
	}
	if blk.Title == "" {
		t.Error("expected non-empty title")
	}
}

func TestTrackerSection_OnlyP0P1Returned(t *testing.T) {
	path := newTestTrackerDB(t)
	t.Setenv("TRACKER_DB", path)

	insertIssue(t, path, "trk-001", "Critical bug", "open", "P0", "alice")
	insertIssue(t, path, "trk-002", "High priority", "open", "P1", "bob")
	insertIssue(t, path, "trk-003", "Normal issue", "open", "P2", "carol")
	insertIssue(t, path, "trk-004", "Low priority", "open", "P3", "")

	sec := &trackerSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if !strings.Contains(blk.Body, "trk-001") {
		t.Error("expected P0 issue in output")
	}
	if !strings.Contains(blk.Body, "trk-002") {
		t.Error("expected P1 issue in output")
	}
	if strings.Contains(blk.Body, "trk-003") {
		t.Error("P2 issue should not appear in output")
	}
	if strings.Contains(blk.Body, "trk-004") {
		t.Error("P3 issue should not appear in output")
	}
}

func TestTrackerSection_ClosedIssuesExcluded(t *testing.T) {
	path := newTestTrackerDB(t)
	t.Setenv("TRACKER_DB", path)

	insertIssue(t, path, "trk-010", "Open P0", "open", "P0", "")
	insertIssue(t, path, "trk-011", "Closed P0", "closed", "P0", "")

	sec := &trackerSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if !strings.Contains(blk.Body, "trk-010") {
		t.Error("expected open P0 in output")
	}
	if strings.Contains(blk.Body, "trk-011") {
		t.Error("closed issue should not appear in output")
	}
}

func TestTrackerSection_AssigneeFormatting(t *testing.T) {
	path := newTestTrackerDB(t)
	t.Setenv("TRACKER_DB", path)

	insertIssue(t, path, "trk-020", "Assigned issue", "open", "P0", "alice")
	insertIssue(t, path, "trk-021", "Unassigned issue", "open", "P1", "")

	sec := &trackerSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if !strings.Contains(blk.Body, "@alice") {
		t.Errorf("expected @alice in output, got: %q", blk.Body)
	}
	// Unassigned issue should not have a trailing @ symbol
	lines := strings.Split(blk.Body, "\n")
	for _, line := range lines {
		if strings.Contains(line, "trk-021") && strings.Contains(line, "@") {
			t.Errorf("unassigned issue should not have @, got: %q", line)
		}
	}
}

func TestTrackerSection_LimitTen(t *testing.T) {
	path := newTestTrackerDB(t)
	t.Setenv("TRACKER_DB", path)

	for i := 0; i < 15; i++ {
		id := fmt.Sprintf("trk-%03d", i)
		insertIssue(t, path, id, "Issue "+id, "open", "P0", "")
	}

	sec := &trackerSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	lines := strings.Split(strings.TrimSpace(blk.Body), "\n")
	if len(lines) > 10 {
		t.Errorf("expected at most 10 issues, got %d", len(lines))
	}
}

func TestTrackerSection_Metadata(t *testing.T) {
	sec := &trackerSection{}
	if sec.Name() != "tracker" {
		t.Errorf("expected name 'tracker', got %q", sec.Name())
	}
	if sec.Priority() <= 0 {
		t.Errorf("expected positive priority, got %d", sec.Priority())
	}
}

func TestTrackerSection_ContextCancelled(t *testing.T) {
	path := newTestTrackerDB(t)
	t.Setenv("TRACKER_DB", path)

	insertIssue(t, path, "trk-100", "Some issue", "open", "P0", "")

	ctx, cancel := context.WithCancel(context.Background())
	cancel()

	sec := &trackerSection{}
	// Cancelled context may return an error or empty block — both are acceptable.
	// What must NOT happen is a panic.
	_, _ = sec.Fetch(ctx)
}

func insertIssueWithSeqID(t *testing.T, dbPath, id, title, status, priority, assignee string, seqID *int) {
	t.Helper()
	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("open db for insert: %v", err)
	}
	defer db.Close()
	_, err = db.Exec(
		`INSERT INTO issues (id, title, status, priority, assignee, seq_id, created_at, updated_at)
		 VALUES (?, ?, ?, ?, ?, ?, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')`,
		id, title, status, priority, assignee, seqID,
	)
	if err != nil {
		t.Fatalf("insert issue %s: %v", id, err)
	}
}

func TestTrackerBlock_DisplayIdUsesSeqId(t *testing.T) {
	path := newTestTrackerDB(t)
	t.Setenv("TRACKER_DB", path)

	seq := 42
	insertIssueWithSeqID(t, path, "trk-ABCD", "Seq ID issue", "open", "P0", "", &seq)

	sec := &trackerSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if !strings.Contains(blk.Body, "[TRK-42]") {
		t.Errorf("expected [TRK-42] in body, got: %q", blk.Body)
	}
	if strings.Contains(blk.Body, "[trk-ABCD]") {
		t.Errorf("hash ID should not appear when seq_id is set, got: %q", blk.Body)
	}
}

func TestTrackerBlock_DisplayIdFallsBackToHash(t *testing.T) {
	path := newTestTrackerDB(t)
	t.Setenv("TRACKER_DB", path)

	insertIssueWithSeqID(t, path, "trk-XYZ1", "No seq ID issue", "open", "P0", "", nil)

	sec := &trackerSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if !strings.Contains(blk.Body, "[trk-XYZ1]") {
		t.Errorf("expected hash ID [trk-XYZ1] in body when seq_id is NULL, got: %q", blk.Body)
	}
}

func TestTrackerBlock_LimitOverride(t *testing.T) {
	path := newTestTrackerDB(t)
	t.Setenv("TRACKER_DB", path)
	t.Setenv("NANIKA_PREFLIGHT_TRACKER_LIMIT", "15")

	for i := 0; i < 30; i++ {
		id := fmt.Sprintf("trk-l%03d", i)
		seq := i + 100
		insertIssueWithSeqID(t, path, id, "Issue "+id, "open", "P0", "", &seq)
	}

	sec := &trackerSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	lines := strings.Split(strings.TrimSpace(blk.Body), "\n")
	// Count non-truncation lines
	issueLines := 0
	hasTruncation := false
	for _, l := range lines {
		if strings.Contains(l, "showing") && strings.Contains(l, "NANIKA_PREFLIGHT_TRACKER_LIMIT") {
			hasTruncation = true
		} else if strings.HasPrefix(l, "- [") {
			issueLines++
		}
	}

	if issueLines != 15 {
		t.Errorf("expected 15 issue lines, got %d", issueLines)
	}
	if !hasTruncation {
		t.Errorf("expected truncation notice in body, got: %q", blk.Body)
	}
}

func TestTrackerBlock_NoTruncationWhenWithinLimit(t *testing.T) {
	path := newTestTrackerDB(t)
	t.Setenv("TRACKER_DB", path)

	for i := 0; i < 5; i++ {
		id := fmt.Sprintf("trk-nt%03d", i)
		seq := i + 200
		insertIssueWithSeqID(t, path, id, "Issue "+id, "open", "P1", "", &seq)
	}

	sec := &trackerSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if strings.Contains(blk.Body, "showing") {
		t.Errorf("expected no truncation notice for 5 rows within limit, got: %q", blk.Body)
	}
}

func TestTrackerBlock_DefaultLimit25(t *testing.T) {
	path := newTestTrackerDB(t)
	t.Setenv("TRACKER_DB", path)

	for i := 0; i < 30; i++ {
		id := fmt.Sprintf("trk-d%03d", i)
		seq := i + 300
		insertIssueWithSeqID(t, path, id, "Issue "+id, "open", "P0", "", &seq)
	}

	sec := &trackerSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	lines := strings.Split(strings.TrimSpace(blk.Body), "\n")
	issueLines := 0
	for _, l := range lines {
		if strings.HasPrefix(l, "- [TRK-") {
			issueLines++
		}
	}

	if issueLines != 25 {
		t.Errorf("expected 25 issue lines with default limit, got %d", issueLines)
	}
}
