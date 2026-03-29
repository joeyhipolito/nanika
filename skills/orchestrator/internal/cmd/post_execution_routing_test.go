package cmd

import (
	"context"
	"path/filepath"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/routing"
)

// openTestRoutingDB creates a temporary on-disk RoutingDB for integration tests.
// Using a real SQLite file (rather than ":memory:") exercises the same code path
// that production uses while remaining isolated from ~/.alluka/learnings.db.
func openTestRoutingDB(t *testing.T) *routing.RoutingDB {
	t.Helper()
	path := filepath.Join(t.TempDir(), "learnings.db")
	rdb, err := routing.OpenDB(path)
	if err != nil {
		t.Fatalf("openTestRoutingDB: %v", err)
	}
	t.Cleanup(func() { rdb.Close() })
	return rdb
}

// ─── recordPostExecutionRoutingPatterns ──────────────────────────────────────

func TestRecordPostExecutionRoutingPatterns_WritesCompletedPhases(t *testing.T) {
	rdb := openTestRoutingDB(t)
	plan := &core.Plan{
		Task: "implement feature",
		Phases: []*core.Phase{
			{ID: "p1", Persona: "senior-backend-engineer", Status: core.StatusCompleted},
			{ID: "p2", Persona: "golang-engineer", Status: core.StatusCompleted},
		},
	}

	if err := recordPostExecutionRoutingPatterns(rdb, "ws-fresh-1", "repo:~/app", plan, "implementation"); err != nil {
		t.Fatalf("recordPostExecutionRoutingPatterns: %v", err)
	}

	patterns, err := rdb.GetRoutingPatterns(context.Background(), "repo:~/app")
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	// Both implementer phases should produce observations (reviewer-class personas
	// are excluded, but these are implementers).
	if len(patterns) != 2 {
		t.Fatalf("len(patterns) = %d, want 2", len(patterns))
	}
	personaSet := map[string]bool{}
	for _, p := range patterns {
		personaSet[p.Persona] = true
		if p.TaskHint != "implementation" {
			t.Errorf("TaskHint = %q, want implementation", p.TaskHint)
		}
	}
	if !personaSet["senior-backend-engineer"] {
		t.Error("missing routing pattern for senior-backend-engineer")
	}
	if !personaSet["golang-engineer"] {
		t.Error("missing routing pattern for golang-engineer")
	}
}

func TestRecordPostExecutionRoutingPatterns_NoOpOnEmptyWsID(t *testing.T) {
	rdb := openTestRoutingDB(t)
	plan := &core.Plan{
		Phases: []*core.Phase{
			{ID: "p1", Persona: "senior-backend-engineer", Status: core.StatusCompleted},
		},
	}

	if err := recordPostExecutionRoutingPatterns(rdb, "", "repo:~/app", plan, "implementation"); err != nil {
		t.Fatalf("empty wsID should be a no-op, got: %v", err)
	}

	patterns, err := rdb.GetRoutingPatterns(context.Background(), "repo:~/app")
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	if len(patterns) != 0 {
		t.Fatalf("len(patterns) = %d, want 0 (no-op)", len(patterns))
	}
}

func TestRecordPostExecutionRoutingPatterns_NoOpOnEmptyTargetID(t *testing.T) {
	rdb := openTestRoutingDB(t)
	plan := &core.Plan{
		Phases: []*core.Phase{
			{ID: "p1", Persona: "senior-backend-engineer", Status: core.StatusCompleted},
		},
	}

	if err := recordPostExecutionRoutingPatterns(rdb, "ws-1", "", plan, "implementation"); err != nil {
		t.Fatalf("empty targetID should be a no-op, got: %v", err)
	}

	// No target to query — just verify no panic and no write happened (an attempt
	// to query with empty targetID would return nothing meaningful anyway).
}

func TestRecordPostExecutionRoutingPatterns_NoOpOnNilPlan(t *testing.T) {
	rdb := openTestRoutingDB(t)

	if err := recordPostExecutionRoutingPatterns(rdb, "ws-1", "repo:~/app", nil, "implementation"); err != nil {
		t.Fatalf("nil plan should be a no-op, got: %v", err)
	}

	patterns, err := rdb.GetRoutingPatterns(context.Background(), "repo:~/app")
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	if len(patterns) != 0 {
		t.Fatalf("len(patterns) = %d, want 0 (no-op for nil plan)", len(patterns))
	}
}

func TestRecordPostExecutionRoutingPatterns_NoOpOnEmptyPhases(t *testing.T) {
	rdb := openTestRoutingDB(t)
	plan := &core.Plan{Phases: nil}

	if err := recordPostExecutionRoutingPatterns(rdb, "ws-1", "repo:~/app", plan, "implementation"); err != nil {
		t.Fatalf("empty phases should be a no-op, got: %v", err)
	}

	patterns, err := rdb.GetRoutingPatterns(context.Background(), "repo:~/app")
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	if len(patterns) != 0 {
		t.Fatalf("len(patterns) = %d, want 0 (no-op for empty phases)", len(patterns))
	}
}

// TestRecordPostExecutionRoutingPatterns_FreshRunPath simulates the wiring
// after a fresh runTask execution: all phases are newly completed.
func TestRecordPostExecutionRoutingPatterns_FreshRunPath(t *testing.T) {
	rdb := openTestRoutingDB(t)
	plan := &core.Plan{
		Task: "add new endpoint",
		Phases: []*core.Phase{
			{ID: "p1", Persona: "senior-backend-engineer", Status: core.StatusCompleted},
			{ID: "p2", Persona: "staff-code-reviewer", Status: core.StatusCompleted},
		},
	}

	if err := recordPostExecutionRoutingPatterns(rdb, "ws-fresh", "repo:~/app", plan, "implementation"); err != nil {
		t.Fatalf("fresh-run recordPostExecutionRoutingPatterns: %v", err)
	}

	patterns, err := rdb.GetRoutingPatterns(context.Background(), "repo:~/app")
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	// Reviewer phase is excluded; only the implementer phase yields a pattern.
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1 (reviewer excluded)", len(patterns))
	}
	if patterns[0].Persona != "senior-backend-engineer" {
		t.Errorf("Persona = %q, want senior-backend-engineer", patterns[0].Persona)
	}
	if patterns[0].SeenCount != 1 {
		t.Errorf("SeenCount = %d, want 1 (first observation)", patterns[0].SeenCount)
	}
}

// TestRecordPostExecutionRoutingPatterns_ResumeRunPath simulates the wiring
// after resumeMission: a plan where previously-failed phases were reset to
// pending and then completed on the resumed run.  The function must record
// only the phases that are now completed and must not be confused by the
// presence of skipped or still-failed phases that were not re-run.
func TestRecordPostExecutionRoutingPatterns_ResumeRunPath(t *testing.T) {
	rdb := openTestRoutingDB(t)

	// Simulate a resumed plan: p1 completed on first run, p2 failed and was
	// retried — now completed.  p3 was skipped (never ran).
	plan := &core.Plan{
		Task: "fix the regression",
		Phases: []*core.Phase{
			{ID: "p1", Persona: "senior-backend-engineer", Status: core.StatusCompleted},
			{ID: "p2", Persona: "golang-engineer", Status: core.StatusCompleted},
			{ID: "p3", Persona: "qa-engineer", Status: core.StatusSkipped},
		},
	}

	if err := recordPostExecutionRoutingPatterns(rdb, "ws-resume", "repo:~/app", plan, "bugfix"); err != nil {
		t.Fatalf("resume recordPostExecutionRoutingPatterns: %v", err)
	}

	patterns, err := rdb.GetRoutingPatterns(context.Background(), "repo:~/app")
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	// Only the two completed non-reviewer phases produce observations.
	if len(patterns) != 2 {
		t.Fatalf("len(patterns) = %d, want 2 (skipped phase excluded)", len(patterns))
	}
	for _, p := range patterns {
		if p.TaskHint != "bugfix" {
			t.Errorf("TaskHint = %q, want bugfix", p.TaskHint)
		}
	}
}

// ─── recordPostExecutionHandoffPatterns ──────────────────────────────────────

func TestRecordPostExecutionHandoffPatterns_WritesDependencyTransitions(t *testing.T) {
	rdb := openTestRoutingDB(t)
	plan := &core.Plan{
		Phases: []*core.Phase{
			{ID: "p1", Persona: "architect", Name: "design", Status: core.StatusCompleted},
			{ID: "p2", Persona: "senior-backend-engineer", Name: "implement", Status: core.StatusCompleted, Dependencies: []string{"p1"}},
		},
	}

	if err := recordPostExecutionHandoffPatterns(rdb, "ws-fresh-2", "repo:~/app", plan); err != nil {
		t.Fatalf("recordPostExecutionHandoffPatterns: %v", err)
	}

	patterns, err := rdb.GetHandoffPatterns(context.Background(), "repo:~/app")
	if err != nil {
		t.Fatalf("GetHandoffPatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1", len(patterns))
	}
	if patterns[0].FromPersona != "architect" {
		t.Errorf("FromPersona = %q, want architect", patterns[0].FromPersona)
	}
	if patterns[0].ToPersona != "senior-backend-engineer" {
		t.Errorf("ToPersona = %q, want senior-backend-engineer", patterns[0].ToPersona)
	}
	if patterns[0].TaskHint != "implement" {
		t.Errorf("TaskHint = %q, want implement", patterns[0].TaskHint)
	}
}

func TestRecordPostExecutionHandoffPatterns_NoOpOnEmptyWsID(t *testing.T) {
	rdb := openTestRoutingDB(t)
	plan := &core.Plan{
		Phases: []*core.Phase{
			{ID: "p1", Persona: "architect", Status: core.StatusCompleted},
			{ID: "p2", Persona: "senior-backend-engineer", Status: core.StatusCompleted, Dependencies: []string{"p1"}},
		},
	}

	if err := recordPostExecutionHandoffPatterns(rdb, "", "repo:~/app", plan); err != nil {
		t.Fatalf("empty wsID should be a no-op, got: %v", err)
	}

	patterns, err := rdb.GetHandoffPatterns(context.Background(), "repo:~/app")
	if err != nil {
		t.Fatalf("GetHandoffPatterns: %v", err)
	}
	if len(patterns) != 0 {
		t.Fatalf("len(patterns) = %d, want 0 (no-op)", len(patterns))
	}
}

func TestRecordPostExecutionHandoffPatterns_NoOpOnEmptyTargetID(t *testing.T) {
	rdb := openTestRoutingDB(t)
	plan := &core.Plan{
		Phases: []*core.Phase{
			{ID: "p1", Persona: "architect", Status: core.StatusCompleted},
			{ID: "p2", Persona: "senior-backend-engineer", Status: core.StatusCompleted, Dependencies: []string{"p1"}},
		},
	}

	if err := recordPostExecutionHandoffPatterns(rdb, "ws-1", "", plan); err != nil {
		t.Fatalf("empty targetID should be a no-op, got: %v", err)
	}
}

func TestRecordPostExecutionHandoffPatterns_NoOpOnNilPlan(t *testing.T) {
	rdb := openTestRoutingDB(t)

	if err := recordPostExecutionHandoffPatterns(rdb, "ws-1", "repo:~/app", nil); err != nil {
		t.Fatalf("nil plan should be a no-op, got: %v", err)
	}

	patterns, err := rdb.GetHandoffPatterns(context.Background(), "repo:~/app")
	if err != nil {
		t.Fatalf("GetHandoffPatterns: %v", err)
	}
	if len(patterns) != 0 {
		t.Fatalf("len(patterns) = %d, want 0 (no-op for nil plan)", len(patterns))
	}
}

func TestRecordPostExecutionHandoffPatterns_NoOpOnEmptyPhases(t *testing.T) {
	rdb := openTestRoutingDB(t)
	plan := &core.Plan{Phases: nil}

	if err := recordPostExecutionHandoffPatterns(rdb, "ws-1", "repo:~/app", plan); err != nil {
		t.Fatalf("empty phases should be a no-op, got: %v", err)
	}

	patterns, err := rdb.GetHandoffPatterns(context.Background(), "repo:~/app")
	if err != nil {
		t.Fatalf("GetHandoffPatterns: %v", err)
	}
	if len(patterns) != 0 {
		t.Fatalf("len(patterns) = %d, want 0 (no-op for empty phases)", len(patterns))
	}
}

// TestRecordPostExecutionHandoffPatterns_FreshRunPath simulates the wiring
// after a fresh runTask execution: a linear dependency chain all newly completed.
func TestRecordPostExecutionHandoffPatterns_FreshRunPath(t *testing.T) {
	rdb := openTestRoutingDB(t)
	plan := &core.Plan{
		Task: "scaffold and implement",
		Phases: []*core.Phase{
			{ID: "p1", Persona: "architect", Name: "scaffold", Status: core.StatusCompleted},
			{ID: "p2", Persona: "senior-backend-engineer", Name: "implement", Status: core.StatusCompleted, Dependencies: []string{"p1"}},
			{ID: "p3", Persona: "staff-code-reviewer", Name: "review", Status: core.StatusCompleted, Dependencies: []string{"p2"}},
		},
	}

	if err := recordPostExecutionHandoffPatterns(rdb, "ws-fresh-3", "repo:~/app", plan); err != nil {
		t.Fatalf("fresh-run recordPostExecutionHandoffPatterns: %v", err)
	}

	patterns, err := rdb.GetHandoffPatterns(context.Background(), "repo:~/app")
	if err != nil {
		t.Fatalf("GetHandoffPatterns: %v", err)
	}
	if len(patterns) != 2 {
		t.Fatalf("len(patterns) = %d, want 2 (architect→impl, impl→reviewer)", len(patterns))
	}
}

// TestRecordPostExecutionHandoffPatterns_ResumeRunPath simulates the wiring
// after resumeMission: the plan has a phase that previously failed and was
// re-run to completion.  Only edges where both endpoints completed must be
// recorded; edges touching a still-failed or skipped phase must be skipped.
func TestRecordPostExecutionHandoffPatterns_ResumeRunPath(t *testing.T) {
	rdb := openTestRoutingDB(t)

	// p1 completed on first run. p2 failed originally, now completed after
	// resume.  p3 was skipped entirely.
	plan := &core.Plan{
		Task: "fix regression and verify",
		Phases: []*core.Phase{
			{ID: "p1", Persona: "architect", Name: "plan", Status: core.StatusCompleted},
			{ID: "p2", Persona: "senior-backend-engineer", Name: "fix", Status: core.StatusCompleted, Dependencies: []string{"p1"}},
			{ID: "p3", Persona: "qa-engineer", Name: "verify", Status: core.StatusSkipped, Dependencies: []string{"p2"}},
		},
	}

	if err := recordPostExecutionHandoffPatterns(rdb, "ws-resume-2", "repo:~/app", plan); err != nil {
		t.Fatalf("resume recordPostExecutionHandoffPatterns: %v", err)
	}

	patterns, err := rdb.GetHandoffPatterns(context.Background(), "repo:~/app")
	if err != nil {
		t.Fatalf("GetHandoffPatterns: %v", err)
	}
	// architect→fix completed; fix→verify skipped — only the first edge recorded.
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1 (skipped-phase edge excluded)", len(patterns))
	}
	if patterns[0].FromPersona != "architect" {
		t.Errorf("FromPersona = %q, want architect", patterns[0].FromPersona)
	}
	if patterns[0].ToPersona != "senior-backend-engineer" {
		t.Errorf("ToPersona = %q, want senior-backend-engineer", patterns[0].ToPersona)
	}
}

// ─── runPostExecutionRecorders wiring ────────────────────────────────────────

// TestRunPostExecutionRecorders_NoOpOnMissingInputs verifies that
// runPostExecutionRecorders exits early and does not panic when guard
// conditions are not met (empty targetID, wsID, nil plan, empty phases).
func TestRunPostExecutionRecorders_NoOpOnMissingInputs(t *testing.T) {
	plan := &core.Plan{
		Phases: []*core.Phase{
			{ID: "p1", Persona: "senior-backend-engineer", Status: core.StatusCompleted},
		},
	}

	cases := []struct {
		name     string
		wsID     string
		targetID string
		plan     *core.Plan
	}{
		{"empty_wsID", "", "repo:~/app", plan},
		{"empty_targetID", "ws-1", "", plan},
		{"nil_plan", "ws-1", "repo:~/app", nil},
		{"empty_phases", "ws-1", "repo:~/app", &core.Plan{}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			// runPostExecutionRecorders tries to open the real routing DB when
			// inputs pass the guard.  With the guard inputs above it must return
			// before ever reaching OpenDB, so no real DB is needed.
			runPostExecutionRecorders(tc.wsID, tc.targetID, tc.plan, "success", "implementation")
			// reaching here without panic = pass
		})
	}
}
