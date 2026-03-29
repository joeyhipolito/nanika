package routing

import (
	"context"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
)

// ─── RecordPlanRoutingPatterns ────────────────────────────────────────────────

func TestRecordPlanRoutingPatterns_RecordsCompletedPhases(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	plan := &core.Plan{
		Task: "implement the feature",
		Phases: []*core.Phase{
			{ID: "p1", Persona: "senior-backend-engineer", Status: core.StatusCompleted},
			// Reviewer phase must NOT be recorded (pollutes implementation signal).
			{ID: "p2", Persona: "staff-code-reviewer", Status: core.StatusCompleted},
		},
	}

	if err := rdb.RecordPlanRoutingPatterns(ctx, "repo:~/myapp", "implementation", plan); err != nil {
		t.Fatalf("RecordPlanRoutingPatterns: %v", err)
	}

	patterns, err := rdb.GetRoutingPatterns(ctx, "repo:~/myapp")
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	// Only the implementer phase should produce an observation; the reviewer phase
	// is excluded to prevent it from polluting the top-level routing signal.
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1 (reviewer phase excluded)", len(patterns))
	}
	if patterns[0].Persona != "senior-backend-engineer" {
		t.Errorf("Persona = %q, want senior-backend-engineer", patterns[0].Persona)
	}
	if patterns[0].TaskHint != "implementation" {
		t.Errorf("TaskHint = %q, want %q", patterns[0].TaskHint, "implementation")
	}
	if patterns[0].SeenCount != 1 {
		t.Errorf("SeenCount = %d, want 1", patterns[0].SeenCount)
	}
	if !approxEqual(patterns[0].Confidence, 0.2) {
		t.Errorf("Confidence = %f, want 0.2", patterns[0].Confidence)
	}
}

func TestRecordPlanRoutingPatterns_SkipsReviewerAndQAPhases(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	plan := &core.Plan{
		Task: "implement and review the feature",
		Phases: []*core.Phase{
			{ID: "p1", Persona: "senior-backend-engineer", Status: core.StatusCompleted},
			{ID: "p2", Persona: "staff-code-reviewer", Status: core.StatusCompleted},
			{ID: "p3", Persona: "qa-engineer", Status: core.StatusCompleted},
			{ID: "p4", Persona: "security-auditor", Status: core.StatusCompleted},
		},
	}

	if err := rdb.RecordPlanRoutingPatterns(ctx, "repo:~/myapp", "implementation", plan); err != nil {
		t.Fatalf("RecordPlanRoutingPatterns: %v", err)
	}

	patterns, err := rdb.GetRoutingPatterns(ctx, "repo:~/myapp")
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	// Only p1 (implementer) should be recorded. p2/p3/p4 are support personas
	// that must not pollute the top-level routing signal.
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1 (reviewer/QA phases excluded)", len(patterns))
	}
	if patterns[0].Persona != "senior-backend-engineer" {
		t.Errorf("Persona = %q, want senior-backend-engineer", patterns[0].Persona)
	}
}

func TestRecordPlanRoutingPatterns_DoesNotSkipImplementerWithReviewLikeText(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	plan := &core.Plan{
		Task: "fix the feature after review",
		Phases: []*core.Phase{
			{
				ID:        "p1",
				Persona:   "senior-backend-engineer",
				Name:      "address-review-feedback",
				Objective: "Fix review comments and patch the implementation",
				Status:    core.StatusCompleted,
			},
			{ID: "p2", Persona: "staff-code-reviewer", Name: "final-review", Status: core.StatusCompleted},
		},
	}

	if err := rdb.RecordPlanRoutingPatterns(ctx, "repo:~/myapp", "implementation", plan); err != nil {
		t.Fatalf("RecordPlanRoutingPatterns: %v", err)
	}

	patterns, err := rdb.GetRoutingPatterns(ctx, "repo:~/myapp")
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1", len(patterns))
	}
	if patterns[0].Persona != "senior-backend-engineer" {
		t.Fatalf("Persona = %q, want senior-backend-engineer", patterns[0].Persona)
	}
}

func TestRecordPlanRoutingPatterns_SkipsNonCompletedPhases(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	plan := &core.Plan{
		Task: "fix the bug",
		Phases: []*core.Phase{
			{ID: "p1", Persona: "senior-backend-engineer", Status: core.StatusCompleted},
			{ID: "p2", Persona: "golang-engineer", Status: core.StatusFailed},
			{ID: "p3", Persona: "staff-code-reviewer", Status: core.StatusSkipped},
			{ID: "p4", Persona: "qa-engineer", Status: core.StatusPending},
			{ID: "p5", Persona: "architect", Status: core.StatusRunning},
		},
	}

	if err := rdb.RecordPlanRoutingPatterns(ctx, "repo:~/myapp", "bugfix", plan); err != nil {
		t.Fatalf("RecordPlanRoutingPatterns: %v", err)
	}

	patterns, err := rdb.GetRoutingPatterns(ctx, "repo:~/myapp")
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	// Only the completed phase should produce an observation.
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1 (only completed phase)", len(patterns))
	}
	if patterns[0].Persona != "senior-backend-engineer" {
		t.Errorf("Persona = %q, want %q", patterns[0].Persona, "senior-backend-engineer")
	}
}

func TestRecordPlanRoutingPatterns_SkipsEmptyPersona(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	plan := &core.Plan{
		Task: "add a feature",
		Phases: []*core.Phase{
			{ID: "p1", Persona: "", Status: core.StatusCompleted},
			{ID: "p2", Persona: "senior-backend-engineer", Status: core.StatusCompleted},
		},
	}

	if err := rdb.RecordPlanRoutingPatterns(ctx, "repo:~/myapp", "implementation", plan); err != nil {
		t.Fatalf("RecordPlanRoutingPatterns: %v", err)
	}

	patterns, err := rdb.GetRoutingPatterns(ctx, "repo:~/myapp")
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1 (empty-persona phase skipped)", len(patterns))
	}
	if patterns[0].Persona != "senior-backend-engineer" {
		t.Errorf("Persona = %q, want senior-backend-engineer", patterns[0].Persona)
	}
}

func TestRecordPlanRoutingPatterns_EmptyTargetIDReturnsError(t *testing.T) {
	rdb := newTestDB(t)
	plan := &core.Plan{
		Phases: []*core.Phase{
			{ID: "p1", Persona: "senior-backend-engineer", Status: core.StatusCompleted},
		},
	}
	if err := rdb.RecordPlanRoutingPatterns(context.Background(), "", "implementation", plan); err == nil {
		t.Fatal("expected error for empty target_id, got nil")
	}
}

func TestRecordPlanRoutingPatterns_NilPlanIsNoOp(t *testing.T) {
	rdb := newTestDB(t)
	if err := rdb.RecordPlanRoutingPatterns(context.Background(), "repo:~/myapp", "", nil); err != nil {
		t.Fatalf("nil plan should be a no-op, got: %v", err)
	}
}

func TestRecordPlanRoutingPatterns_EmptyPhasesIsNoOp(t *testing.T) {
	rdb := newTestDB(t)
	plan := &core.Plan{Phases: nil}
	if err := rdb.RecordPlanRoutingPatterns(context.Background(), "repo:~/myapp", "", plan); err != nil {
		t.Fatalf("empty phases should be a no-op, got: %v", err)
	}
}

func TestRecordPlanRoutingPatterns_AccumulatesConfidenceAcrossRuns(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/myapp"

	phase := &core.Phase{ID: "p1", Persona: "senior-backend-engineer", Status: core.StatusCompleted}
	plan := &core.Plan{Phases: []*core.Phase{phase}}

	// Record the same persona five times (simulating five missions).
	for i := 0; i < 5; i++ {
		if err := rdb.RecordPlanRoutingPatterns(ctx, target, "implementation", plan); err != nil {
			t.Fatalf("RecordPlanRoutingPatterns[%d]: %v", i, err)
		}
	}

	patterns, err := rdb.GetRoutingPatterns(ctx, target)
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1", len(patterns))
	}
	if patterns[0].SeenCount != 5 {
		t.Errorf("SeenCount = %d, want 5", patterns[0].SeenCount)
	}
	// After 5 observations confidence should reach the cap of 1.0.
	if !approxEqual(patterns[0].Confidence, 1.0) {
		t.Errorf("Confidence = %f, want 1.0 (cap)", patterns[0].Confidence)
	}
}

func TestRecordPlanRoutingPatterns_EmptyTaskTypeStoredAsBlank(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	plan := &core.Plan{
		Phases: []*core.Phase{
			{ID: "p1", Persona: "senior-backend-engineer", Status: core.StatusCompleted},
		},
	}

	if err := rdb.RecordPlanRoutingPatterns(ctx, "repo:~/myapp", "", plan); err != nil {
		t.Fatalf("RecordPlanRoutingPatterns: %v", err)
	}

	patterns, err := rdb.GetRoutingPatterns(ctx, "repo:~/myapp")
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1", len(patterns))
	}
	if patterns[0].TaskHint != "" {
		t.Errorf("TaskHint = %q, want empty string", patterns[0].TaskHint)
	}
}

func TestRecordPlanRoutingPatterns_NilPhaseSkipped(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	plan := &core.Plan{
		Phases: []*core.Phase{
			nil,
			{ID: "p2", Persona: "senior-backend-engineer", Status: core.StatusCompleted},
		},
	}

	if err := rdb.RecordPlanRoutingPatterns(ctx, "repo:~/myapp", "implementation", plan); err != nil {
		t.Fatalf("RecordPlanRoutingPatterns with nil phase: %v", err)
	}

	patterns, err := rdb.GetRoutingPatterns(ctx, "repo:~/myapp")
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1 (nil phase skipped)", len(patterns))
	}
}

// ─── RecordPlanHandoffPatterns ──────────────────────────────────────────────

func TestRecordPlanHandoffPatterns_RecordsDependencyTransitions(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	plan := &core.Plan{
		Phases: []*core.Phase{
			{ID: "p1", Persona: "architect", Name: "design", Status: core.StatusCompleted},
			{ID: "p2", Persona: "senior-backend-engineer", Name: "implement", Status: core.StatusCompleted, Dependencies: []string{"p1"}},
		},
	}

	if err := rdb.RecordPlanHandoffPatterns(ctx, "repo:~/myapp", plan); err != nil {
		t.Fatalf("RecordPlanHandoffPatterns: %v", err)
	}

	patterns, err := rdb.GetHandoffPatterns(ctx, "repo:~/myapp")
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
	if patterns[0].SeenCount != 1 {
		t.Errorf("SeenCount = %d, want 1", patterns[0].SeenCount)
	}
	if !approxEqual(patterns[0].Confidence, 0.2) {
		t.Errorf("Confidence = %f, want 0.2", patterns[0].Confidence)
	}
}

func TestRecordPlanHandoffPatterns_SkipsSelfTransitions(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	plan := &core.Plan{
		Phases: []*core.Phase{
			{ID: "p1", Persona: "senior-backend-engineer", Name: "phase-a", Status: core.StatusCompleted},
			{ID: "p2", Persona: "senior-backend-engineer", Name: "phase-b", Status: core.StatusCompleted, Dependencies: []string{"p1"}},
		},
	}

	if err := rdb.RecordPlanHandoffPatterns(ctx, "repo:~/myapp", plan); err != nil {
		t.Fatalf("RecordPlanHandoffPatterns: %v", err)
	}

	patterns, err := rdb.GetHandoffPatterns(ctx, "repo:~/myapp")
	if err != nil {
		t.Fatalf("GetHandoffPatterns: %v", err)
	}
	if len(patterns) != 0 {
		t.Fatalf("len(patterns) = %d, want 0 (self-transition excluded)", len(patterns))
	}
}

func TestRecordPlanHandoffPatterns_SkipsNonCompletedPhases(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	plan := &core.Plan{
		Phases: []*core.Phase{
			{ID: "p1", Persona: "architect", Name: "design", Status: core.StatusCompleted},
			{ID: "p2", Persona: "senior-backend-engineer", Name: "implement", Status: core.StatusFailed, Dependencies: []string{"p1"}},
			{ID: "p3", Persona: "golang-engineer", Name: "fix", Status: core.StatusCompleted, Dependencies: []string{"p4"}},
			{ID: "p4", Persona: "architect", Name: "replan", Status: core.StatusSkipped},
		},
	}

	if err := rdb.RecordPlanHandoffPatterns(ctx, "repo:~/myapp", plan); err != nil {
		t.Fatalf("RecordPlanHandoffPatterns: %v", err)
	}

	patterns, err := rdb.GetHandoffPatterns(ctx, "repo:~/myapp")
	if err != nil {
		t.Fatalf("GetHandoffPatterns: %v", err)
	}
	// p1→p2: p2 failed → skipped. p4→p3: p4 skipped → skipped.
	if len(patterns) != 0 {
		t.Fatalf("len(patterns) = %d, want 0 (non-completed phases excluded)", len(patterns))
	}
}

func TestRecordPlanHandoffPatterns_SkipsEmptyPersona(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	plan := &core.Plan{
		Phases: []*core.Phase{
			{ID: "p1", Persona: "", Name: "setup", Status: core.StatusCompleted},
			{ID: "p2", Persona: "senior-backend-engineer", Name: "implement", Status: core.StatusCompleted, Dependencies: []string{"p1"}},
		},
	}

	if err := rdb.RecordPlanHandoffPatterns(ctx, "repo:~/myapp", plan); err != nil {
		t.Fatalf("RecordPlanHandoffPatterns: %v", err)
	}

	patterns, err := rdb.GetHandoffPatterns(ctx, "repo:~/myapp")
	if err != nil {
		t.Fatalf("GetHandoffPatterns: %v", err)
	}
	if len(patterns) != 0 {
		t.Fatalf("len(patterns) = %d, want 0 (empty-persona dependency excluded)", len(patterns))
	}
}

func TestRecordPlanHandoffPatterns_NilPlanIsNoOp(t *testing.T) {
	rdb := newTestDB(t)
	if err := rdb.RecordPlanHandoffPatterns(context.Background(), "repo:~/myapp", nil); err != nil {
		t.Fatalf("nil plan should be a no-op, got: %v", err)
	}
}

func TestRecordPlanHandoffPatterns_EmptyPhasesIsNoOp(t *testing.T) {
	rdb := newTestDB(t)
	plan := &core.Plan{Phases: nil}
	if err := rdb.RecordPlanHandoffPatterns(context.Background(), "repo:~/myapp", plan); err != nil {
		t.Fatalf("empty phases should be a no-op, got: %v", err)
	}
}

func TestRecordPlanHandoffPatterns_EmptyTargetIDReturnsError(t *testing.T) {
	rdb := newTestDB(t)
	plan := &core.Plan{
		Phases: []*core.Phase{
			{ID: "p1", Persona: "architect", Name: "design", Status: core.StatusCompleted},
			{ID: "p2", Persona: "senior-backend-engineer", Name: "impl", Status: core.StatusCompleted, Dependencies: []string{"p1"}},
		},
	}
	if err := rdb.RecordPlanHandoffPatterns(context.Background(), "", plan); err == nil {
		t.Fatal("expected error for empty target_id, got nil")
	}
}

func TestRecordPlanHandoffPatterns_NilPhaseSkipped(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	plan := &core.Plan{
		Phases: []*core.Phase{
			{ID: "p1", Persona: "architect", Name: "design", Status: core.StatusCompleted},
			nil,
			{ID: "p3", Persona: "senior-backend-engineer", Name: "implement", Status: core.StatusCompleted, Dependencies: []string{"p1"}},
		},
	}

	if err := rdb.RecordPlanHandoffPatterns(ctx, "repo:~/myapp", plan); err != nil {
		t.Fatalf("RecordPlanHandoffPatterns with nil phase: %v", err)
	}

	patterns, err := rdb.GetHandoffPatterns(ctx, "repo:~/myapp")
	if err != nil {
		t.Fatalf("GetHandoffPatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1 (nil phase skipped, valid handoff recorded)", len(patterns))
	}
}

func TestRecordPlanHandoffPatterns_AccumulatesConfidenceAcrossRuns(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/myapp"

	plan := &core.Plan{
		Phases: []*core.Phase{
			{ID: "p1", Persona: "architect", Name: "design", Status: core.StatusCompleted},
			{ID: "p2", Persona: "senior-backend-engineer", Name: "implement", Status: core.StatusCompleted, Dependencies: []string{"p1"}},
		},
	}

	// Record the same handoff five times (simulating five missions).
	for i := 0; i < 5; i++ {
		if err := rdb.RecordPlanHandoffPatterns(ctx, target, plan); err != nil {
			t.Fatalf("RecordPlanHandoffPatterns[%d]: %v", i, err)
		}
	}

	patterns, err := rdb.GetHandoffPatterns(ctx, target)
	if err != nil {
		t.Fatalf("GetHandoffPatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1", len(patterns))
	}
	if patterns[0].SeenCount != 5 {
		t.Errorf("SeenCount = %d, want 5", patterns[0].SeenCount)
	}
	if !approxEqual(patterns[0].Confidence, 1.0) {
		t.Errorf("Confidence = %f, want 1.0 (cap)", patterns[0].Confidence)
	}
}

func TestRecordPlanHandoffPatterns_MultipleDependencies(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	// p3 depends on both p1 and p2 — two distinct handoffs should be recorded.
	plan := &core.Plan{
		Phases: []*core.Phase{
			{ID: "p1", Persona: "architect", Name: "design", Status: core.StatusCompleted},
			{ID: "p2", Persona: "security-auditor", Name: "threat-model", Status: core.StatusCompleted},
			{ID: "p3", Persona: "senior-backend-engineer", Name: "implement", Status: core.StatusCompleted, Dependencies: []string{"p1", "p2"}},
		},
	}

	if err := rdb.RecordPlanHandoffPatterns(ctx, "repo:~/myapp", plan); err != nil {
		t.Fatalf("RecordPlanHandoffPatterns: %v", err)
	}

	patterns, err := rdb.GetHandoffPatterns(ctx, "repo:~/myapp")
	if err != nil {
		t.Fatalf("GetHandoffPatterns: %v", err)
	}
	if len(patterns) != 2 {
		t.Fatalf("len(patterns) = %d, want 2 (one per dependency)", len(patterns))
	}

	fromPersonas := map[string]bool{}
	for _, p := range patterns {
		fromPersonas[p.FromPersona] = true
		if p.ToPersona != "senior-backend-engineer" {
			t.Errorf("ToPersona = %q, want senior-backend-engineer", p.ToPersona)
		}
	}
	if !fromPersonas["architect"] {
		t.Error("missing handoff from architect")
	}
	if !fromPersonas["security-auditor"] {
		t.Error("missing handoff from security-auditor")
	}
}

func TestRecordPlanHandoffPatterns_NoDependenciesIsNoOp(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	// Two completed phases but no dependency edges — no handoffs.
	plan := &core.Plan{
		Phases: []*core.Phase{
			{ID: "p1", Persona: "architect", Name: "design", Status: core.StatusCompleted},
			{ID: "p2", Persona: "senior-backend-engineer", Name: "implement", Status: core.StatusCompleted},
		},
	}

	if err := rdb.RecordPlanHandoffPatterns(ctx, "repo:~/myapp", plan); err != nil {
		t.Fatalf("RecordPlanHandoffPatterns: %v", err)
	}

	patterns, err := rdb.GetHandoffPatterns(ctx, "repo:~/myapp")
	if err != nil {
		t.Fatalf("GetHandoffPatterns: %v", err)
	}
	if len(patterns) != 0 {
		t.Fatalf("len(patterns) = %d, want 0 (no dependency edges)", len(patterns))
	}
}
