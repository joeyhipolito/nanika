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

// --- findExistingIssue (dedup) ---

func TestFindExistingIssue_FindsOpenIssueByLabel(t *testing.T) {
	f := proposableFinding{Ability: "shu", Category: "review-blocker", ScopeKind: "workspace", ScopeValue: "ws-deduptest"}
	key := computeDedupKey(f)
	label := "dedup:" + key

	seqID := int64(42)
	labelStr := "auto,nen," + label
	issues := []trackerIssue{
		{ID: "trk-abc", SeqID: &seqID, Title: "existing", Status: "open", Labels: &labelStr},
	}

	got := findExistingIssue(issues, key)
	if got == "" {
		t.Error("expected to find existing issue, got empty string")
	}
}

func TestFindExistingIssue_IgnoresClosedIssues(t *testing.T) {
	f := proposableFinding{Ability: "shu", Category: "review-blocker", ScopeKind: "workspace", ScopeValue: "ws-closedtest"}
	key := computeDedupKey(f)
	label := "dedup:" + key
	labelStr := "auto,nen," + label
	issues := []trackerIssue{
		{ID: "trk-old", Title: "closed issue", Status: "done", Labels: &labelStr},
	}

	got := findExistingIssue(issues, key)
	if got != "" {
		t.Errorf("expected no match for closed issue, got %q", got)
	}
}

func TestFindExistingIssue_NoMatchReturnsEmpty(t *testing.T) {
	issues := []trackerIssue{
		{ID: "trk-other", Title: "unrelated", Status: "open"},
	}
	got := findExistingIssue(issues, "deadbeef")
	if got != "" {
		t.Errorf("expected empty, got %q", got)
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

	// Create a pre-existing tracker issue with the dedup label
	key := computeDedupKey(findings[0])
	labelStr := fmt.Sprintf("auto,nen,dedup:%s", key)
	seqID := int64(7)
	existing := []trackerIssue{
		{ID: "trk-existing", SeqID: &seqID, Title: "existing review-blocker fix", Status: "open", Labels: &labelStr},
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
