package main

import (
	"context"
	"database/sql"
	"fmt"
	"os"
	"path/filepath"
	"strings"
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

// --- sweep mission dir ALLUKA_HOME tests ---

func TestCloseSweepDir_RespectsAllukaHome(t *testing.T) {
	custom := t.TempDir()
	t.Setenv("ALLUKA_HOME", custom)

	got := remediationMissionDir()

	if !strings.HasPrefix(got, custom) {
		t.Errorf("remediationMissionDir() = %q, want path under ALLUKA_HOME %q", got, custom)
	}
	home, _ := os.UserHomeDir()
	if strings.HasPrefix(got, home) && !strings.HasPrefix(custom, home) {
		t.Errorf("remediationMissionDir() = %q uses $HOME instead of ALLUKA_HOME %q", got, custom)
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

// --- trackerIssueStatus unit tests ---

// writeStubTracker creates a shell script at stubDir/tracker that echoes the
// given JSON payload, prepends stubDir to PATH, and restores PATH on cleanup.
func writeStubTracker(t *testing.T, payload string) {
	t.Helper()
	stubDir := t.TempDir()
	stubPath := filepath.Join(stubDir, "tracker")
	script := "#!/bin/sh\necho '" + payload + "'\n"
	if err := os.WriteFile(stubPath, []byte(script), 0o755); err != nil {
		t.Fatalf("write stub tracker: %v", err)
	}
	orig := os.Getenv("PATH")
	t.Setenv("PATH", stubDir+":"+orig)
}

func TestTrackerIssueStatus_ResolvesByDisplayID(t *testing.T) {
	writeStubTracker(t, `{"items":[{"id":"abc-123","seq_id":42,"status":"open"}]}`)

	status, err := trackerIssueStatus(context.Background(), "TRK-42")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if status != "open" {
		t.Errorf("got %q, want %q", status, "open")
	}
}

func TestTrackerIssueStatus_ResolvesByRawID(t *testing.T) {
	writeStubTracker(t, `{"items":[{"id":"abc-123","seq_id":42,"status":"done"}]}`)

	status, err := trackerIssueStatus(context.Background(), "abc-123")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if status != "done" {
		t.Errorf("got %q, want %q", status, "done")
	}
}

// --- allFindingsSuperseded tests ---

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

// --- findingsRecovered tests ---

// insertFindingFull inserts a finding with full control over source, found_at, and superseded_by.
func insertFindingFull(t *testing.T, db *sql.DB, id, source string, foundAt time.Time, supersededBy string) {
	t.Helper()
	now := time.Now().UTC().Format(time.RFC3339)
	_, err := db.Exec(`INSERT INTO findings
		(id, ability, category, severity, title, description, scope_kind, scope_value, evidence, source, found_at, superseded_by, created_at)
		VALUES (?, 'test', 'test', 'high', 'Test', 'Test desc', 'workspace', 'ws-1', '[]', ?, ?, ?, ?)`,
		id, source, foundAt.UTC().Format(time.RFC3339), supersededBy, now)
	if err != nil {
		t.Fatalf("insert finding %s: %v", id, err)
	}
}

func TestFindingsRecovered(t *testing.T) {
	now := time.Now().UTC()
	stale := now.Add(-72 * time.Hour) // older than 48h findingStaleness
	fresh := now.Add(-1 * time.Hour)  // newer than 48h findingStaleness

	freshBeat := now.Add(-5 * time.Minute) // within 30min scannerFreshness

	type seed struct {
		id, source   string
		foundAt      time.Time
		supersededBy string
	}
	cases := []struct {
		name       string
		seeds      []seed
		ids        []string
		heartbeats map[string]time.Time
		wantResult bool
	}{
		{
			name:       "empty IDs returns false",
			ids:        []string{},
			heartbeats: map[string]time.Time{"scanner-a": freshBeat},
			wantResult: false,
		},
		{
			name: "one scanner dead returns false",
			seeds: []seed{
				{id: "f-1", source: "scanner-alive", foundAt: stale},
				{id: "f-2", source: "scanner-dead", foundAt: stale},
			},
			ids: []string{"f-1", "f-2"},
			heartbeats: map[string]time.Time{
				"scanner-alive": freshBeat,
				// scanner-dead is absent from heartbeats → treated as dead
			},
			wantResult: false,
		},
		{
			name: "all alive and stale found_at returns true",
			seeds: []seed{
				{id: "f-1", source: "scanner-a", foundAt: stale},
				{id: "f-2", source: "scanner-b", foundAt: stale},
			},
			ids: []string{"f-1", "f-2"},
			heartbeats: map[string]time.Time{
				"scanner-a": freshBeat,
				"scanner-b": freshBeat,
			},
			wantResult: true,
		},
		{
			name: "already superseded finding skipped returns false",
			seeds: []seed{
				// f-1 is already superseded — excluded from recovery check.
				{id: "f-1", source: "scanner-a", foundAt: stale, supersededBy: "mission:ws-1"},
			},
			ids: []string{"f-1"},
			heartbeats: map[string]time.Time{
				"scanner-a": freshBeat,
			},
			// Query filters out superseded findings → seen==0 → false.
			wantResult: false,
		},
		{
			name: "missing ID does not block recovery",
			seeds: []seed{
				{id: "f-1", source: "scanner-a", foundAt: stale},
			},
			ids: []string{"f-1", "f-ghost"},
			heartbeats: map[string]time.Time{
				"scanner-a": freshBeat,
			},
			// f-ghost not in DB → ignored; f-1 passes alive+stale → true.
			wantResult: true,
		},
		{
			name: "fresh found_at blocks recovery",
			seeds: []seed{
				{id: "f-1", source: "scanner-a", foundAt: fresh},
			},
			ids: []string{"f-1"},
			heartbeats: map[string]time.Time{
				"scanner-a": freshBeat,
			},
			wantResult: false,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			path, cleanup := withTempFindingsPath(t)
			defer cleanup()

			db := openFindingsDB(t, path)
			for _, s := range tc.seeds {
				insertFindingFull(t, db, s.id, s.source, s.foundAt, s.supersededBy)
			}
			db.Close()

			got, err := findingsRecovered(tc.ids, 48*time.Hour, 30*time.Minute, tc.heartbeats)
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if got != tc.wantResult {
				t.Errorf("findingsRecovered() = %v, want %v", got, tc.wantResult)
			}
		})
	}
}

// --- runCloseSweep recovery branch integration tests ---

// writeMission writes a minimal remediation mission file to dir and returns its path.
func writeMission(t *testing.T, dir, filename, trackerIssue string, findingIDs []string, generatedAt time.Time) string {
	t.Helper()
	idLines := ""
	for _, id := range findingIDs {
		idLines += "  - " + id + "\n"
	}
	content := fmt.Sprintf("---\nsource: shu-propose\ntracker_issue: %s\nfinding_ids:\n%sgenerated_at: \"%s\"\n---\n\n# Fix: test\n",
		trackerIssue, idLines, generatedAt.UTC().Format(time.RFC3339))
	path := filepath.Join(dir, filename)
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatalf("write mission %s: %v", filename, err)
	}
	return path
}

// writeStatsJSON writes a nen-daemon.stats.json file with the given scanner → lastSuccessAt entries.
func writeStatsJSON(t *testing.T, dir string, scanners map[string]time.Time) {
	t.Helper()
	statsDir := filepath.Join(dir, "nen")
	if err := os.MkdirAll(statsDir, 0o755); err != nil {
		t.Fatalf("create stats dir: %v", err)
	}
	obj := `{"scanners":{`
	first := true
	for name, ts := range scanners {
		if !first {
			obj += ","
		}
		obj += fmt.Sprintf(`%q:{"last_success_at":%q}`, name, ts.UTC().Format(time.RFC3339))
		first = false
	}
	obj += "}}"
	if err := os.WriteFile(filepath.Join(statsDir, "nen-daemon.stats.json"), []byte(obj), 0o644); err != nil {
		t.Fatalf("write stats.json: %v", err)
	}
}

func TestRunCloseSweep_RecoveryBranch(t *testing.T) {
	now := time.Now().UTC()

	// Point ALLUKA_HOME at a temp dir so all paths resolve there.
	allukaDir := t.TempDir()
	t.Setenv("ALLUKA_HOME", allukaDir)

	dbPath := filepath.Join(allukaDir, "nen", "findings.db")

	// Seed a finding with stale found_at (3 days ago) — no prior supersession.
	db := openFindingsDB(t, dbPath)
	insertFindingFull(t, db, "f-recovery-1", "test-scanner", now.Add(-72*time.Hour), "")
	db.Close()

	// Write a fresh scanner heartbeat (5 minutes ago).
	writeStatsJSON(t, allukaDir, map[string]time.Time{
		"test-scanner": now.Add(-5 * time.Minute),
	})

	// Write the remediation mission file.
	missionDir := t.TempDir()
	writeMission(t, missionDir, "mission-recovery.md", "TRK-99",
		[]string{"f-recovery-1"}, now.Add(-48*time.Hour))

	// Stub tracker: status=open; update/comment calls succeed (exit 0).
	writeStubTracker(t, `{"items":[{"id":"abc-99","seq_id":99,"status":"open"}]}`)

	report, err := runCloseSweep(context.Background(), sweepOptions{
		MissionDir: missionDir,
		DryRun:     false,
	})
	if err != nil {
		t.Fatalf("runCloseSweep: %v", err)
	}
	if report.SweptToDone != 1 {
		t.Errorf("SweptToDone = %d, want 1; errors: %v", report.SweptToDone, report.Errors)
	}
	if report.Skipped != 0 {
		t.Errorf("Skipped = %d, want 0; errors: %v", report.Skipped, report.Errors)
	}

	// Verify findings.superseded_by was written.
	db2, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("open findings.db for verify: %v", err)
	}
	defer db2.Close()
	var supersededBy string
	if err := db2.QueryRow(`SELECT superseded_by FROM findings WHERE id = 'f-recovery-1'`).Scan(&supersededBy); err != nil {
		t.Fatalf("query superseded_by: %v", err)
	}
	if supersededBy != "sweep:metric-recovered" {
		t.Errorf("superseded_by = %q, want %q", supersededBy, "sweep:metric-recovered")
	}
}

// TestRunCloseSweep_RecoveryBranchDryRun verifies that DryRun=true gates the
// UPDATE: the sweep counts the mission as SweptToDone but does NOT write
// superseded_by to the findings DB.
func TestRunCloseSweep_RecoveryBranchDryRun(t *testing.T) {
	now := time.Now().UTC()

	allukaDir := t.TempDir()
	t.Setenv("ALLUKA_HOME", allukaDir)

	dbPath := filepath.Join(allukaDir, "nen", "findings.db")

	db := openFindingsDB(t, dbPath)
	insertFindingFull(t, db, "f-dry-1", "test-scanner", now.Add(-72*time.Hour), "")
	db.Close()

	writeStatsJSON(t, allukaDir, map[string]time.Time{
		"test-scanner": now.Add(-5 * time.Minute),
	})

	missionDir := t.TempDir()
	writeMission(t, missionDir, "mission-dryrun.md", "TRK-99",
		[]string{"f-dry-1"}, now.Add(-48*time.Hour))

	writeStubTracker(t, `{"items":[{"id":"abc-99","seq_id":99,"status":"open"}]}`)

	report, err := runCloseSweep(context.Background(), sweepOptions{
		MissionDir: missionDir,
		DryRun:     true,
	})
	if err != nil {
		t.Fatalf("runCloseSweep dry-run: %v", err)
	}
	if report.SweptToDone != 1 {
		t.Errorf("SweptToDone = %d, want 1; errors: %v", report.SweptToDone, report.Errors)
	}

	// superseded_by must NOT be written when dry-run is true.
	db2, _ := sql.Open("sqlite", dbPath)
	defer db2.Close()
	var supersededBy string
	db2.QueryRow(`SELECT superseded_by FROM findings WHERE id = 'f-dry-1'`).Scan(&supersededBy)
	if supersededBy != "" {
		t.Errorf("dry-run: superseded_by = %q, want empty (no DB write)", supersededBy)
	}
}
