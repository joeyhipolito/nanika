package main

import (
	"database/sql"
	"os"
	"path/filepath"
	"testing"
	"time"

	_ "modernc.org/sqlite"
)

// openFindingsDB creates a minimal findings.db at path and returns the DB.
func openFindingsDB(t *testing.T, path string) *sql.DB {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatalf("create dir: %v", err)
	}
	db, err := sql.Open("sqlite", path)
	if err != nil {
		t.Fatalf("open findings db: %v", err)
	}
	if _, err := db.Exec(`CREATE TABLE IF NOT EXISTS findings (
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
	)`); err != nil {
		t.Fatalf("migrate findings db: %v", err)
	}
	return db
}

// insertFinding adds a row directly into findings.
func insertFinding(t *testing.T, db *sql.DB, id, supersededBy string) {
	t.Helper()
	now := time.Now().UTC().Format(time.RFC3339)
	_, err := db.Exec(`INSERT INTO findings
		(id, ability, category, severity, title, description, scope_kind, scope_value, evidence, source, found_at, superseded_by, created_at)
		VALUES (?, 'test', 'test', 'high', 'Test', 'Test desc', 'workspace', 'ws-1', '[]', 'test', ?, ?, ?)`,
		id, now, supersededBy, now)
	if err != nil {
		t.Fatalf("insert finding %s: %v", id, err)
	}
}

// withTempFindingsPath sets ALLUKA_HOME to a temp dir so findingsDBPath()
// returns a predictable path. Returns the path and a cleanup function.
func withTempFindingsPath(t *testing.T) (string, func()) {
	t.Helper()
	dir := t.TempDir()
	path := filepath.Join(dir, "nen", "findings.db")
	orig := os.Getenv("ALLUKA_HOME")
	os.Setenv("ALLUKA_HOME", dir)
	return path, func() {
		if orig == "" {
			os.Unsetenv("ALLUKA_HOME")
		} else {
			os.Setenv("ALLUKA_HOME", orig)
		}
	}
}

// --- supersedeFindings unit tests ---

func TestSupersedeFindings_ZeroAffected(t *testing.T) {
	path, cleanup := withTempFindingsPath(t)
	defer cleanup()

	db := openFindingsDB(t, path)
	db.Close()

	// IDs that don't exist in the DB.
	affected, err := supersedeFindings([]string{"nonexistent-1", "nonexistent-2"}, "mission:test")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if affected != 0 {
		t.Errorf("expected 0 affected, got %d", affected)
	}
}

func TestSupersedeFindings_AllMatch(t *testing.T) {
	path, cleanup := withTempFindingsPath(t)
	defer cleanup()

	db := openFindingsDB(t, path)
	insertFinding(t, db, "f-001", "")
	insertFinding(t, db, "f-002", "")
	insertFinding(t, db, "f-003", "")
	db.Close()

	affected, err := supersedeFindings([]string{"f-001", "f-002", "f-003"}, "mission:ws-abc")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if affected != 3 {
		t.Errorf("expected 3 affected, got %d", affected)
	}

	// Verify superseded_by was written.
	db2, _ := sql.Open("sqlite", path)
	defer db2.Close()
	var count int
	db2.QueryRow(`SELECT COUNT(*) FROM findings WHERE superseded_by = 'mission:ws-abc'`).Scan(&count)
	if count != 3 {
		t.Errorf("expected 3 findings with superseded_by=mission:ws-abc, got %d", count)
	}
}

func TestSupersedeFindings_PartialMatch(t *testing.T) {
	path, cleanup := withTempFindingsPath(t)
	defer cleanup()

	db := openFindingsDB(t, path)
	insertFinding(t, db, "f-001", "")
	insertFinding(t, db, "f-002", "")
	db.Close()

	// Pass 4 IDs but only 2 exist.
	affected, err := supersedeFindings([]string{"f-001", "f-002", "ghost-1", "ghost-2"}, "mission:ws-xyz")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if affected != 2 {
		t.Errorf("expected 2 affected, got %d", affected)
	}
}

func TestRunClose_RefusesOnZeroAffected(t *testing.T) {
	path, cleanup := withTempFindingsPath(t)
	defer cleanup()

	// Create findings.db with no matching IDs.
	db := openFindingsDB(t, path)
	db.Close()

	err := runClose([]string{
		"--tracker-issue", "TRK-99",
		"--finding-ids", "ghost-1,ghost-2",
	})
	if err == nil {
		t.Fatal("expected error when no findings matched, got nil")
	}
	if got := err.Error(); len(got) == 0 {
		t.Error("error message was empty")
	}
}

// --- parseRemediationMissionMeta tests ---

func writeTempMission(t *testing.T, content string) string {
	t.Helper()
	f, err := os.CreateTemp(t.TempDir(), "mission-*.md")
	if err != nil {
		t.Fatalf("create temp mission: %v", err)
	}
	if _, err := f.WriteString(content); err != nil {
		t.Fatalf("write temp mission: %v", err)
	}
	f.Close()
	return f.Name()
}

func TestParseRemediationMissionMeta_Valid(t *testing.T) {
	path := writeTempMission(t, `---
source: shu-propose
tracker_issue: TRK-42
finding_ids:
  - f-abc
  - f-def
generated_at: "2026-04-01T10:00:00Z"
---

# Fix: something
`)
	meta, err := parseRemediationMissionMeta(path)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if meta.TrackerIssue != "TRK-42" {
		t.Errorf("tracker_issue: got %q, want TRK-42", meta.TrackerIssue)
	}
	if len(meta.FindingIDs) != 2 || meta.FindingIDs[0] != "f-abc" || meta.FindingIDs[1] != "f-def" {
		t.Errorf("finding_ids: got %v, want [f-abc f-def]", meta.FindingIDs)
	}
	if meta.GeneratedAt.Year() != 2026 {
		t.Errorf("generated_at year: got %d, want 2026", meta.GeneratedAt.Year())
	}
}

func TestParseRemediationMissionMeta_NoFrontmatter(t *testing.T) {
	path := writeTempMission(t, "# Just a title\nNo frontmatter here.\n")
	_, err := parseRemediationMissionMeta(path)
	if err == nil {
		t.Fatal("expected error for missing frontmatter, got nil")
	}
}

func TestParseRemediationMissionMeta_MalformedYAML(t *testing.T) {
	// Valid delimiters but garbled content — should not panic.
	path := writeTempMission(t, "---\n: this is not valid yaml\n---\n")
	_, err := parseRemediationMissionMeta(path)
	// Malformed content missing tracker_issue/finding_ids → error expected.
	if err == nil {
		t.Fatal("expected error for malformed/missing fields, got nil")
	}
}

func TestParseRemediationMissionMeta_MissingRequiredFields(t *testing.T) {
	path := writeTempMission(t, "---\ngenerated_at: \"2026-04-01T00:00:00Z\"\n---\n")
	_, err := parseRemediationMissionMeta(path)
	if err == nil {
		t.Fatal("expected error when tracker_issue and finding_ids are missing")
	}
}

// --- allFindingsSuperseded tests ---

func TestAllFindingsSuperseded_AllResolved(t *testing.T) {
	path, cleanup := withTempFindingsPath(t)
	defer cleanup()

	db := openFindingsDB(t, path)
	insertFinding(t, db, "f-1", "mission:ws-1")
	insertFinding(t, db, "f-2", "mission:ws-1")
	db.Close()

	ok, err := allFindingsSuperseded([]string{"f-1", "f-2"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !ok {
		t.Error("expected all superseded, got false")
	}
}

func TestAllFindingsSuperseded_OneActive(t *testing.T) {
	path, cleanup := withTempFindingsPath(t)
	defer cleanup()

	db := openFindingsDB(t, path)
	insertFinding(t, db, "f-1", "mission:ws-1")
	insertFinding(t, db, "f-2", "") // still active
	db.Close()

	ok, err := allFindingsSuperseded([]string{"f-1", "f-2"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if ok {
		t.Error("expected not all superseded (one active), got true")
	}
}

func TestAllFindingsSuperseded_MissingIDs(t *testing.T) {
	path, cleanup := withTempFindingsPath(t)
	defer cleanup()

	db := openFindingsDB(t, path)
	insertFinding(t, db, "f-1", "mission:ws-1")
	db.Close()

	// f-ghost doesn't exist in the DB — treated as superseded.
	ok, err := allFindingsSuperseded([]string{"f-1", "f-ghost"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !ok {
		t.Error("expected all superseded (missing IDs treated as resolved), got false")
	}
}
