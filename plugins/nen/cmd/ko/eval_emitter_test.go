package main

import (
	"context"
	"database/sql"
	"fmt"
	"path/filepath"
	"testing"
	"time"

	"github.com/joeyhipolito/nen/internal/scan"
	"github.com/joeyhipolito/nen/ko"
	_ "modernc.org/sqlite"
)

func TestThresholdSeverity_Boundaries(t *testing.T) {
	tests := []struct {
		passRate float64
		want     scan.Severity
	}{
		{0, scan.SeverityHigh},
		{59.9, scan.SeverityHigh},
		{60.0, scan.SeverityMedium},
		{79.9, scan.SeverityMedium},
		{80.0, ""},
		{100, ""},
	}
	for _, tt := range tests {
		got := thresholdSeverity(tt.passRate)
		if got != tt.want {
			t.Errorf("thresholdSeverity(%.1f) = %q, want %q", tt.passRate, got, tt.want)
		}
	}
}

func TestDeterministicKoFindingID_Stable(t *testing.T) {
	a := deterministicKoFindingID("/path/config.yaml", "suite-a")
	b := deterministicKoFindingID("/path/config.yaml", "suite-a")
	if a != b {
		t.Errorf("same inputs produced different IDs: %q vs %q", a, b)
	}
}

func TestDeterministicKoFindingID_DifferentSuites(t *testing.T) {
	a := deterministicKoFindingID("/path/config.yaml", "suite-a")
	b := deterministicKoFindingID("/path/config.yaml", "suite-b")
	if a == b {
		t.Errorf("different suites produced same ID: %q", a)
	}
}

// TestDeterministicKoFindingID_ConfigPathIsFirstArg verifies that swapping
// configPath and suite produces a different ID (TRK-398: args must not be
// transposed).
func TestDeterministicKoFindingID_ConfigPathIsFirstArg(t *testing.T) {
	id1 := deterministicKoFindingID("/cfg/review.yaml", "suite-x")
	id2 := deterministicKoFindingID("suite-x", "/cfg/review.yaml")
	if id1 == id2 {
		t.Errorf("transposed args produced the same ID %q — configPath must be the first argument", id1)
	}
}

func TestAggregateBySuite_SetsConfigPath(t *testing.T) {
	suites := aggregateBySuite("/abs/eval.yaml", nil)
	if len(suites) != 1 {
		t.Fatalf("got %d suites, want 1", len(suites))
	}
	if suites[0].ConfigPath != "/abs/eval.yaml" {
		t.Errorf("ConfigPath = %q, want %q", suites[0].ConfigPath, "/abs/eval.yaml")
	}
	if suites[0].Name != "/abs/eval.yaml" {
		t.Errorf("Name = %q, want %q", suites[0].Name, "/abs/eval.yaml")
	}
}

func TestAggregateBySuite(t *testing.T) {
	results := []ko.TestResult{
		{Description: "test 1", Passed: true},
		{Description: "test 2", Passed: false, Assertions: []ko.AssertionResult{
			{Type: "contains", Passed: false, Message: "missing keyword"},
		}},
		{Description: "test 3", Passed: true},
	}
	suites := aggregateBySuite("/cfg.yaml", results)
	if len(suites) != 1 {
		t.Fatalf("got %d suites, want 1", len(suites))
	}
	s := suites[0]
	if s.Total != 3 || s.Passed != 2 || s.Failed != 1 {
		t.Errorf("counts = {total:%d passed:%d failed:%d}, want {3 2 1}", s.Total, s.Passed, s.Failed)
	}
	if len(s.FailedTests) != 1 {
		t.Fatalf("got %d failed tests, want 1", len(s.FailedTests))
	}
	if s.FailedTests[0].Description != "test 2" {
		t.Errorf("failed test description = %q, want %q", s.FailedTests[0].Description, "test 2")
	}
}

func TestEmitFindings_NothingBelowThreshold(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ALLUKA_HOME", dir)

	suites := []SuiteResult{
		{Name: "good", Total: 10, Passed: 10, Failed: 0},
		{Name: "also-good", Total: 5, Passed: 4, Failed: 1}, // 80% → not emitted
	}
	emitted, err := emitFindings(context.Background(), suites, time.Now().UTC())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(emitted) != 0 {
		t.Errorf("expected 0 findings, got %d", len(emitted))
	}
}

func TestEmitFindings_SingleFailingSuite(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ALLUKA_HOME", dir)

	suites := []SuiteResult{
		{
			Name: "/eval/review.yaml", Total: 10, Passed: 5, Failed: 5,
			FailedTests: []failedTest{
				{Description: "test-1", Message: "wrong output", Source: "/eval/review.yaml#test-1"},
			},
		},
	}
	emitted, err := emitFindings(context.Background(), suites, time.Now().UTC())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(emitted) != 1 {
		t.Fatalf("expected 1 finding, got %d", len(emitted))
	}
	f := emitted[0]
	if f.Severity != scan.SeverityHigh {
		t.Errorf("severity = %q, want %q (50%% pass rate)", f.Severity, scan.SeverityHigh)
	}
	if f.Ability != "ko-eval" {
		t.Errorf("ability = %q, want %q", f.Ability, "ko-eval")
	}
	if f.Category != "eval-failure" {
		t.Errorf("category = %q, want %q", f.Category, "eval-failure")
	}
	if f.Scope.Kind != "eval-config" {
		t.Errorf("scope.kind = %q, want %q", f.Scope.Kind, "eval-config")
	}
	if len(f.Evidence) == 0 {
		t.Fatal("expected at least one evidence item")
	}
	if f.Evidence[0].Source == "" {
		t.Error("evidence[0].Source is empty — shu propose will skip this finding")
	}
}

func TestEmitFindings_CrashedSuite(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ALLUKA_HOME", dir)

	// Zero total simulates a crashed suite.
	suites := []SuiteResult{
		{Name: "crashed", Total: 0, Passed: 0, Failed: 0},
	}
	emitted, err := emitFindings(context.Background(), suites, time.Now().UTC())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(emitted) != 1 {
		t.Fatalf("expected 1 finding for crashed suite, got %d", len(emitted))
	}
	if emitted[0].Severity != scan.SeverityHigh {
		t.Errorf("severity = %q, want %q", emitted[0].Severity, scan.SeverityHigh)
	}
}

func TestEmitFindings_EvidenceCap(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ALLUKA_HOME", dir)

	var failed []failedTest
	for i := 0; i < 25; i++ {
		failed = append(failed, failedTest{
			Description: fmt.Sprintf("test-%d", i),
			Message:     "failed",
			Source:      fmt.Sprintf("/cfg.yaml#test-%d", i),
		})
	}
	suites := []SuiteResult{
		{Name: "/cfg.yaml", Total: 25, Passed: 0, Failed: 25, FailedTests: failed},
	}
	emitted, err := emitFindings(context.Background(), suites, time.Now().UTC())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(emitted) != 1 {
		t.Fatalf("expected 1 finding, got %d", len(emitted))
	}
	// 10 real items + 1 truncation marker = 11
	if len(emitted[0].Evidence) != maxEvidenceItems+1 {
		t.Errorf("evidence count = %d, want %d (cap + truncation marker)", len(emitted[0].Evidence), maxEvidenceItems+1)
	}
}

func TestEmitFindings_EvidenceSourceNonEmpty(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ALLUKA_HOME", dir)

	suites := []SuiteResult{
		{
			Name: "/cfg.yaml", Total: 4, Passed: 1, Failed: 3,
			FailedTests: []failedTest{
				{Description: "a", Message: "m", Source: "/cfg.yaml#a"},
				{Description: "b", Message: "m", Source: "/cfg.yaml#b"},
				{Description: "c", Message: "m", Source: "/cfg.yaml#c"},
			},
		},
	}
	emitted, err := emitFindings(context.Background(), suites, time.Now().UTC())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	for i, ev := range emitted[0].Evidence {
		if ev.Source == "" {
			t.Errorf("evidence[%d].Source is empty", i)
		}
	}
}

// ── Integration tests ─────────────────────────────────────────────────────────

func TestEmitFindings_WritesToFindingsDB(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ALLUKA_HOME", dir)

	suites := []SuiteResult{
		{
			Name: "/eval/persona.yaml", Total: 10, Passed: 3, Failed: 7,
			FailedTests: []failedTest{
				{Description: "test-1", Message: "wrong", Source: "/eval/persona.yaml#test-1"},
			},
		},
	}
	emitted, err := emitFindings(context.Background(), suites, time.Now().UTC())
	if err != nil {
		t.Fatalf("emitFindings: %v", err)
	}
	if len(emitted) != 1 {
		t.Fatalf("expected 1 emitted finding, got %d", len(emitted))
	}

	// Open findings.db and verify the row.
	dbPath := filepath.Join(dir, "nen", "findings.db")
	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("open findings.db: %v", err)
	}
	defer db.Close()

	var ability, category, severity, scopeKind string
	err = db.QueryRow(`SELECT ability, category, severity, scope_kind FROM findings WHERE id = ?`,
		emitted[0].ID).Scan(&ability, &category, &severity, &scopeKind)
	if err != nil {
		t.Fatalf("query finding: %v", err)
	}
	if ability != "ko-eval" {
		t.Errorf("ability = %q, want %q", ability, "ko-eval")
	}
	if category != "eval-failure" {
		t.Errorf("category = %q, want %q", category, "eval-failure")
	}
	if severity != "high" {
		t.Errorf("severity = %q, want %q", severity, "high")
	}
	if scopeKind != "eval-config" {
		t.Errorf("scope_kind = %q, want %q", scopeKind, "eval-config")
	}
}

func TestEmitFindings_Idempotent(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ALLUKA_HOME", dir)

	suites := []SuiteResult{
		{
			Name: "/eval/review.yaml", Total: 5, Passed: 2, Failed: 3,
			FailedTests: []failedTest{
				{Description: "t1", Message: "m1", Source: "/eval/review.yaml#t1"},
			},
		},
	}
	now := time.Now().UTC()

	// Call twice with same input.
	if _, err := emitFindings(context.Background(), suites, now); err != nil {
		t.Fatalf("first call: %v", err)
	}
	if _, err := emitFindings(context.Background(), suites, now); err != nil {
		t.Fatalf("second call: %v", err)
	}

	// Verify exactly one row (UpsertFinding refreshes existing, doesn't double-insert).
	dbPath := filepath.Join(dir, "nen", "findings.db")
	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("open findings.db: %v", err)
	}
	defer db.Close()

	var count int
	if err := db.QueryRow(`SELECT COUNT(*) FROM findings WHERE ability = 'ko-eval'`).Scan(&count); err != nil {
		t.Fatalf("count: %v", err)
	}
	if count != 1 {
		t.Errorf("expected 1 finding row after idempotent calls, got %d", count)
	}
}

// TestEmitFindings_SupersedeOnRecovery verifies that when a suite recovers above 80%,
// the prior finding's superseded_by is set (TRK-397).
func TestEmitFindings_SupersedeOnRecovery(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ALLUKA_HOME", dir)

	// First call: suite at 50% → emits a high finding.
	failing := []SuiteResult{
		{
			ConfigPath: "/eval/agent.yaml", Name: "/eval/agent.yaml",
			Total: 10, Passed: 5, Failed: 5,
			FailedTests: []failedTest{
				{Description: "t1", Message: "wrong output", Source: "/eval/agent.yaml#t1"},
			},
		},
	}
	emitted, err := emitFindings(context.Background(), failing, time.Now().UTC())
	if err != nil {
		t.Fatalf("first emitFindings: %v", err)
	}
	if len(emitted) != 1 {
		t.Fatalf("expected 1 finding from failing run, got %d", len(emitted))
	}
	findingID := emitted[0].ID

	// Second call: suite at 100% → should supersede the prior finding.
	passing := []SuiteResult{
		{ConfigPath: "/eval/agent.yaml", Name: "/eval/agent.yaml", Total: 10, Passed: 10},
	}
	emitted2, err := emitFindings(context.Background(), passing, time.Now().UTC())
	if err != nil {
		t.Fatalf("second emitFindings (recovery): %v", err)
	}
	if len(emitted2) != 0 {
		t.Errorf("expected 0 findings from recovery run, got %d", len(emitted2))
	}

	// Verify the prior finding now has superseded_by set.
	dbPath := filepath.Join(dir, "nen", "findings.db")
	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("open findings.db: %v", err)
	}
	defer db.Close()

	var supersededBy string
	err = db.QueryRow(`SELECT superseded_by FROM findings WHERE id = ?`, findingID).Scan(&supersededBy)
	if err != nil {
		t.Fatalf("query finding: %v", err)
	}
	if supersededBy == "" {
		t.Errorf("superseded_by is empty — prior finding was not superseded on recovery")
	}
}

// TestEmitFindings_SupersedeNoOpWhenNoFindings verifies that supersede-on-recovery
// is a no-op when findings.db doesn't yet exist (TRK-397 edge case).
func TestEmitFindings_SupersedeNoOpWhenNoFindings(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ALLUKA_HOME", dir)

	// Passing suite but no prior findings.db at all — should not error.
	passing := []SuiteResult{
		{ConfigPath: "/eval/new.yaml", Name: "/eval/new.yaml", Total: 5, Passed: 5},
	}
	emitted, err := emitFindings(context.Background(), passing, time.Now().UTC())
	if err != nil {
		t.Fatalf("emitFindings with no prior db: %v", err)
	}
	if len(emitted) != 0 {
		t.Errorf("expected 0 findings, got %d", len(emitted))
	}
}

// TestEmitFindings_MediumSeverity verifies 60-79% pass rate produces medium severity.
func TestEmitFindings_MediumSeverity(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("ALLUKA_HOME", dir)

	// 7/10 = 70% → medium
	suites := []SuiteResult{
		{
			Name: "/eval/medium.yaml", Total: 10, Passed: 7, Failed: 3,
			FailedTests: []failedTest{
				{Description: "t1", Message: "m1", Source: "/eval/medium.yaml#t1"},
			},
		},
	}
	emitted, err := emitFindings(context.Background(), suites, time.Now().UTC())
	if err != nil {
		t.Fatalf("emitFindings: %v", err)
	}
	if len(emitted) != 1 {
		t.Fatalf("expected 1 finding, got %d", len(emitted))
	}
	if emitted[0].Severity != scan.SeverityMedium {
		t.Errorf("severity = %q, want %q", emitted[0].Severity, scan.SeverityMedium)
	}
}
