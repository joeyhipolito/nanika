package learning

import (
	"testing"
)

func TestHeuristicScore_TierTable(t *testing.T) {
	tests := []struct {
		name string
		typ  LearningType
		want float64
	}{
		{name: "insight tier", typ: TypeInsight, want: 1.0},
		{name: "decision tier", typ: TypeDecision, want: 0.8},
		{name: "pattern tier", typ: TypePattern, want: 0.7},
		{name: "error tier", typ: TypeError, want: 0.6},
		{name: "source tier", typ: TypeSource, want: 0.4},
		{name: "preference tier", typ: LearningType("preference"), want: 0.3},
		{name: "behavior tier", typ: LearningType("behavior"), want: 0.3},
		{name: "unknown type falls through to default", typ: LearningType("mystery"), want: 0.5},
		{name: "empty type falls through to default", typ: LearningType(""), want: 0.5},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := HeuristicScore(tt.typ); got != tt.want {
				t.Errorf("HeuristicScore(%q) = %v, want %v", tt.typ, got, tt.want)
			}
		})
	}
}

func TestCaptureFromText_SetsQualityScore(t *testing.T) {
	// Content must be >=20 chars and end with terminal punctuation to pass isValidLearning.
	text := `
LEARNING: Workers should wrap every error with context to aid triage.
INSIGHT: Dream mining dedupes via sha256 of transcript chunks to skip stable files.
PATTERN: Table-driven subtests scale better than duplicate test functions in Go.
DECISION: Use stdlib flag package for CLI parsing instead of cobra to avoid a heavy dep.
GOTCHA: modernc.org/sqlite pure-Go driver lacks some pragmas that CGo sqlite3 supports.
SOURCE: See scripts/learnings-rescore.sql lines 12-22 for the tier table.
`

	learnings := CaptureFromText(text, "alpha", "dev", "test-workspace")
	if len(learnings) == 0 {
		t.Fatalf("CaptureFromText returned no learnings; expected at least one")
	}

	for _, l := range learnings {
		want := HeuristicScore(l.Type)
		if l.QualityScore != want {
			t.Errorf("learning %q (type=%s) QualityScore = %v, want %v",
				l.Marker, l.Type, l.QualityScore, want)
		}
	}

	// Spot-check: confirm we actually exercised multiple tiers, not just one.
	seen := make(map[LearningType]bool)
	for _, l := range learnings {
		seen[l.Type] = true
	}
	if len(seen) < 2 {
		t.Errorf("expected learnings across multiple types, got only %v", seen)
	}
}
