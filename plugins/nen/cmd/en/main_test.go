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

	"github.com/joeyhipolito/nen/internal/scan"
	_ "modernc.org/sqlite"
)

// ---- 1. Deterministic ID generation ----------------------------------------

func TestFindingID_Deterministic(t *testing.T) {
	tests := []struct {
		category string
		scope    string
	}{
		{"routing-quality", ""},
		{"routing-quality", "/home/user/metrics.db"},
		{"mission-activity", "/home/user/metrics.db"},
		{"binary-freshness", "orchestrator"},
		{"", ""},
	}
	for _, tt := range tests {
		t.Run(fmt.Sprintf("%s/%s", tt.category, tt.scope), func(t *testing.T) {
			id1 := findingID(tt.category, tt.scope)
			id2 := findingID(tt.category, tt.scope)
			if id1 != id2 {
				t.Errorf("not deterministic: got %q then %q", id1, id2)
			}
			if !strings.HasPrefix(id1, "en-") {
				t.Errorf("ID %q missing 'en-' prefix", id1)
			}
		})
	}
}

func TestFindingID_UniquenessAcrossInputs(t *testing.T) {
	// Different scope value → different ID
	a := findingID("routing-quality", "/path/a")
	b := findingID("routing-quality", "/path/b")
	if a == b {
		t.Errorf("different scope values produced the same ID %q", a)
	}

	// Different category, same scope → different ID
	c := findingID("routing-quality", "same")
	d := findingID("mission-activity", "same")
	if c == d {
		t.Errorf("different categories produced the same ID %q", c)
	}
}

// ---- 2. Severity calibration -----------------------------------------------

// newMetricsDB creates an in-memory SQLite database with the phases and
// missions schema pre-populated with the supplied rows.
func newMetricsDB(t *testing.T) *sql.DB {
	t.Helper()
	db, err := sql.Open("sqlite", ":memory:")
	if err != nil {
		t.Fatalf("open in-memory db: %v", err)
	}
	_, err = db.Exec(`
		CREATE TABLE phases (
			id               TEXT PRIMARY KEY,
			persona          TEXT NOT NULL DEFAULT '',
			selection_method TEXT
		);
		CREATE TABLE missions (
			id         TEXT PRIMARY KEY,
			started_at TEXT
		);
	`)
	if err != nil {
		t.Fatalf("create schema: %v", err)
	}
	return db
}

func seedPhases(t *testing.T, db *sql.DB, total, fallback int) {
	t.Helper()
	for i := 0; i < total; i++ {
		method := "scored"
		if i < fallback {
			method = "fallback"
		}
		if _, err := db.Exec(`INSERT INTO phases (id, persona, selection_method) VALUES (?,?,?)`,
			fmt.Sprintf("p%d", i), "some-persona", method); err != nil {
			t.Fatalf("seed phase %d: %v", i, err)
		}
	}
}

func seedLastMission(t *testing.T, db *sql.DB, daysAgo int) {
	t.Helper()
	ts := time.Now().UTC().Add(-time.Duration(daysAgo) * 24 * time.Hour).Format(time.RFC3339)
	if _, err := db.Exec(`INSERT INTO missions (id, started_at) VALUES ('m1', ?)`, ts); err != nil {
		t.Fatalf("seed mission: %v", err)
	}
}

func TestQueryFallbackRate_SeverityThresholds(t *testing.T) {
	// routingFallbackLow=5, routingFallbackMedium=15, routingFallbackHigh=30
	tests := []struct {
		name     string
		total    int
		fallback int
		wantSev  scan.Severity
	}{
		// below low threshold: < 5%
		{"0 of 100 fallback → info", 100, 0, scan.SeverityInfo},
		{"4 of 100 fallback (4%) → info", 100, 4, scan.SeverityInfo},
		// at low threshold: >= 5%
		{"5 of 100 fallback (5%) → low", 100, 5, scan.SeverityLow},
		{"14 of 100 fallback (14%) → low", 100, 14, scan.SeverityLow},
		// at medium threshold: >= 15%
		{"15 of 100 fallback (15%) → medium", 100, 15, scan.SeverityMedium},
		{"29 of 100 fallback (29%) → medium", 100, 29, scan.SeverityMedium},
		// at high threshold: >= 30%
		{"30 of 100 fallback (30%) → high", 100, 30, scan.SeverityHigh},
		{"50 of 100 fallback (50%) → high", 100, 50, scan.SeverityHigh},
		// no data → info
		{"no phases → info", 0, 0, scan.SeverityInfo},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			db := newMetricsDB(t)
			defer db.Close()
			if tt.total > 0 {
				seedPhases(t, db, tt.total, tt.fallback)
			}
			f, errMsg := queryFallbackRate(context.Background(), db, "/test/metrics.db")
			if errMsg != "" {
				t.Fatalf("unexpected error: %s", errMsg)
			}
			if f.Severity != tt.wantSev {
				t.Errorf("fallback=%d/%d: want severity %q, got %q (title: %s)",
					tt.fallback, tt.total, tt.wantSev, f.Severity, f.Title)
			}
			if f.Category != categoryRoutingQuality {
				t.Errorf("want category %q, got %q", categoryRoutingQuality, f.Category)
			}
		})
	}
}

func TestQueryLastMissionDate_SeverityThresholds(t *testing.T) {
	// missionStaleDays=14, missionMediumDays=30, missionHighDays=60
	tests := []struct {
		name    string
		daysAgo int // -1 means no missions
		wantSev scan.Severity
	}{
		// <= missionStaleDays → info
		{"0 days ago → info", 0, scan.SeverityInfo},
		{"14 days ago → info (boundary)", 14, scan.SeverityInfo},
		// > missionStaleDays and <= missionMediumDays → low
		{"15 days ago → low", 15, scan.SeverityLow},
		{"30 days ago → low (boundary)", 30, scan.SeverityLow},
		// > missionMediumDays and <= missionHighDays → medium
		{"31 days ago → medium", 31, scan.SeverityMedium},
		{"60 days ago → medium (boundary)", 60, scan.SeverityMedium},
		// > missionHighDays → high
		{"61 days ago → high", 61, scan.SeverityHigh},
		{"90 days ago → high", 90, scan.SeverityHigh},
		// no missions → info
		{"no missions → info", -1, scan.SeverityInfo},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			db := newMetricsDB(t)
			defer db.Close()
			if tt.daysAgo >= 0 {
				seedLastMission(t, db, tt.daysAgo)
			}
			f, errMsg := queryLastMissionDate(context.Background(), db, "/test/metrics.db")
			if errMsg != "" {
				t.Fatalf("unexpected error: %s", errMsg)
			}
			if f.Severity != tt.wantSev {
				t.Errorf("daysAgo=%d: want severity %q, got %q (title: %s)",
					tt.daysAgo, tt.wantSev, f.Severity, f.Title)
			}
			if f.Category != categoryMissionActivity {
				t.Errorf("want category %q, got %q", categoryMissionActivity, f.Category)
			}
		})
	}
}

// ---- 3. Scope filtering ----------------------------------------------------

func TestSelectChecks_EmptyScope_ReturnsAll(t *testing.T) {
	got := selectChecks(scan.Scope{})
	if len(got) != len(allChecks) {
		t.Errorf("empty scope: want %d checks, got %d", len(allChecks), len(got))
	}
}

func TestSelectChecks_CategoryScope(t *testing.T) {
	tests := []struct {
		category   string
		wantCount  int
		wantNotZero bool
	}{
		{categoryRoutingQuality, 1, true},
		{categoryMissionActivity, 1, true},
		{categoryBinaryFreshness, 1, true},
		{categoryWorkspaceHygiene, 1, true},
		{categoryEmbedding, 1, true},
		{categoryDeadWeight, 1, true},
		{categoryDaemonHealth, 1, true},
		{categorySchedulerHealth, 1, true},
		{"nonexistent-category", 0, false},
	}
	for _, tt := range tests {
		t.Run(tt.category, func(t *testing.T) {
			got := selectChecks(scan.Scope{Kind: "category", Value: tt.category})
			if len(got) != tt.wantCount {
				t.Errorf("category %q: want %d checks, got %d", tt.category, tt.wantCount, len(got))
			}
		})
	}
}

func TestSelectChecks_BinaryScope(t *testing.T) {
	tests := []struct {
		binary    string
		wantCount int
	}{
		// orchestrator → checkOrchestratorBinaryAge, checkDaemonSocket, checkMetricsDB
		{"orchestrator", 3},
		// scheduler → checkSchedulerDaemon
		{"scheduler", 1},
		// nen-daemon → checkDaemonSocket
		{"nen-daemon", 1},
		// unknown → 0
		{"unknown-binary", 0},
	}
	for _, tt := range tests {
		t.Run(tt.binary, func(t *testing.T) {
			got := selectChecks(scan.Scope{Kind: "binary", Value: tt.binary})
			if len(got) != tt.wantCount {
				t.Errorf("binary %q: want %d checks, got %d", tt.binary, tt.wantCount, len(got))
			}
		})
	}
}

func TestSelectChecks_PathScope(t *testing.T) {
	tests := []struct {
		path      string
		wantCount int
	}{
		{"/home/user/.alluka/metrics.db", 1},
		{"/home/user/.alluka/learnings.db", 1},
		// workspaces dir
		{"/home/user/.alluka/workspaces", 1},
		// socket path
		{"/tmp/nen.sock", 1},
		{"scheduler", 1},
		// unrecognised path
		{"/tmp/unknown.db", 0},
	}
	for _, tt := range tests {
		t.Run(tt.path, func(t *testing.T) {
			got := selectChecks(scan.Scope{Kind: "path", Value: tt.path})
			if len(got) != tt.wantCount {
				t.Errorf("path %q: want %d checks, got %d", tt.path, tt.wantCount, len(got))
			}
		})
	}
}

func TestEnScan_NoMatchReturnsError(t *testing.T) {
	_, err := enScan(context.Background(), scan.Scope{Kind: "category", Value: "completely-unknown"})
	if err == nil {
		t.Error("expected error for unmatched scope, got nil")
	}
}

func TestEnScan_CategoryScopeFiltersFindings(t *testing.T) {
	// checkMetricsDB produces both routing-quality and mission-activity.
	// When scoping to routing-quality, mission-activity findings must be absent.
	// We run against a missing metrics.db (returns a single info finding).
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", t.TempDir())

	scope := scan.Scope{Kind: "category", Value: categoryRoutingQuality}
	findings, _ := enScan(context.Background(), scope)
	for _, f := range findings {
		if f.Category != categoryRoutingQuality {
			t.Errorf("category scope %q leaked finding with category %q", categoryRoutingQuality, f.Category)
		}
	}
}

// ---- 4. findings.db persistence and deduplication -------------------------

func TestPersistFindings_WritesRows(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", dir)

	findings := []scan.Finding{
		{
			ID:          "en-abc123",
			Ability:     abilitySystemHealth,
			Category:    categoryRoutingQuality,
			Severity:    scan.SeverityInfo,
			Title:       "test finding",
			Description: "test description",
			Scope:       scan.Scope{Kind: "file", Value: "/test/metrics.db"},
			Source:      "en",
			FoundAt:     time.Now().UTC(),
		},
	}

	if err := scan.PersistFindings(context.Background(), findings); err != nil {
		t.Fatalf("persist: %v", err)
	}

	dbPath := filepath.Join(dir, "nen", "findings.db")
	if _, err := os.Stat(dbPath); err != nil {
		t.Fatalf("findings.db not created at %s: %v", dbPath, err)
	}

	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("open findings.db: %v", err)
	}
	defer db.Close()

	var count int
	if err := db.QueryRow("SELECT COUNT(*) FROM findings WHERE id = ?", "en-abc123").Scan(&count); err != nil {
		t.Fatalf("count query: %v", err)
	}
	if count != 1 {
		t.Errorf("want 1 finding, got %d", count)
	}
}

func TestPersistFindings_Deduplication(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", dir)

	f := scan.Finding{
		ID:          "en-dedup-test",
		Ability:     abilitySystemHealth,
		Category:    categoryMissionActivity,
		Severity:    scan.SeverityLow,
		Title:       "duplicate finding",
		Description: "should only be inserted once",
		Scope:       scan.Scope{Kind: "file", Value: "/test/metrics.db"},
		Source:      "en",
		FoundAt:     time.Now().UTC(),
	}

	// Persist twice.
	for i := 0; i < 2; i++ {
		if err := scan.PersistFindings(context.Background(), []scan.Finding{f}); err != nil {
			t.Fatalf("persist attempt %d: %v", i+1, err)
		}
	}

	dbPath := filepath.Join(dir, "nen", "findings.db")
	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("open findings.db: %v", err)
	}
	defer db.Close()

	var count int
	if err := db.QueryRow("SELECT COUNT(*) FROM findings WHERE id = ?", "en-dedup-test").Scan(&count); err != nil {
		t.Fatalf("count query: %v", err)
	}
	if count != 1 {
		t.Errorf("deduplication failed: want 1 row, got %d", count)
	}
}

func TestPersistFindings_EmptySliceIsNoOp(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", dir)

	if err := scan.PersistFindings(context.Background(), nil); err != nil {
		t.Errorf("nil findings: unexpected error: %v", err)
	}
	if err := scan.PersistFindings(context.Background(), []scan.Finding{}); err != nil {
		t.Errorf("empty slice: unexpected error: %v", err)
	}

	// findings.db should not have been created
	dbPath := filepath.Join(dir, "nen", "findings.db")
	if _, err := os.Stat(dbPath); err == nil {
		t.Error("findings.db should not be created for empty input")
	}
}

func TestPersistFindings_MultipleDistinctIDs(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", dir)

	findings := make([]scan.Finding, 5)
	for i := range findings {
		findings[i] = scan.Finding{
			ID:          fmt.Sprintf("en-multi-%d", i),
			Ability:     abilitySystemHealth,
			Category:    categoryRoutingQuality,
			Severity:    scan.SeverityInfo,
			Title:       fmt.Sprintf("finding %d", i),
			Description: "batch test",
			Scope:       scan.Scope{Kind: "file", Value: "/test.db"},
			Source:      "en",
			FoundAt:     time.Now().UTC(),
		}
	}

	if err := scan.PersistFindings(context.Background(), findings); err != nil {
		t.Fatalf("persist: %v", err)
	}

	dbPath := filepath.Join(dir, "nen", "findings.db")
	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	defer db.Close()

	var count int
	if err := db.QueryRow("SELECT COUNT(*) FROM findings WHERE id LIKE 'en-multi-%'").Scan(&count); err != nil {
		t.Fatalf("count: %v", err)
	}
	if count != 5 {
		t.Errorf("want 5 findings, got %d", count)
	}
}
