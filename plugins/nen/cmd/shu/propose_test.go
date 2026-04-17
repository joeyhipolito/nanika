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

// evidenceWithSource returns a JSON evidence array with one item that has a source field.
func evidenceWithSource(raw, source string) string {
	return fmt.Sprintf(`[{"kind":"log","raw":%q,"source":%q,"captured_at":"2026-01-01T00:00:00Z"}]`, raw, source)
}

// seedFindingsDB creates a minimal findings.db at dbPath with the given rows.
// Each row is a map with keys matching findings table columns.
func seedFindingsDB(t *testing.T, dbPath string, rows []map[string]string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(dbPath), 0o755); err != nil {
		t.Fatalf("creating db dir: %v", err)
	}
	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("open db: %v", err)
	}
	defer db.Close()
	_, err = db.Exec(`CREATE TABLE IF NOT EXISTS findings (
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
	)`)
	if err != nil {
		t.Fatalf("create table: %v", err)
	}
	now := time.Now().UTC().Format(time.RFC3339)
	for _, row := range rows {
		createdAt := row["created_at"]
		if createdAt == "" {
			createdAt = now
		}
		supersededBy := row["superseded_by"] // defaults to empty string per schema
		_, err = db.Exec(`INSERT INTO findings
			(id, ability, category, severity, title, description, scope_kind, scope_value, evidence, source, found_at, superseded_by, created_at)
			VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
			row["id"], row["ability"], row["category"], row["severity"],
			row["title"], row["description"], row["scope_kind"], row["scope_value"],
			row["evidence"], row["source"], row["found_at"], supersededBy, createdAt)
		if err != nil {
			t.Fatalf("insert finding %s: %v", row["id"], err)
		}
	}
}

// --- computeDedupKey ---

func TestComputeDedupKey_IncludesScopeValue(t *testing.T) {
	// Two findings with same ability+category+scopeKind but different scope_value must produce different keys.
	base := proposableFinding{Ability: "shu", Category: "review-blocker", ScopeKind: "workspace"}

	a := base
	a.ScopeValue = "ws-aaaaaaaa"
	b := base
	b.ScopeValue = "ws-bbbbbbbb"

	keyA := computeDedupKey(a)
	keyB := computeDedupKey(b)

	if keyA == keyB {
		t.Errorf("expected different dedup keys for different scope_value, got both %q", keyA)
	}
}

func TestComputeDedupKey_SameInputProducesSameKey(t *testing.T) {
	f := proposableFinding{Ability: "shu", Category: "review-blocker", ScopeKind: "workspace", ScopeValue: "ws-aabbccdd"}
	if computeDedupKey(f) != computeDedupKey(f) {
		t.Error("expected identical inputs to produce the same dedup key")
	}
}

// --- groupReviewBlockers ---

func TestGroupReviewBlockers_GroupsByWorkspace(t *testing.T) {
	findings := []proposableFinding{
		{ID: "f1", ScopeValue: "ws-aaa"},
		{ID: "f2", ScopeValue: "ws-bbb"},
		{ID: "f3", ScopeValue: "ws-aaa"},
	}
	groups := groupReviewBlockers(findings)
	if len(groups) != 2 {
		t.Fatalf("expected 2 workspace groups, got %d", len(groups))
	}
	if len(groups["ws-aaa"]) != 2 {
		t.Errorf("expected 2 findings in ws-aaa, got %d", len(groups["ws-aaa"]))
	}
	if len(groups["ws-bbb"]) != 1 {
		t.Errorf("expected 1 finding in ws-bbb, got %d", len(groups["ws-bbb"]))
	}
}

func TestGroupReviewBlockers_EmptyInput(t *testing.T) {
	groups := groupReviewBlockers(nil)
	if len(groups) != 0 {
		t.Errorf("expected empty map, got %d entries", len(groups))
	}
}

// --- readWorkspaceIteration ---

func TestReadWorkspaceIteration(t *testing.T) {
	tests := []struct {
		name        string
		content     string // empty means no file
		wantIter    int
		wantErr     bool
	}{
		{
			name:     "missing file returns 0",
			content:  "",
			wantIter: 0,
		},
		{
			name: "iteration present",
			content: `---
source: orchestrator
iteration: 2
domain: dev
---

# Mission`,
			wantIter: 2,
		},
		{
			name: "iteration zero",
			content: `---
iteration: 0
---`,
			wantIter: 0,
		},
		{
			name: "no iteration field returns 0",
			content: `---
source: orchestrator
domain: dev
---`,
			wantIter: 0,
		},
		{
			name: "malformed iteration returns error",
			content: `---
iteration: notanumber
---`,
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tmpHome := t.TempDir()
			t.Setenv("HOME", tmpHome)

			workspaceID := "20260101-testtest"
			wsDir := filepath.Join(tmpHome, ".alluka", "workspaces", workspaceID)

			if tt.content != "" {
				if err := os.MkdirAll(wsDir, 0o755); err != nil {
					t.Fatalf("mkdir: %v", err)
				}
				if err := os.WriteFile(filepath.Join(wsDir, "mission.md"), []byte(tt.content), 0o644); err != nil {
					t.Fatalf("write mission.md: %v", err)
				}
			}

			got, err := readWorkspaceIteration(workspaceID)
			if tt.wantErr {
				if err == nil {
					t.Fatal("expected error, got nil")
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if got != tt.wantIter {
				t.Errorf("readWorkspaceIteration() = %d, want %d", got, tt.wantIter)
			}
		})
	}
}

// --- generateReviewBlockerMission ---

func TestGenerateReviewBlockerMission_HasCorrectPHASELines(t *testing.T) {
	findings := []proposableFinding{
		{
			ID:       "fn-001",
			Title:    "nil pointer dereference",
			Evidence: []evidenceItem{{Raw: "pkg/foo/bar.go:42 nil pointer", Source: "pkg/foo/bar.go"}},
		},
	}

	content, err := generateReviewBlockerMission("ws-testtest", findings, 1, "TRK-99")
	if err != nil {
		t.Fatalf("generateReviewBlockerMission: %v", err)
	}

	// Verify frontmatter fields
	for _, want := range []string{
		"source: shu-propose",
		"tracker_issue: TRK-99",
		"- fn-001",
		"severity: high",
		"category: review-blocker",
		"origin_workspace: ws-testtest",
		"iteration: 1",
		"domain: dev",
	} {
		if !strings.Contains(content, want) {
			t.Errorf("generated mission missing %q\nContent:\n%s", want, content)
		}
	}

	// Verify 3 PHASE lines exist
	phaseCount := strings.Count(content, "\nPHASE:")
	if phaseCount != 3 {
		t.Errorf("expected 3 PHASE lines, got %d\nContent:\n%s", phaseCount, content)
	}

	// Verify PHASE order and DEPENDS
	if !strings.Contains(content, "PHASE: fix |") {
		t.Error("missing PHASE: fix")
	}
	if !strings.Contains(content, "PHASE: test |") {
		t.Error("missing PHASE: test")
	}
	if !strings.Contains(content, "PHASE: review |") {
		t.Error("missing PHASE: review")
	}
	if !strings.Contains(content, "DEPENDS: fix") {
		t.Error("missing DEPENDS: fix on test phase")
	}
	if !strings.Contains(content, "DEPENDS: test") {
		t.Error("missing DEPENDS: test on review phase")
	}
}

func TestGenerateReviewBlockerMission_BlockerDetailsEmbedded(t *testing.T) {
	// Evidence source:raw should appear in mission body
	findings := []proposableFinding{
		{
			ID:    "fn-002",
			Title: "missing error check",
			Evidence: []evidenceItem{
				{Raw: "unchecked error at line 77", Source: "cmd/shu/main.go"},
			},
		},
	}

	content, err := generateReviewBlockerMission("ws-blocktest", findings, 0, "(dry-run)")
	if err != nil {
		t.Fatalf("generateReviewBlockerMission: %v", err)
	}

	if !strings.Contains(content, "cmd/shu/main.go") {
		t.Error("expected evidence source to appear in mission content")
	}
	if !strings.Contains(content, "unchecked error at line 77") {
		t.Error("expected evidence raw to appear in mission content")
	}
}

// --- findBlockingIssue (status + age dedup policy) ---
//
// Regression guard: tracker issues with a dedup label previously suppressed new
// proposals forever. See findBlockingIssue for the status × age policy these
// tests exercise.

func makeTrackerIssueWithLabel(id, status, dedupKey string, age time.Duration, now time.Time) trackerIssue {
	labels := "auto,nen,dedup:" + dedupKey
	updatedAt := now.Add(-age).UTC().Format(time.RFC3339)
	return trackerIssue{
		ID:        id,
		Title:     "fixture",
		Status:    status,
		Labels:    &labels,
		UpdatedAt: updatedAt,
	}
}

func TestFindBlockingIssue_InProgress(t *testing.T) {
	now := time.Date(2026, 4, 5, 12, 0, 0, 0, time.UTC)
	key := "dkey000000000001"
	issues := []trackerIssue{makeTrackerIssueWithLabel("trk-inprog", "in-progress", key, 30*24*time.Hour, now)}

	got := findBlockingIssue(issues, key, defaultDedupPolicy(), now)
	if got == "" {
		t.Error("expected in-progress issue to block, got empty string")
	}
}

func TestFindBlockingIssue_OpenWithinTTL(t *testing.T) {
	now := time.Date(2026, 4, 5, 12, 0, 0, 0, time.UTC)
	key := "dkey000000000002"
	issues := []trackerIssue{makeTrackerIssueWithLabel("trk-open-fresh", "open", key, 12*time.Hour, now)}

	got := findBlockingIssue(issues, key, defaultDedupPolicy(), now)
	if got == "" {
		t.Error("expected 12h-old open issue to block, got empty string")
	}
}

func TestFindBlockingIssue_OpenAlwaysBlocks(t *testing.T) {
	// Open issues always block regardless of age — the issue is unresolved.
	now := time.Date(2026, 4, 5, 12, 0, 0, 0, time.UTC)
	key := "dkey000000000003"
	issues := []trackerIssue{makeTrackerIssueWithLabel("trk-open-stale", "open", key, 48*time.Hour, now)}

	got := findBlockingIssue(issues, key, defaultDedupPolicy(), now)
	if got == "" {
		t.Error("expected open issue to block regardless of age, got empty string")
	}
}

func TestFindBlockingIssue_CancelledWithinTTL(t *testing.T) {
	// Protects an explicit human rejection from being immediately re-proposed.
	now := time.Date(2026, 4, 5, 12, 0, 0, 0, time.UTC)
	key := "dkey000000000004"
	issues := []trackerIssue{makeTrackerIssueWithLabel("trk-cancel-recent", "cancelled", key, 3*24*time.Hour, now)}

	got := findBlockingIssue(issues, key, defaultDedupPolicy(), now)
	if got == "" {
		t.Error("expected 3d-old cancelled issue to block, got empty string")
	}
}

func TestFindBlockingIssue_CancelledExpiredTTL(t *testing.T) {
	now := time.Date(2026, 4, 5, 12, 0, 0, 0, time.UTC)
	key := "dkey000000000005"
	issues := []trackerIssue{makeTrackerIssueWithLabel("trk-cancel-stale", "cancelled", key, 10*24*time.Hour, now)}

	got := findBlockingIssue(issues, key, defaultDedupPolicy(), now)
	if got != "" {
		t.Errorf("expected 10d-old cancelled issue to NOT block, got %q", got)
	}
}

func TestFindBlockingIssue_Done(t *testing.T) {
	// Recurrence means the fix did not stick, so re-propose — never block on done.
	now := time.Date(2026, 4, 5, 12, 0, 0, 0, time.UTC)
	key := "dkey000000000006"
	issues := []trackerIssue{makeTrackerIssueWithLabel("trk-done", "done", key, time.Second, now)}

	got := findBlockingIssue(issues, key, defaultDedupPolicy(), now)
	if got != "" {
		t.Errorf("expected done issue to NOT block, got %q", got)
	}
}

func TestFindBlockingIssue_Closed(t *testing.T) {
	now := time.Date(2026, 4, 5, 12, 0, 0, 0, time.UTC)
	key := "dkey000000000007"
	issues := []trackerIssue{makeTrackerIssueWithLabel("trk-closed", "closed", key, 5*time.Minute, now)}

	got := findBlockingIssue(issues, key, defaultDedupPolicy(), now)
	if got != "" {
		t.Errorf("expected closed issue to NOT block, got %q", got)
	}
}

func TestFindBlockingIssue_NoMatchingLabel(t *testing.T) {
	now := time.Date(2026, 4, 5, 12, 0, 0, 0, time.UTC)
	issues := []trackerIssue{makeTrackerIssueWithLabel("trk-other", "in-progress", "otherkey00000000", time.Hour, now)}

	got := findBlockingIssue(issues, "mismatchkey00000", defaultDedupPolicy(), now)
	if got != "" {
		t.Errorf("expected empty for mismatched key, got %q", got)
	}
}

func TestFindBlockingIssue_UnparseableUpdatedAt(t *testing.T) {
	// Open and in-progress always block regardless of updated_at quality.
	// Cancelled with broken updated_at should fail open (not block).
	key := "dkey000000000008"
	labels := "auto,nen,dedup:" + key
	now := time.Date(2026, 4, 5, 12, 0, 0, 0, time.UTC)

	t.Run("open-with-broken-updated-at-still-blocks", func(t *testing.T) {
		issues := []trackerIssue{{ID: "trk-broken", Status: "open", Labels: &labels, UpdatedAt: "not-a-timestamp"}}
		if got := findBlockingIssue(issues, key, defaultDedupPolicy(), now); got == "" {
			t.Error("expected open issue to block even with broken updated_at")
		}
	})
	t.Run("open-with-empty-updated-at-still-blocks", func(t *testing.T) {
		issues := []trackerIssue{{ID: "trk-empty", Status: "open", Labels: &labels, UpdatedAt: ""}}
		if got := findBlockingIssue(issues, key, defaultDedupPolicy(), now); got == "" {
			t.Error("expected open issue to block even with empty updated_at")
		}
	})
	t.Run("in-progress-with-broken-updated-at-still-blocks", func(t *testing.T) {
		issues := []trackerIssue{{ID: "trk-inprog", Status: "in-progress", Labels: &labels, UpdatedAt: "garbage"}}
		if got := findBlockingIssue(issues, key, defaultDedupPolicy(), now); got == "" {
			t.Error("expected in-progress to block even with broken updated_at")
		}
	})
}

func TestFindBlockingIssue_MultipleOpenIssuesBlock(t *testing.T) {
	// Multiple open issues with the same dedup label: the first match blocks.
	now := time.Date(2026, 4, 5, 12, 0, 0, 0, time.UTC)
	createdAt := now.Add(-5 * 24 * time.Hour)

	key := "dkey000000000009"
	labels := "auto,nen,gyo,dedup:" + key
	updatedAt := createdAt.Format(time.RFC3339)

	issues := []trackerIssue{
		{ID: "trk-0E8F", Status: "open", Labels: &labels, UpdatedAt: updatedAt},
		{ID: "trk-6C01", Status: "open", Labels: &labels, UpdatedAt: updatedAt},
		{ID: "trk-F121", Status: "open", Labels: &labels, UpdatedAt: updatedAt},
	}

	got := findBlockingIssue(issues, key, defaultDedupPolicy(), now)
	if got == "" {
		t.Error("expected open issues to block regardless of age, got empty string")
	}
	if got != "trk-0E8F" {
		t.Errorf("expected first matching open issue trk-0E8F, got %q", got)
	}
}

func TestFindBlockingIssue_CustomPolicy(t *testing.T) {
	// Verify the cancelled TTL policy is actually consulted (not hardcoded).
	now := time.Date(2026, 4, 5, 12, 0, 0, 0, time.UTC)
	key := "dkey00000000000a"
	strict := dedupBlockingPolicy{cancelledTTL: time.Hour}

	// Open issues always block regardless of policy TTL.
	openIssues := []trackerIssue{makeTrackerIssueWithLabel("trk-open-2h", "open", key, 2*time.Hour, now)}
	if got := findBlockingIssue(openIssues, key, strict, now); got == "" {
		t.Error("expected open issue to always block, got empty string")
	}

	// Cancelled issue 2h old: strict 1h policy should NOT block.
	cancelledIssues := []trackerIssue{makeTrackerIssueWithLabel("trk-cancel-2h", "cancelled", key, 2*time.Hour, now)}
	if got := findBlockingIssue(cancelledIssues, key, strict, now); got != "" {
		t.Errorf("expected strict 1h cancelledTTL to expire 2h-old cancelled issue, got %q", got)
	}

	// But the default 7d policy should still block the cancelled issue.
	if got := findBlockingIssue(cancelledIssues, key, defaultDedupPolicy(), now); got == "" {
		t.Error("expected default 7d cancelledTTL to still block 2h-old cancelled issue")
	}
}

func TestFindBlockingIssue_PrefersFirstBlockingMatch(t *testing.T) {
	// When multiple matching issues exist, return the first blocking one.
	// Non-blocking matches (done/closed) should be skipped so the scan does
	// not stop at a non-blocking match and miss a real block.
	now := time.Date(2026, 4, 5, 12, 0, 0, 0, time.UTC)
	key := "dkey00000000000b"

	issues := []trackerIssue{
		makeTrackerIssueWithLabel("trk-done", "done", key, time.Hour, now),
		makeTrackerIssueWithLabel("trk-inprog", "in-progress", key, time.Hour, now),
	}

	got := findBlockingIssue(issues, key, defaultDedupPolicy(), now)
	if got != "trk-inprog" {
		t.Errorf("expected to skip done and return in-progress, got %q", got)
	}
}

// --- computeDedupKey length ---

func TestComputeDedupKey_Is16HexChars(t *testing.T) {
	f := proposableFinding{Ability: "shu", Category: "perf", ScopeKind: "mission", ScopeValue: "m-abc"}
	key := computeDedupKey(f)
	if len(key) != 16 {
		t.Errorf("expected dedup key length 16 (8 bytes hex), got %d: %q", len(key), key)
	}
}

// --- proposals DB (wasRecentlyProposed / recordProposed) ---

func openTestProposalsDB(t *testing.T) *sql.DB {
	t.Helper()
	_, dbPath := setupAllukaHome(t)
	// proposalsDB goes next to findings.db in the same nen/ dir
	proposalsDir := filepath.Dir(dbPath)
	if err := os.MkdirAll(proposalsDir, 0o755); err != nil {
		t.Fatalf("creating proposals db dir: %v", err)
	}
	proposalsPath := filepath.Join(proposalsDir, "proposals.db")
	db, err := sql.Open("sqlite", proposalsPath)
	if err != nil {
		t.Fatalf("open proposals db: %v", err)
	}
	if _, err := db.Exec(`CREATE TABLE IF NOT EXISTS proposals (
		dedup_key        TEXT PRIMARY KEY,
		last_proposed_at DATETIME NOT NULL,
		ability          TEXT NOT NULL DEFAULT '',
		category         TEXT NOT NULL DEFAULT '',
		tracker_issue    TEXT NOT NULL DEFAULT ''
	)`); err != nil {
		t.Fatalf("create proposals table: %v", err)
	}
	t.Cleanup(func() { db.Close() })
	return db
}

func TestWasRecentlyProposed_FalseWhenNoRecord(t *testing.T) {
	db := openTestProposalsDB(t)
	recent, err := wasRecentlyProposed(db, "deadbeef01020304")
	if err != nil {
		t.Fatalf("wasRecentlyProposed: %v", err)
	}
	if recent {
		t.Error("expected false for unknown dedup key, got true")
	}
}

func TestWasRecentlyProposed_TrueAfterRecord(t *testing.T) {
	db := openTestProposalsDB(t)
	key := "aabbccdd11223344"
	if err := recordProposed(db, key, "", "", ""); err != nil {
		t.Fatalf("recordProposed: %v", err)
	}
	recent, err := wasRecentlyProposed(db, key)
	if err != nil {
		t.Fatalf("wasRecentlyProposed: %v", err)
	}
	if !recent {
		t.Error("expected true immediately after recording, got false")
	}
}

func TestWasRecentlyProposed_FalseAfterExpiry(t *testing.T) {
	db := openTestProposalsDB(t)
	key := "oldkey1122334455"
	// Insert a record older than 7 days
	old := time.Now().UTC().Add(-8 * 24 * time.Hour).Format(time.RFC3339)
	if _, err := db.Exec(`INSERT INTO proposals (dedup_key, last_proposed_at) VALUES (?, ?)`, key, old); err != nil {
		t.Fatalf("insert old record: %v", err)
	}
	recent, err := wasRecentlyProposed(db, key)
	if err != nil {
		t.Fatalf("wasRecentlyProposed: %v", err)
	}
	if recent {
		t.Error("expected false for record older than 7 days, got true")
	}
}

func TestRecordProposed_UpdatesExistingRecord(t *testing.T) {
	db := openTestProposalsDB(t)
	key := "updatekey1234567"
	// Insert an old record
	old := time.Now().UTC().Add(-25 * time.Hour).Format(time.RFC3339)
	if _, err := db.Exec(`INSERT INTO proposals (dedup_key, last_proposed_at) VALUES (?, ?)`, key, old); err != nil {
		t.Fatalf("insert old record: %v", err)
	}
	// Re-record — should update to now
	if err := recordProposed(db, key, "", "", ""); err != nil {
		t.Fatalf("recordProposed: %v", err)
	}
	recent, err := wasRecentlyProposed(db, key)
	if err != nil {
		t.Fatalf("wasRecentlyProposed: %v", err)
	}
	if !recent {
		t.Error("expected true after re-recording stale key, got false")
	}
}

// --- buildRegularItems ---

func TestBuildRegularItems_BatchesGroupsOf3Plus(t *testing.T) {
	findings := []proposableFinding{
		{ID: "f1", Ability: "shu", Category: "perf", Severity: "medium"},
		{ID: "f2", Ability: "shu", Category: "perf", Severity: "medium"},
		{ID: "f3", Ability: "shu", Category: "perf", Severity: "medium"},
	}
	items := buildRegularItems(findings)
	if len(items) != 1 {
		t.Fatalf("expected 1 batch item, got %d", len(items))
	}
	if !items[0].isBatch {
		t.Error("expected batch item")
	}
	if len(items[0].findings) != 3 {
		t.Errorf("expected 3 findings in batch, got %d", len(items[0].findings))
	}
}

func TestBuildRegularItems_KeepsSmallGroupsIndividual(t *testing.T) {
	findings := []proposableFinding{
		{ID: "f1", Ability: "shu", Category: "perf", Severity: "medium"},
		{ID: "f2", Ability: "shu", Category: "perf", Severity: "medium"},
	}
	items := buildRegularItems(findings)
	if len(items) != 2 {
		t.Fatalf("expected 2 individual items, got %d", len(items))
	}
	for _, item := range items {
		if item.isBatch {
			t.Error("expected individual items, got batch")
		}
	}
}

func TestBuildRegularItems_SeverityOrderCriticalFirst(t *testing.T) {
	findings := []proposableFinding{
		{ID: "f1", Ability: "shu", Category: "perf", Severity: "medium"},
		{ID: "f2", Ability: "ko", Category: "error", Severity: "critical"},
	}
	items := buildRegularItems(findings)
	if len(items) != 2 {
		t.Fatalf("expected 2 items, got %d", len(items))
	}
	if items[0].severity != "critical" {
		t.Errorf("expected critical first, got %q", items[0].severity)
	}
}

func TestBuildRegularItems_Exactly3IsBatched(t *testing.T) {
	// Boundary: exactly 3 triggers batch.
	findings := []proposableFinding{
		{ID: "a", Ability: "ko", Category: "err", Severity: "high"},
		{ID: "b", Ability: "ko", Category: "err", Severity: "high"},
		{ID: "c", Ability: "ko", Category: "err", Severity: "high"},
	}
	items := buildRegularItems(findings)
	if len(items) != 1 || !items[0].isBatch {
		t.Errorf("exactly 3 findings should produce 1 batch item, got %d items", len(items))
	}
}

func TestBuildRegularItems_MixedGroupsAndIndividuals(t *testing.T) {
	// ability=a, category=x: 3 findings → batch
	// ability=b, category=y: 1 finding → individual
	findings := []proposableFinding{
		{ID: "a1", Ability: "a", Category: "x", Severity: "high"},
		{ID: "a2", Ability: "a", Category: "x", Severity: "high"},
		{ID: "a3", Ability: "a", Category: "x", Severity: "high"},
		{ID: "b1", Ability: "b", Category: "y", Severity: "medium"},
	}
	items := buildRegularItems(findings)
	if len(items) != 2 {
		t.Fatalf("expected 2 items, got %d", len(items))
	}
	batches := 0
	individuals := 0
	for _, item := range items {
		if item.isBatch {
			batches++
		} else {
			individuals++
		}
	}
	if batches != 1 || individuals != 1 {
		t.Errorf("expected 1 batch + 1 individual, got %d batch + %d individual", batches, individuals)
	}
}

// --- computeBatchDedupKey ---

func TestComputeBatchDedupKey_DifferentAbilityProducesDifferentKey(t *testing.T) {
	k1 := computeBatchDedupKey("shu", "perf")
	k2 := computeBatchDedupKey("ko", "perf")
	if k1 == k2 {
		t.Error("different ability should produce different batch dedup keys")
	}
}

func TestComputeBatchDedupKey_DifferentCategoryProducesDifferentKey(t *testing.T) {
	k1 := computeBatchDedupKey("shu", "perf")
	k2 := computeBatchDedupKey("shu", "safety")
	if k1 == k2 {
		t.Error("different category should produce different batch dedup keys")
	}
}

func TestComputeBatchDedupKey_Is16HexChars(t *testing.T) {
	key := computeBatchDedupKey("shu", "perf")
	if len(key) != 16 {
		t.Errorf("expected 16 hex chars, got %d: %q", len(key), key)
	}
}

// --- generateBatchMission ---

func TestGenerateBatchMission_ContainsTitle(t *testing.T) {
	findings := []proposableFinding{
		{ID: "f1", Title: "nil pointer", Severity: "high", Description: "desc1",
			Evidence: []evidenceItem{{Raw: "ev1", Source: "foo.go"}}},
		{ID: "f2", Title: "index out of range", Severity: "high", Description: "desc2",
			Evidence: []evidenceItem{{Raw: "ev2", Source: "bar.go"}}},
		{ID: "f3", Title: "divide by zero", Severity: "medium", Description: "desc3",
			Evidence: []evidenceItem{{Raw: "ev3", Source: "baz.go"}}},
	}
	content, err := generateBatchMission("shu", "safety", findings, "TRK-100")
	if err != nil {
		t.Fatalf("generateBatchMission: %v", err)
	}
	if !strings.Contains(content, "Fix: 3 safety findings in shu") {
		t.Errorf("expected batch title in content:\n%s", content)
	}
	if !strings.Contains(content, "tracker_issue: TRK-100") {
		t.Error("missing tracker_issue in frontmatter")
	}
	for _, id := range []string{"f1", "f2", "f3"} {
		if !strings.Contains(content, id) {
			t.Errorf("expected finding ID %s in mission content", id)
		}
	}
}

func TestGenerateBatchMission_Has3PhaseLines(t *testing.T) {
	findings := []proposableFinding{
		{ID: "f1", Title: "t1", Severity: "high", Evidence: []evidenceItem{{Raw: "e1", Source: "a.go"}}},
		{ID: "f2", Title: "t2", Severity: "high", Evidence: []evidenceItem{{Raw: "e2", Source: "b.go"}}},
		{ID: "f3", Title: "t3", Severity: "high", Evidence: []evidenceItem{{Raw: "e3", Source: "c.go"}}},
	}
	content, err := generateBatchMission("ko", "error", findings, "TRK-200")
	if err != nil {
		t.Fatalf("generateBatchMission: %v", err)
	}
	phaseCount := strings.Count(content, "\nPHASE:")
	if phaseCount != 3 {
		t.Errorf("expected 3 PHASE lines, got %d:\n%s", phaseCount, content)
	}
}

// --- rate limit via dry-run ---

func TestRateLimit_StopsAt5DryRun(t *testing.T) {
	_, dbPath := setupAllukaHome(t)
	tmpHome := t.TempDir()
	t.Setenv("HOME", tmpHome)

	now := time.Now().UTC()
	// Seed 10 individual findings (each in its own ability+category to avoid batching)
	var rows []map[string]string
	for i := 0; i < 10; i++ {
		rows = append(rows, map[string]string{
			"id":          fmt.Sprintf("rl-%03d", i),
			"ability":     fmt.Sprintf("ability%d", i),
			"category":    fmt.Sprintf("cat%d", i),
			"severity":    "medium",
			"title":       fmt.Sprintf("Finding %d", i),
			"description": "desc",
			"scope_kind":  "mission",
			"scope_value": fmt.Sprintf("mission-%d", i),
			"evidence":    evidenceWithSource("ev", fmt.Sprintf("file%d.go", i)),
			"source":      "test",
			"found_at":    now.Add(-25 * time.Hour).Format(time.RFC3339), // old enough to pass 24h threshold
		})
	}
	seedFindingsDB(t, dbPath, rows)

	// Run propose in dry-run mode (no tracker calls, no disk writes beyond proposals.db)
	err := runPropose([]string{"--dry-run"})
	if err != nil {
		t.Fatalf("runPropose: %v", err)
	}
	// We can't directly inspect out here, but the function must not return error.
	// To validate the rate limit, we test buildRegularItems + the limit constant.
}

func TestRateLimitMax_Is5(t *testing.T) {
	if rateLimitMax != 5 {
		t.Errorf("rateLimitMax should be 5, got %d", rateLimitMax)
	}
}

// --- queryProposableFindings threshold logic ---

func setupAllukaHome(t *testing.T) (string, string) {
	t.Helper()
	tmpDir := t.TempDir()
	t.Setenv("ALLUKA_HOME", tmpDir)
	dbPath := filepath.Join(tmpDir, "nen", "findings.db")
	return tmpDir, dbPath
}

func TestQueryProposable_ReviewBlockerBypassesThreshold(t *testing.T) {
	// A high-severity review-blocker finding found 1 hour ago (normally < 24h) should be proposable.
	_, dbPath := setupAllukaHome(t)

	recentlyFound := time.Now().UTC().Add(-1 * time.Hour).Format(time.RFC3339)
	evidence := evidenceWithSource("nil dereference at review", "plugins/nen/cmd/shu/propose.go")

	seedFindingsDB(t, dbPath, []map[string]string{
		{
			"id": "rb-001", "ability": "shu", "category": "review-blocker",
			"severity": "high", "title": "Review blocker: nil pointer",
			"description": "nil pointer in review phase", "scope_kind": "workspace",
			"scope_value": "ws-rb-test", "evidence": evidence,
			"source": "ko-eval", "found_at": recentlyFound,
		},
	})

	ctx := context.Background()
	findings, err := queryProposableFindings(ctx)
	if err != nil {
		t.Fatalf("queryProposableFindings: %v", err)
	}

	if len(findings) != 1 {
		t.Fatalf("expected 1 proposable finding (review-blocker bypasses 24h), got %d", len(findings))
	}
	if findings[0].Category != "review-blocker" {
		t.Errorf("expected review-blocker category, got %q", findings[0].Category)
	}
}

func TestQueryProposable_HighNormalRequires24hOrMultiple(t *testing.T) {
	// A recent high-severity non-review-blocker (< 24h, count=1) should NOT be proposable.
	_, dbPath := setupAllukaHome(t)

	recentlyFound := time.Now().UTC().Add(-30 * time.Minute).Format(time.RFC3339)
	evidence := evidenceWithSource("performance issue", "plugins/nen/foo.go")

	seedFindingsDB(t, dbPath, []map[string]string{
		{
			"id": "h-001", "ability": "shu", "category": "performance",
			"severity": "high", "title": "Slow query",
			"description": "slow query in hot path", "scope_kind": "mission",
			"scope_value": "mission-123", "evidence": evidence,
			"source": "ko-eval", "found_at": recentlyFound,
		},
	})

	ctx := context.Background()
	findings, err := queryProposableFindings(ctx)
	if err != nil {
		t.Fatalf("queryProposableFindings: %v", err)
	}

	for _, f := range findings {
		if f.ID == "h-001" {
			t.Error("recent high finding with count=1 should NOT be proposable under 24h")
		}
	}
}

func TestQueryProposable_HighNormalProposableAfter24h(t *testing.T) {
	// A high-severity non-review-blocker older than 24h SHOULD be proposable.
	_, dbPath := setupAllukaHome(t)

	oldFound := time.Now().UTC().Add(-25 * time.Hour).Format(time.RFC3339)
	evidence := evidenceWithSource("stale perf issue", "plugins/nen/bar.go")

	seedFindingsDB(t, dbPath, []map[string]string{
		{
			"id": "h-002", "ability": "shu", "category": "performance-old",
			"severity": "high", "title": "Old slow query",
			"description": "old slow query", "scope_kind": "mission",
			"scope_value": "mission-456", "evidence": evidence,
			"source": "ko-eval", "found_at": oldFound,
		},
	})

	ctx := context.Background()
	findings, err := queryProposableFindings(ctx)
	if err != nil {
		t.Fatalf("queryProposableFindings: %v", err)
	}

	found := false
	for _, f := range findings {
		if f.ID == "h-002" {
			found = true
		}
	}
	if !found {
		t.Error("high finding older than 24h should be proposable")
	}
}

func TestQueryProposable_HighNormalProposableWhenMultiple(t *testing.T) {
	// Two high-severity findings in the same ability:category (< 24h) should both be proposable (count >= 2).
	_, dbPath := setupAllukaHome(t)

	recentlyFound := time.Now().UTC().Add(-30 * time.Minute).Format(time.RFC3339)
	evidence := evidenceWithSource("repeated issue", "plugins/nen/baz.go")

	seedFindingsDB(t, dbPath, []map[string]string{
		{
			"id": "h-003", "ability": "shu", "category": "repeated-cat",
			"severity": "high", "title": "Issue A",
			"description": "issue A", "scope_kind": "mission",
			"scope_value": "mission-a", "evidence": evidence,
			"source": "ko-eval", "found_at": recentlyFound,
		},
		{
			"id": "h-004", "ability": "shu", "category": "repeated-cat",
			"severity": "high", "title": "Issue B",
			"description": "issue B", "scope_kind": "phase",
			"scope_value": "phase-b", "evidence": evidence,
			"source": "ko-eval", "found_at": recentlyFound,
		},
	})

	ctx := context.Background()
	findings, err := queryProposableFindings(ctx)
	if err != nil {
		t.Fatalf("queryProposableFindings: %v", err)
	}

	ids := make(map[string]bool)
	for _, f := range findings {
		ids[f.ID] = true
	}
	if !ids["h-003"] || !ids["h-004"] {
		t.Errorf("both high findings with count>=2 should be proposable; got IDs: %v", ids)
	}
}

func TestQueryProposable_ExcludesSupersededFindings(t *testing.T) {
	_, dbPath := setupAllukaHome(t)

	recentlyFound := time.Now().UTC().Add(-1 * time.Hour).Format(time.RFC3339)
	evidence := evidenceWithSource("superseded issue", "plugins/nen/qux.go")

	seedFindingsDB(t, dbPath, []map[string]string{
		{
			"id": "sup-001", "ability": "shu", "category": "review-blocker",
			"severity": "high", "title": "Superseded finding",
			"description": "should not appear", "scope_kind": "workspace",
			"scope_value": "ws-superseded", "evidence": evidence,
			"source": "ko-eval", "found_at": recentlyFound,
			"superseded_by": "sup-002", // non-empty: excluded
		},
	})

	ctx := context.Background()
	findings, err := queryProposableFindings(ctx)
	if err != nil {
		t.Fatalf("queryProposableFindings: %v", err)
	}
	for _, f := range findings {
		if f.ID == "sup-001" {
			t.Error("superseded finding should not be returned as proposable")
		}
	}
}

// --- proposeReviewBlockerGroup dry-run ---

func TestProposeReviewBlockerGroup_DryRun_NormalIteration_GeneratesMission(t *testing.T) {
	// iteration < 3 → writes fix mission, not escalation
	tmpHome := t.TempDir()
	t.Setenv("HOME", tmpHome)

	workspaceID := "20260101-abcdef12"
	wsDir := filepath.Join(tmpHome, ".alluka", "workspaces", workspaceID)
	if err := os.MkdirAll(wsDir, 0o755); err != nil {
		t.Fatalf("mkdir workspace: %v", err)
	}
	// iteration: 1 — below escalation threshold
	missionContent := "---\nsource: orchestrator\niteration: 1\n---\n# Mission\n"
	if err := os.WriteFile(filepath.Join(wsDir, "mission.md"), []byte(missionContent), 0o644); err != nil {
		t.Fatalf("write mission.md: %v", err)
	}

	findings := []proposableFinding{
		{
			ID:       "rb-dry-001",
			Ability:  "shu",
			Category: "review-blocker",
			Severity: "high",
			Title:    "nil dereference in review",
			ScopeKind:  "workspace",
			ScopeValue: workspaceID,
			Evidence: []evidenceItem{{Raw: "nil pointer at line 42", Source: "cmd/shu/propose.go"}},
		},
	}

	p, skipped, err := proposeReviewBlockerGroup(workspaceID, findings, nil, true /*dryRun*/, false)
	if err != nil {
		t.Fatalf("proposeReviewBlockerGroup: %v", err)
	}
	if len(skipped) > 0 {
		t.Errorf("expected no skipped findings, got %v", skipped)
	}
	if p == nil {
		t.Fatal("expected a proposal, got nil")
	}
	if p.TrackerIssue != "(dry-run)" {
		t.Errorf("expected (dry-run) tracker issue, got %q", p.TrackerIssue)
	}
	if p.MissionFile == "" {
		t.Error("expected non-empty mission file path")
	}
	// Mission file path should contain the workspace ID
	if !strings.Contains(p.MissionFile, workspaceID) {
		t.Errorf("mission file path %q does not contain workspace ID %q", p.MissionFile, workspaceID)
	}
}

func TestProposeReviewBlockerGroup_DryRun_Escalation_WhenIterationGe3(t *testing.T) {
	// iteration >= 3 → escalation issue, no mission file
	tmpHome := t.TempDir()
	t.Setenv("HOME", tmpHome)

	workspaceID := "20260101-escalate1"
	wsDir := filepath.Join(tmpHome, ".alluka", "workspaces", workspaceID)
	if err := os.MkdirAll(wsDir, 0o755); err != nil {
		t.Fatalf("mkdir workspace: %v", err)
	}
	missionContent := "---\nsource: orchestrator\niteration: 3\n---\n# Mission\n"
	if err := os.WriteFile(filepath.Join(wsDir, "mission.md"), []byte(missionContent), 0o644); err != nil {
		t.Fatalf("write mission.md: %v", err)
	}

	findings := []proposableFinding{
		{
			ID:         "rb-esc-001",
			Ability:    "shu",
			Category:   "review-blocker",
			Severity:   "high",
			Title:      "persistent nil dereference",
			ScopeKind:  "workspace",
			ScopeValue: workspaceID,
			Evidence:   []evidenceItem{{Raw: "nil pointer iteration 3", Source: "cmd/shu/review.go"}},
		},
	}

	p, skipped, err := proposeReviewBlockerGroup(workspaceID, findings, nil, true /*dryRun*/, false)
	if err != nil {
		t.Fatalf("proposeReviewBlockerGroup: %v", err)
	}
	if len(skipped) > 0 {
		t.Errorf("expected no skipped findings, got %v", skipped)
	}
	if p == nil {
		t.Fatal("expected a proposal, got nil")
	}
	if p.TrackerIssue != "(dry-run escalation)" {
		t.Errorf("expected escalation proposal, got tracker issue %q", p.TrackerIssue)
	}
	if p.MissionFile != "" {
		t.Errorf("escalation should have no mission file, got %q", p.MissionFile)
	}
	if p.Severity != "critical" {
		t.Errorf("escalation severity should be 'critical', got %q", p.Severity)
	}
}

func TestProposeReviewBlockerGroup_DryRun_Escalation_ExactlyAt3(t *testing.T) {
	// Boundary: iteration == 3 triggers escalation.
	tmpHome := t.TempDir()
	t.Setenv("HOME", tmpHome)

	workspaceID := "20260101-boundary3"
	wsDir := filepath.Join(tmpHome, ".alluka", "workspaces", workspaceID)
	if err := os.MkdirAll(wsDir, 0o755); err != nil {
		t.Fatalf("mkdir workspace: %v", err)
	}
	if err := os.WriteFile(filepath.Join(wsDir, "mission.md"), []byte("---\niteration: 3\n---\n"), 0o644); err != nil {
		t.Fatalf("write mission.md: %v", err)
	}

	findings := []proposableFinding{
		{ID: "rb-b3-001", ScopeValue: workspaceID,
			Evidence: []evidenceItem{{Raw: "issue", Source: "foo.go"}}},
	}
	p, _, err := proposeReviewBlockerGroup(workspaceID, findings, nil, true, false)
	if err != nil {
		t.Fatalf("proposeReviewBlockerGroup: %v", err)
	}
	if p == nil || p.TrackerIssue != "(dry-run escalation)" {
		t.Errorf("iteration==3 should escalate; got proposal %+v", p)
	}
}

func TestProposeReviewBlockerGroup_DryRun_NoEscalation_At2(t *testing.T) {
	// Boundary: iteration == 2 should NOT escalate.
	tmpHome := t.TempDir()
	t.Setenv("HOME", tmpHome)

	workspaceID := "20260101-boundary2"
	wsDir := filepath.Join(tmpHome, ".alluka", "workspaces", workspaceID)
	if err := os.MkdirAll(wsDir, 0o755); err != nil {
		t.Fatalf("mkdir workspace: %v", err)
	}
	if err := os.WriteFile(filepath.Join(wsDir, "mission.md"), []byte("---\niteration: 2\n---\n"), 0o644); err != nil {
		t.Fatalf("write mission.md: %v", err)
	}

	findings := []proposableFinding{
		{ID: "rb-b2-001", ScopeValue: workspaceID,
			Evidence: []evidenceItem{{Raw: "issue", Source: "bar.go"}}},
	}
	p, _, err := proposeReviewBlockerGroup(workspaceID, findings, nil, true, false)
	if err != nil {
		t.Fatalf("proposeReviewBlockerGroup: %v", err)
	}
	if p == nil || p.TrackerIssue != "(dry-run)" {
		t.Errorf("iteration==2 should NOT escalate; got proposal %+v", p)
	}
}

func TestProposeReviewBlockerGroup_DedupSkipsWhenIssueExists(t *testing.T) {
	tmpHome := t.TempDir()
	t.Setenv("HOME", tmpHome)

	workspaceID := "20260101-deduptest"

	findings := []proposableFinding{
		{
			ID:         "rb-dup-001",
			Ability:    "shu",
			Category:   "review-blocker",
			ScopeKind:  "workspace",
			ScopeValue: workspaceID,
			Evidence:   []evidenceItem{{Raw: "dup issue", Source: "cmd/shu/foo.go"}},
		},
	}

	// Create a pre-existing tracker issue with the dedup label. Under the
	// findBlockingIssue policy, an `open` issue only blocks while its
	// updated_at is within the stale-open TTL (24h), so set a recent
	// timestamp to guarantee the block.
	key := computeDedupKey(findings[0])
	labelStr := fmt.Sprintf("auto,nen,dedup:%s", key)
	seqID := int64(7)
	recentUpdatedAt := time.Now().UTC().Add(-1 * time.Hour).Format(time.RFC3339)
	existing := []trackerIssue{
		{ID: "trk-existing", SeqID: &seqID, Title: "existing review-blocker fix", Status: "open", Labels: &labelStr, UpdatedAt: recentUpdatedAt},
	}

	p, skipped, err := proposeReviewBlockerGroup(workspaceID, findings, existing, true, false)
	if err != nil {
		t.Fatalf("proposeReviewBlockerGroup: %v", err)
	}
	if p != nil {
		t.Errorf("expected no proposal when dedup issue exists, got %+v", p)
	}
	if len(skipped) != 1 {
		t.Errorf("expected 1 skipped finding, got %d", len(skipped))
	}
	if skipped[0].FindingID != "rb-dup-001" {
		t.Errorf("expected skipped finding rb-dup-001, got %q", skipped[0].FindingID)
	}
}

// Regression guard: a stale open review-blocker tracker issue (older than
// Open issues always block — even stale ones. The issue is unresolved,
// so re-proposing would create duplicates.
func TestProposeReviewBlockerGroup_OpenIssueBlocks(t *testing.T) {
	tmpHome := t.TempDir()
	t.Setenv("HOME", tmpHome)

	workspaceID := "20260101-staletest"

	findings := []proposableFinding{
		{
			ID:         "rb-stale-001",
			Ability:    "shu",
			Category:   "review-blocker",
			ScopeKind:  "workspace",
			ScopeValue: workspaceID,
			Evidence:   []evidenceItem{{Raw: "still broken", Source: "cmd/shu/bar.go"}},
		},
	}

	key := computeDedupKey(findings[0])
	labelStr := fmt.Sprintf("auto,nen,dedup:%s", key)
	seqID := int64(9)
	staleUpdatedAt := time.Now().UTC().Add(-48 * time.Hour).Format(time.RFC3339)
	existing := []trackerIssue{
		{ID: "trk-stale", SeqID: &seqID, Title: "stuck review-blocker issue", Status: "open", Labels: &labelStr, UpdatedAt: staleUpdatedAt},
	}

	p, skipped, err := proposeReviewBlockerGroup(workspaceID, findings, existing, true, false)
	if err != nil {
		t.Fatalf("proposeReviewBlockerGroup: %v", err)
	}
	if p != nil {
		t.Errorf("expected no proposal when open issue exists, got %+v", p)
	}
	foundBlocking := false
	for _, s := range skipped {
		if strings.Contains(s.Reason, "blocking tracker issue") {
			foundBlocking = true
		}
	}
	if !foundBlocking {
		t.Error("expected open issue to be reported as blocking in skipped reasons")
	}
}

// --- remediationMissionDir / missionFilePath ALLUKA_HOME tests ---

func TestMissionFilePath_RespectsAllukaHome(t *testing.T) {
	custom := t.TempDir()
	t.Setenv("ALLUKA_HOME", custom)

	f := proposableFinding{Title: "test finding"}
	got := missionFilePath(f)

	if !strings.HasPrefix(got, custom) {
		t.Errorf("missionFilePath() = %q, want path under ALLUKA_HOME %q", got, custom)
	}
	home, _ := os.UserHomeDir()
	if strings.HasPrefix(got, home) && !strings.HasPrefix(custom, home) {
		t.Errorf("missionFilePath() = %q uses $HOME instead of ALLUKA_HOME %q", got, custom)
	}
}

func TestBatchMissionFilePath_RespectsAllukaHome(t *testing.T) {
	custom := t.TempDir()
	t.Setenv("ALLUKA_HOME", custom)

	got := batchMissionFilePath("gyo", "unchecked-error")

	if !strings.HasPrefix(got, custom) {
		t.Errorf("batchMissionFilePath() = %q, want path under ALLUKA_HOME %q", got, custom)
	}
}

func TestRemediationMissionDir_RespectsAllukaHome(t *testing.T) {
	custom := t.TempDir()
	t.Setenv("ALLUKA_HOME", custom)

	got := remediationMissionDir()

	if !strings.HasPrefix(got, custom) {
		t.Errorf("remediationMissionDir() = %q, want path under ALLUKA_HOME %q", got, custom)
	}
}

// --- schedulerJobExists JSON parsing ---
//
// The scheduler CLI has shipped two shapes for `query items --json` over time:
// a wrapped envelope `{"items": [...], "count": N}` (current) and a bare JSON
// array `[...]` (older). schedulerJobExistsInOutput accepts both so version
// skew between an installed scheduler binary and an installed shu binary can't
// silently break `shu propose --init`. These tests guard both shapes.

func TestSchedulerJobExistsInOutput_EnvelopeMatchesName(t *testing.T) {
	out := []byte(`{
		"items": [
			{"id": 1, "name": "propose-remediations", "schedule": "0 */4 * * *"},
			{"id": 2, "name": "dispatch-approved", "schedule": "*/15 * * * *"}
		],
		"count": 2
	}`)
	got, err := schedulerJobExistsInOutput(out, "dispatch-approved")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !got {
		t.Error("expected dispatch-approved to be found in envelope shape, got false")
	}
}

func TestSchedulerJobExistsInOutput_EnvelopeMissingName(t *testing.T) {
	out := []byte(`{"items": [{"id": 1, "name": "propose-remediations"}], "count": 1}`)
	got, err := schedulerJobExistsInOutput(out, "close-sweep")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if got {
		t.Error("expected close-sweep to be absent, got true")
	}
}

func TestSchedulerJobExistsInOutput_BareArrayMatchesName(t *testing.T) {
	// Older scheduler versions returned a bare array. Defensive fallback must
	// still recognize that shape so version-skew across upgrades doesn't break
	// nen init.
	out := []byte(`[
		{"id": 1, "name": "propose-remediations"},
		{"id": 2, "name": "dispatch-approved"}
	]`)
	got, err := schedulerJobExistsInOutput(out, "dispatch-approved")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !got {
		t.Error("expected dispatch-approved to be found in bare-array shape, got false")
	}
}

func TestSchedulerJobExistsInOutput_BareArrayMissingName(t *testing.T) {
	out := []byte(`[{"id": 1, "name": "propose-remediations"}]`)
	got, err := schedulerJobExistsInOutput(out, "close-sweep")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if got {
		t.Error("expected close-sweep to be absent, got true")
	}
}

func TestSchedulerJobExistsInOutput_EmptyEnvelope(t *testing.T) {
	got, err := schedulerJobExistsInOutput([]byte(`{"items": [], "count": 0}`), "anything")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if got {
		t.Error("empty envelope should never match")
	}
}

func TestSchedulerJobExistsInOutput_EmptyBareArray(t *testing.T) {
	got, err := schedulerJobExistsInOutput([]byte(`[]`), "anything")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if got {
		t.Error("empty bare array should never match")
	}
}

func TestSchedulerJobExistsInOutput_MalformedJSONReturnsError(t *testing.T) {
	_, err := schedulerJobExistsInOutput([]byte(`not json`), "x")
	if err == nil {
		t.Error("expected error for malformed JSON, got nil")
	}
}
