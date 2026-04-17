package engine

// Table-driven tests for engine-level behaviours:
//
//  1. Max-turns default guardrail: WorkerConfig.MaxTurns is set to 50 when <= 0
//  2. Session ID persistence: phase.SessionID is updated when a worker returns one
//  3. Retry uses resume: config.ResumeSessionID is populated from phase.SessionID before each attempt
//  4. skipDependents: returns count of skipped phases for completion tracking
//  5. Parallel deadlock fix: event loop terminates when phases are skipped
//  6. TargetDir inheritance: workspace.TargetDir propagates to phases that have none

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/sdk"
)

// ---------------------------------------------------------------------------
// 4. Max-turns default guardrail
// ---------------------------------------------------------------------------

// applyMaxTurnsDefault replicates the guardrail logic in executePhase.
// It is defined here so the table test documents and verifies the exact
// contract: MaxTurns <= 0 → resolveMaxTurns (persona-aware); MaxTurns > 0 → unchanged.
func applyMaxTurnsDefault(config *core.WorkerConfig, persona string) {
	if config.MaxTurns <= 0 {
		config.MaxTurns = resolveMaxTurns(0, persona)
	}
}

func TestDefaultMaxTurnsTable(t *testing.T) {
	tests := []struct {
		name         string
		initialTurns int
		wantTurns    int
	}{
		{
			name:         "zero is replaced by default (50)",
			initialTurns: 0,
			wantTurns:    50,
		},
		{
			name:         "negative is replaced by default (50)",
			initialTurns: -1,
			wantTurns:    50,
		},
		{
			name:         "explicitly set value is preserved",
			initialTurns: 20,
			wantTurns:    20,
		},
		{
			name:         "default value itself (50) is preserved unchanged",
			initialTurns: 50,
			wantTurns:    50,
		},
		{
			name:         "value above default is preserved",
			initialTurns: 100,
			wantTurns:    100,
		},
		{
			name:         "single turn (1) is preserved",
			initialTurns: 1,
			wantTurns:    1,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			config := &core.WorkerConfig{MaxTurns: tt.initialTurns}
			applyMaxTurnsDefault(config, "")
			if config.MaxTurns != tt.wantTurns {
				t.Errorf("MaxTurns = %d; want %d", config.MaxTurns, tt.wantTurns)
			}
		})
	}
}

func TestResolveMaxTurns(t *testing.T) {
	tests := []struct {
		name           string
		engineMaxTurns int
		persona        string
		want           int
	}{
		{
			name:           "global flag overrides persona tier",
			engineMaxTurns: 25,
			persona:        "architect",
			want:           25,
		},
		{
			name:           "architect gets 30 when no global override",
			engineMaxTurns: 0,
			persona:        "architect",
			want:           30,
		},
		{
			name:           "architect case-insensitive",
			engineMaxTurns: 0,
			persona:        "Architect",
			want:           30,
		},
		{
			name:           "unknown persona gets default 50",
			engineMaxTurns: 0,
			persona:        "senior-backend-engineer",
			want:           50,
		},
		{
			name:           "empty persona gets default 50",
			engineMaxTurns: 0,
			persona:        "",
			want:           50,
		},
		{
			name:           "global flag overrides unknown persona",
			engineMaxTurns: 75,
			persona:        "researcher",
			want:           75,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := resolveMaxTurns(tt.engineMaxTurns, tt.persona)
			if got != tt.want {
				t.Errorf("resolveMaxTurns(%d, %q) = %d; want %d",
					tt.engineMaxTurns, tt.persona, got, tt.want)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// 2. Session ID persistence: phase.SessionID is updated when worker returns one
// ---------------------------------------------------------------------------

// updatePhaseSessionID replicates the session-ID capture logic in executePhase:
//
//	if sessionID != "" {
//	    phase.SessionID = sessionID
//	}
//
// It is extracted here to make the contract explicit and testable.
func updatePhaseSessionID(phase *core.Phase, sessionID string) {
	if sessionID != "" {
		phase.SessionID = sessionID
	}
}

func TestSessionIDPersistenceTable(t *testing.T) {
	tests := []struct {
		name            string
		priorSessionID  string // phase.SessionID before the worker run
		workerSessionID string // sessionID returned by worker.Execute
		wantSessionID   string // phase.SessionID after updatePhaseSessionID
	}{
		{
			name:            "empty prior ID updated when worker returns one",
			priorSessionID:  "",
			workerSessionID: "sess-new-abc",
			wantSessionID:   "sess-new-abc",
		},
		{
			name:            "prior ID replaced when worker returns a different one",
			priorSessionID:  "sess-old-xyz",
			workerSessionID: "sess-new-abc",
			wantSessionID:   "sess-new-abc",
		},
		{
			name:            "prior ID preserved when worker returns empty (no session captured)",
			priorSessionID:  "sess-old-xyz",
			workerSessionID: "",
			wantSessionID:   "sess-old-xyz",
		},
		{
			name:            "both empty: stays empty",
			priorSessionID:  "",
			workerSessionID: "",
			wantSessionID:   "",
		},
		{
			name:            "UUID-format session ID is stored verbatim",
			priorSessionID:  "",
			workerSessionID: "550e8400-e29b-41d4-a716-446655440000",
			wantSessionID:   "550e8400-e29b-41d4-a716-446655440000",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			phase := &core.Phase{SessionID: tt.priorSessionID}
			updatePhaseSessionID(phase, tt.workerSessionID)
			if phase.SessionID != tt.wantSessionID {
				t.Errorf("phase.SessionID = %q; want %q", phase.SessionID, tt.wantSessionID)
			}
		})
	}
}

func TestBuildHandoffs(t *testing.T) {
	e := &Engine{
		phases: map[string]*core.Phase{
			"plan": {
				ID:      "plan",
				Persona: "architect",
				Role:    core.RolePlanner,
				Status:  core.StatusCompleted,
				Output:  "Created the implementation plan and key constraints.",
			},
			"impl": {
				ID:           "impl",
				Persona:      "senior-backend-engineer",
				Role:         core.RoleImplementer,
				Status:       core.StatusCompleted,
				Dependencies: []string{"plan"},
				Output:       "Implemented the auth flow.",
			},
			"same-role": {
				ID:      "same-role",
				Persona: "qa-engineer",
				Role:    core.RoleReviewer,
				Status:  core.StatusCompleted,
				Output:  "Reviewer notes.",
			},
		},
	}

	phase := &core.Phase{
		ID:           "review",
		Persona:      "staff-code-reviewer",
		Role:         core.RoleReviewer,
		Dependencies: []string{"plan", "same-role", "impl"},
	}
	e.phases["review"] = phase

	handoffs := e.buildHandoffs(phase)
	if len(handoffs) != 2 {
		t.Fatalf("len(handoffs) = %d, want 2", len(handoffs))
	}

	if handoffs[0].FromPhaseID != "plan" || handoffs[0].FromRole != core.RolePlanner || handoffs[0].ToRole != core.RoleReviewer {
		t.Fatalf("handoff[0] = %+v, want planner->reviewer from plan", handoffs[0])
	}
	if handoffs[1].FromPhaseID != "impl" || handoffs[1].FromRole != core.RoleImplementer || handoffs[1].ToRole != core.RoleReviewer {
		t.Fatalf("handoff[1] = %+v, want implementer->reviewer from impl", handoffs[1])
	}
	if len(handoffs[1].Expectations) == 0 {
		t.Fatal("handoff[1] expectations should not be empty")
	}
}

// ---------------------------------------------------------------------------
// skipDependents: returns count of skipped phases (deadlock fix regression)
// ---------------------------------------------------------------------------

func TestSkipDependentsCount(t *testing.T) {
	tests := []struct {
		name         string
		phases       []*core.Phase               // all phases in the DAG
		failedID     string                      // the phase that failed
		pendingIDs   []string                    // phases still pending when failure occurs
		wantSkipped  int                         // expected return value
		wantStatuses map[string]core.PhaseStatus // expected statuses after skip
	}{
		{
			name: "single direct dependent is skipped",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusFailed},
				{ID: "b", Name: "B", Status: core.StatusPending, Dependencies: []string{"a"}},
			},
			failedID:    "a",
			pendingIDs:  []string{"b"},
			wantSkipped: 1,
			wantStatuses: map[string]core.PhaseStatus{
				"b": core.StatusSkipped,
			},
		},
		{
			name: "transitive dependents are skipped (A->B->C)",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusFailed},
				{ID: "b", Name: "B", Status: core.StatusPending, Dependencies: []string{"a"}},
				{ID: "c", Name: "C", Status: core.StatusPending, Dependencies: []string{"b"}},
			},
			failedID:    "a",
			pendingIDs:  []string{"b", "c"},
			wantSkipped: 2,
			wantStatuses: map[string]core.PhaseStatus{
				"b": core.StatusSkipped,
				"c": core.StatusSkipped,
			},
		},
		{
			name: "diamond: A fails, B and C depend on A, D depends on B and C",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusFailed},
				{ID: "b", Name: "B", Status: core.StatusPending, Dependencies: []string{"a"}},
				{ID: "c", Name: "C", Status: core.StatusPending, Dependencies: []string{"a"}},
				{ID: "d", Name: "D", Status: core.StatusPending, Dependencies: []string{"b", "c"}},
			},
			failedID:    "a",
			pendingIDs:  []string{"b", "c", "d"},
			wantSkipped: 3,
			wantStatuses: map[string]core.PhaseStatus{
				"b": core.StatusSkipped,
				"c": core.StatusSkipped,
				"d": core.StatusSkipped,
			},
		},
		{
			name: "independent phase is not skipped",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusFailed},
				{ID: "b", Name: "B", Status: core.StatusPending, Dependencies: []string{"a"}},
				{ID: "c", Name: "C", Status: core.StatusPending}, // no deps
			},
			failedID:    "a",
			pendingIDs:  []string{"b", "c"},
			wantSkipped: 1,
			wantStatuses: map[string]core.PhaseStatus{
				"b": core.StatusSkipped,
				"c": core.StatusPending,
			},
		},
		{
			name: "no dependents: nothing skipped",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusFailed},
				{ID: "b", Name: "B", Status: core.StatusPending}, // no dep on a
			},
			failedID:    "a",
			pendingIDs:  []string{"b"},
			wantSkipped: 0,
			wantStatuses: map[string]core.PhaseStatus{
				"b": core.StatusPending,
			},
		},
		{
			name: "deep chain: A->B->C->D all skipped",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusFailed},
				{ID: "b", Name: "B", Status: core.StatusPending, Dependencies: []string{"a"}},
				{ID: "c", Name: "C", Status: core.StatusPending, Dependencies: []string{"b"}},
				{ID: "d", Name: "D", Status: core.StatusPending, Dependencies: []string{"c"}},
			},
			failedID:    "a",
			pendingIDs:  []string{"b", "c", "d"},
			wantSkipped: 3,
			wantStatuses: map[string]core.PhaseStatus{
				"b": core.StatusSkipped,
				"c": core.StatusSkipped,
				"d": core.StatusSkipped,
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			eng := &Engine{
				phases:    make(map[string]*core.Phase),
				emitter:   event.NoOpEmitter{},
				workspace: &core.Workspace{ID: "test"},
			}
			for _, p := range tt.phases {
				eng.phases[p.ID] = p
			}
			pending := make(map[string]bool)
			for _, id := range tt.pendingIDs {
				pending[id] = true
			}

			got := eng.skipDependents(context.Background(), tt.failedID, pending)
			if got != tt.wantSkipped {
				t.Errorf("skipDependents() returned %d; want %d", got, tt.wantSkipped)
			}

			for id, wantStatus := range tt.wantStatuses {
				phase := eng.phases[id]
				if phase.Status != wantStatus {
					t.Errorf("phase %s status = %q; want %q", id, phase.Status, wantStatus)
				}
			}
		})
	}
}

func TestSkipSequentialDependents(t *testing.T) {
	phases := []*core.Phase{
		{ID: "phase-1", Name: "implement", Status: core.StatusFailed},
		{ID: "phase-2", Name: "review", Status: core.StatusPending, Dependencies: []string{"phase-1"}},
		{ID: "phase-3", Name: "validate", Status: core.StatusPending, Dependencies: []string{"phase-2"}},
		{ID: "phase-4", Name: "publish", Status: core.StatusPending},
	}
	eng := &Engine{
		phases:    make(map[string]*core.Phase),
		emitter:   event.NoOpEmitter{},
		workspace: &core.Workspace{ID: "test"},
	}
	plan := &core.Plan{Phases: phases}
	for _, p := range phases {
		eng.phases[p.ID] = p
	}

	got := eng.skipSequentialDependents(context.Background(), plan, 0, "phase-1")
	if got != 2 {
		t.Fatalf("skipSequentialDependents() = %d; want 2", got)
	}
	if phases[1].Status != core.StatusSkipped {
		t.Errorf("phase-2 status = %q; want skipped", phases[1].Status)
	}
	if phases[2].Status != core.StatusSkipped {
		t.Errorf("phase-3 status = %q; want skipped", phases[2].Status)
	}
	if phases[3].Status != core.StatusPending {
		t.Errorf("phase-4 status = %q; want pending", phases[3].Status)
	}
}

func TestExecuteSequential_ReviewLoopInjectsAndRunsFixPhase(t *testing.T) {
	tmp := t.TempDir()
	fakeClaude := filepath.Join(tmp, "claude")
	script := `#!/bin/sh
prompt=""
last=""
for arg in "$@"; do
  if [ "$last" = "-p" ]; then
    prompt="$arg"
    break
  fi
  last="$arg"
done

if printf "%s" "$prompt" | grep -q "Fix the following code review blockers"; then
  printf '%s\n' '{"type":"assistant","content":[{"type":"text","text":"applied fixes"}]}'
elif printf "%s" "$prompt" | grep -q "Review the implementation"; then
  printf '%s\n' '{"type":"assistant","content":[{"type":"text","text":"### Blockers\n- **[engine.go:123]** Missing regression coverage for sequential review loop."}]}'
else
  printf '%s\n' '{"type":"assistant","content":[{"type":"text","text":"implemented feature"}]}'
fi
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-test","num_turns":1,"duration_ms":1}'
`
	if err := os.WriteFile(fakeClaude, []byte(script), 0700); err != nil {
		t.Fatalf("write fake claude: %v", err)
	}
	t.Setenv("PATH", tmp+":"+os.Getenv("PATH"))

	ws := &core.Workspace{
		ID:     "ws-review-loop",
		Path:   filepath.Join(tmp, "ws"),
		Domain: "dev",
	}
	if err := os.MkdirAll(ws.Path, 0700); err != nil {
		t.Fatalf("mkdir workspace: %v", err)
	}

	plan := &core.Plan{
		ID:            "plan-test",
		Task:          "test review loop",
		ExecutionMode: "sequential",
		Phases: []*core.Phase{
			{
				ID:        "phase-1",
				Name:      "implement",
				Objective: "Implement the feature",
				Persona:   "senior-backend-engineer",
				ModelTier: "work",
				Status:    core.StatusPending,
			},
			{
				ID:                     "phase-2",
				Name:                   "review",
				Objective:              "Review the implementation",
				Persona:                "staff-code-reviewer",
				ModelTier:              "think",
				Dependencies:           []string{"phase-1"},
				Status:                 core.StatusPending,
				PersonaSelectionMethod: core.SelectionRequiredReview,
				MaxReviewLoops:         1,
			},
		},
	}

	eng := &Engine{
		workspace: ws,
		config: &core.OrchestratorConfig{
			ForceSequential: true,
		},
		emitter: event.NoOpEmitter{},
		phases:  make(map[string]*core.Phase),
	}

	result, err := eng.Execute(context.Background(), plan)
	if err != nil {
		t.Fatalf("Execute() error: %v", err)
	}
	if !result.Success {
		t.Fatalf("expected success, got result error: %s", result.Error)
	}
	// impl + review + fix + re-review = 4 phases after the remediation loop.
	if len(plan.Phases) != 4 {
		t.Fatalf("expected 4 phases (impl, review, fix, re-review), got %d", len(plan.Phases))
	}
	fix := plan.Phases[2]
	if fix.Name != "fix" {
		t.Fatalf("injected fix phase name = %q, want fix", fix.Name)
	}
	if fix.Status != core.StatusCompleted {
		t.Fatalf("fix phase status = %q, want completed", fix.Status)
	}
	if fix.Persona != "senior-backend-engineer" {
		t.Fatalf("fix phase persona = %q, want senior-backend-engineer", fix.Persona)
	}
	reReview := plan.Phases[3]
	if reReview.Name != "re-review" {
		t.Fatalf("injected re-review phase name = %q, want re-review", reReview.Name)
	}
	if reReview.PersonaSelectionMethod != core.SelectionRequiredReview {
		t.Fatalf("re-review PersonaSelectionMethod = %q, want %q", reReview.PersonaSelectionMethod, core.SelectionRequiredReview)
	}
	if reReview.Status != core.StatusCompleted {
		t.Fatalf("re-review phase status = %q, want completed", reReview.Status)
	}
	if !strings.Contains(result.Output, "applied fixes") {
		t.Fatalf("combined output missing fix phase output: %q", result.Output)
	}
}

func TestExecuteSequential_ReviewLoopRunsForExplicitReviewerPhase(t *testing.T) {
	tmp := t.TempDir()
	fakeClaude := filepath.Join(tmp, "claude")
	script := `#!/bin/sh
prompt=""
last=""
for arg in "$@"; do
  if [ "$last" = "-p" ]; then
    prompt="$arg"
    break
  fi
  last="$arg"
done

if printf "%s" "$prompt" | grep -q "Fix the following code review blockers"; then
  printf '%s\n' '{"type":"assistant","content":[{"type":"text","text":"applied fixes"}]}'
elif printf "%s" "$prompt" | grep -q "Review the implementation"; then
  printf '%s\n' '{"type":"assistant","content":[{"type":"text","text":"### Blockers\n- **[engine.go:123]** Missing regression coverage for explicit reviewer loops."}]}'
else
  printf '%s\n' '{"type":"assistant","content":[{"type":"text","text":"implemented feature"}]}'
fi
printf '%s\n' '{"type":"result","subtype":"success","session_id":"sess-test","num_turns":1,"duration_ms":1}'
`
	if err := os.WriteFile(fakeClaude, []byte(script), 0700); err != nil {
		t.Fatalf("write fake claude: %v", err)
	}
	t.Setenv("PATH", tmp+":"+os.Getenv("PATH"))

	ws := &core.Workspace{
		ID:     "ws-explicit-review-loop",
		Path:   filepath.Join(tmp, "ws"),
		Domain: "dev",
	}
	if err := os.MkdirAll(ws.Path, 0700); err != nil {
		t.Fatalf("mkdir workspace: %v", err)
	}

	plan := &core.Plan{
		ID:            "plan-explicit-review",
		Task:          "test explicit review loop",
		ExecutionMode: "sequential",
		Phases: []*core.Phase{
			{
				ID:        "phase-1",
				Name:      "implement",
				Objective: "Implement the feature",
				Persona:   "senior-backend-engineer",
				ModelTier: "work",
				Role:      core.RoleImplementer,
				Status:    core.StatusPending,
			},
			{
				ID:             "phase-2",
				Name:           "review",
				Objective:      "Review the implementation",
				Persona:        "staff-code-reviewer",
				ModelTier:      "think",
				Role:           core.RoleReviewer,
				Dependencies:   []string{"phase-1"},
				Status:         core.StatusPending,
				MaxReviewLoops: 1,
			},
		},
	}

	eng := &Engine{
		workspace: ws,
		config: &core.OrchestratorConfig{
			ForceSequential: true,
		},
		emitter: event.NoOpEmitter{},
		phases:  make(map[string]*core.Phase),
	}

	result, err := eng.Execute(context.Background(), plan)
	if err != nil {
		t.Fatalf("Execute() error: %v", err)
	}
	if !result.Success {
		t.Fatalf("expected success, got result error: %s", result.Error)
	}
	if len(plan.Phases) != 4 {
		t.Fatalf("expected 4 phases (impl, review, fix, re-review), got %d", len(plan.Phases))
	}
	if plan.Phases[2].Name != "fix" || plan.Phases[3].Name != "re-review" {
		t.Fatalf("unexpected injected phases: %q, %q", plan.Phases[2].Name, plan.Phases[3].Name)
	}
}

// ---------------------------------------------------------------------------
// Parallel deadlock fix: event loop terminates when phases are skipped
// ---------------------------------------------------------------------------

// TestParallelLoopTerminatesOnSkip verifies that the parallel event loop
// (for completed < total) terminates when a phase fails and its dependents
// are skipped. This is the regression test for the deadlock bug where
// skipped phases were not counted toward completion.
func TestParallelLoopTerminatesOnSkip(t *testing.T) {
	tests := []struct {
		name       string
		phases     []*core.Phase
		failPhases map[string]bool // phase IDs that will fail
		wantStatus map[string]core.PhaseStatus
	}{
		{
			name: "single failure skips dependent, loop terminates",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusPending},
				{ID: "b", Name: "B", Status: core.StatusPending, Dependencies: []string{"a"}},
			},
			failPhases: map[string]bool{"a": true},
			wantStatus: map[string]core.PhaseStatus{
				"a": core.StatusFailed,
				"b": core.StatusSkipped,
			},
		},
		{
			name: "failure with transitive skip chain, loop terminates",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusPending},
				{ID: "b", Name: "B", Status: core.StatusPending, Dependencies: []string{"a"}},
				{ID: "c", Name: "C", Status: core.StatusPending, Dependencies: []string{"b"}},
			},
			failPhases: map[string]bool{"a": true},
			wantStatus: map[string]core.PhaseStatus{
				"a": core.StatusFailed,
				"b": core.StatusSkipped,
				"c": core.StatusSkipped,
			},
		},
		{
			name: "failure with independent phase succeeding",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusPending},
				{ID: "b", Name: "B", Status: core.StatusPending, Dependencies: []string{"a"}},
				{ID: "c", Name: "C", Status: core.StatusPending}, // independent
			},
			failPhases: map[string]bool{"a": true},
			wantStatus: map[string]core.PhaseStatus{
				"a": core.StatusFailed,
				"b": core.StatusSkipped,
				"c": core.StatusCompleted,
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			eng := &Engine{
				phases:    make(map[string]*core.Phase),
				emitter:   event.NoOpEmitter{},
				config:    &core.OrchestratorConfig{},
				workspace: &core.Workspace{ID: "test"},
			}
			for _, p := range tt.phases {
				eng.phases[p.ID] = p
			}

			// Simulate the parallel event loop inline.
			// This replicates the completion-tracking logic from executeParallel
			// without spawning real workers.
			type phaseResult struct {
				phaseID string
				output  string
				err     error
			}

			pending := make(map[string]bool)
			for _, p := range tt.phases {
				pending[p.ID] = true
			}

			completionCh := make(chan phaseResult, len(tt.phases))

			// depsCompleted checks if all dependencies are completed.
			depsCompleted := func(phase *core.Phase) bool {
				for _, depID := range phase.Dependencies {
					dep := eng.phases[depID]
					if dep.Status != core.StatusCompleted {
						return false
					}
				}
				return true
			}

			// dispatch sends ready phases to the completion channel.
			dispatch := func() {
				for id := range pending {
					phase := eng.phases[id]
					if phase.Status != core.StatusPending {
						continue
					}
					if !depsCompleted(phase) {
						continue
					}
					phase.Status = core.StatusRunning
					phaseID := id
					if tt.failPhases[phaseID] {
						completionCh <- phaseResult{phaseID, "", fmt.Errorf("simulated failure")}
					} else {
						completionCh <- phaseResult{phaseID, "ok", nil}
					}
				}
			}

			dispatch()

			completed := 0
			total := len(tt.phases)

			// Use a timeout to detect the deadlock.
			deadline := time.After(2 * time.Second)

			for completed < total {
				select {
				case <-deadline:
					t.Fatalf("DEADLOCK: event loop did not terminate (completed=%d, total=%d)", completed, total)

				case pr := <-completionCh:
					completed++
					delete(pending, pr.phaseID)

					phase := eng.phases[pr.phaseID]
					if pr.err != nil {
						phase.Status = core.StatusFailed
						phase.Error = pr.err.Error()
						completed += eng.skipDependents(context.Background(), pr.phaseID, pending)
					} else {
						phase.Status = core.StatusCompleted
						phase.Output = pr.output
					}

					dispatch()
				}
			}

			// Verify statuses.
			for id, want := range tt.wantStatus {
				got := eng.phases[id].Status
				if got != want {
					t.Errorf("phase %s: status = %q; want %q", id, got, want)
				}
			}
		})
	}
}

// ---------------------------------------------------------------------------
// 3. Retry uses resume: config.ResumeSessionID is set from phase.SessionID
// ---------------------------------------------------------------------------

// setResumeSessionID replicates the assignment that runs before each retry
// attempt in executePhase:
//
//	config.ResumeSessionID = phase.SessionID
//
// Extracted here to pin the contract.
func setResumeSessionID(config *core.WorkerConfig, phase *core.Phase) {
	config.ResumeSessionID = phase.SessionID
}

func TestRetryUsesResumeTable(t *testing.T) {
	tests := []struct {
		name           string
		phaseSessionID string // value persisted from prior attempt
		wantResumeID   string // what config.ResumeSessionID should be set to
	}{
		{
			name:           "session ID from prior attempt is passed to retry",
			phaseSessionID: "sess-from-attempt-1",
			wantResumeID:   "sess-from-attempt-1",
		},
		{
			name:           "empty session ID means fresh start (no resume)",
			phaseSessionID: "",
			wantResumeID:   "",
		},
		{
			name:           "UUID session ID flows through unchanged",
			phaseSessionID: "550e8400-e29b-41d4-a716-446655440000",
			wantResumeID:   "550e8400-e29b-41d4-a716-446655440000",
		},
		{
			name:           "first attempt with no checkpoint has empty resume ID",
			phaseSessionID: "", // no checkpoint SessionID → fresh start
			wantResumeID:   "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			phase := &core.Phase{SessionID: tt.phaseSessionID}
			config := &core.WorkerConfig{}

			setResumeSessionID(config, phase)

			if config.ResumeSessionID != tt.wantResumeID {
				t.Errorf("config.ResumeSessionID = %q; want %q",
					config.ResumeSessionID, tt.wantResumeID)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// Combined: full retry lifecycle — session ID persists, then used on retry
// ---------------------------------------------------------------------------

// TestRetryLifecycleTable exercises the three-step retry loop contract:
//
//  1. config.ResumeSessionID = phase.SessionID  (before attempt N)
//  2. worker runs, returns sessionID
//  3. if sessionID != "" → phase.SessionID = sessionID  (checkpoint update)
//
// On the next attempt, step 1 passes the newly captured session ID.
func TestRetryLifecycleTable(t *testing.T) {
	tests := []struct {
		name                string
		checkpointSessID    string   // session ID loaded from checkpoint (first attempt)
		workerResponses     []string // sessionID returned by worker on each attempt (empty = failure with no ID)
		wantFinalSessID     string   // phase.SessionID after all attempts
		wantResumeOnAttempt []string // config.ResumeSessionID at the start of each attempt
	}{
		{
			name:                "no prior session: first attempt starts fresh",
			checkpointSessID:    "",
			workerResponses:     []string{"sess-001"},
			wantFinalSessID:     "sess-001",
			wantResumeOnAttempt: []string{""},
		},
		{
			name:                "first attempt captures ID; second attempt resumes",
			checkpointSessID:    "",
			workerResponses:     []string{"sess-001", "sess-002"},
			wantFinalSessID:     "sess-002",
			wantResumeOnAttempt: []string{"", "sess-001"},
		},
		{
			name:                "checkpoint provides ID; first attempt resumes from it",
			checkpointSessID:    "sess-from-checkpoint",
			workerResponses:     []string{"sess-new-001"},
			wantFinalSessID:     "sess-new-001",
			wantResumeOnAttempt: []string{"sess-from-checkpoint"},
		},
		{
			name:             "worker returns no ID on failure; prior ID is preserved for next retry",
			checkpointSessID: "sess-prior",
			workerResponses:  []string{"", "sess-recovered"},
			wantFinalSessID:  "sess-recovered",
			// Attempt 1: resume from checkpoint. Attempt 2: still resume from
			// checkpoint because worker returned empty (no update to phase.SessionID).
			wantResumeOnAttempt: []string{"sess-prior", "sess-prior"},
		},
		{
			name:                "three attempts, IDs chain correctly",
			checkpointSessID:    "",
			workerResponses:     []string{"sess-001", "sess-002", "sess-003"},
			wantFinalSessID:     "sess-003",
			wantResumeOnAttempt: []string{"", "sess-001", "sess-002"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			phase := &core.Phase{SessionID: tt.checkpointSessID}

			for attempt, workerSessionID := range tt.workerResponses {
				config := &core.WorkerConfig{}

				// Step 1: engine sets resume ID from phase.SessionID before each attempt.
				setResumeSessionID(config, phase)

				// Verify the resume ID matches the expected value for this attempt.
				wantResume := tt.wantResumeOnAttempt[attempt]
				if config.ResumeSessionID != wantResume {
					t.Errorf("attempt %d: config.ResumeSessionID = %q; want %q",
						attempt+1, config.ResumeSessionID, wantResume)
				}

				// Step 2 (simulated): worker runs and returns a session ID.
				// Step 3: engine updates phase.SessionID if non-empty.
				updatePhaseSessionID(phase, workerSessionID)
			}

			if phase.SessionID != tt.wantFinalSessID {
				t.Errorf("final phase.SessionID = %q; want %q",
					phase.SessionID, tt.wantFinalSessID)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// learningsLimitForPhase: tier-based limit selection
// ---------------------------------------------------------------------------

func TestLearningsLimitForPhase(t *testing.T) {
	tests := []struct {
		name      string
		phaseName string
		want      int
	}{
		{"research phase → 5", "research-topic", 5},
		{"write phase → 5", "write-article", 5},
		{"Research capitalised → 5", "Research Phase", 5},
		{"Write capitalised → 5", "Write Draft", 5},
		{"image phase → 2", "generate-images", 2},
		{"illustrate phase → 2", "illustrate-concepts", 2},
		{"social phase → 2", "social-post", 2},
		{"Image capitalised → 2", "Image Generation", 2},
		{"default phase → 3", "compile-results", 3},
		{"verify phase → 3", "verify", 3},
		{"empty name → 3", "", 3},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			phase := &core.Phase{Name: tt.phaseName}
			got := learningsLimitForPhase(phase)
			if got != tt.want {
				t.Errorf("learningsLimitForPhase(%q) = %d; want %d", tt.phaseName, got, tt.want)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// extractMissionContext: parses key-value pairs from task header
// ---------------------------------------------------------------------------

func TestExtractMissionContext(t *testing.T) {
	tests := []struct {
		name       string
		taskHeader string
		want       string
	}{
		{
			name: "all supported keys",
			taskHeader: `# Mission: Build Article

**Repo**: ~/skills/orchestrator
Target: ~/blog/posts/my-article.mdx
Image target: ~/blog/public/images/my-article
Type: article
Illustration staging: ~/nanika/staging/illustrations

## Changes Required`,
			want: "- **Target** ~/blog/posts/my-article.mdx\n- **Image target** ~/blog/public/images/my-article\n- **Type** article\n- **Illustration staging** ~/nanika/staging/illustrations",
		},
		{
			name:       "no matching keys",
			taskHeader: "# Mission: Simple Task\n\nJust do the thing.\n",
			want:       "",
		},
		{
			name:       "empty input",
			taskHeader: "",
			want:       "",
		},
		{
			name: "partial keys",
			taskHeader: `Target: ~/blog/posts/go-errors.mdx
Type: tutorial`,
			want: "- **Target** ~/blog/posts/go-errors.mdx\n- **Type** tutorial",
		},
		{
			name:       "key with no value is skipped",
			taskHeader: "Target:   \nType: article",
			want:       "- **Type** article",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := extractMissionContext(tt.taskHeader)
			if got != tt.want {
				t.Errorf("extractMissionContext() =\n%q\nwant:\n%q", got, tt.want)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// truncateContent: sentence-boundary cap at maxLen characters
// ---------------------------------------------------------------------------

func TestContentTruncation(t *testing.T) {
	tests := []struct {
		name    string
		content string
		maxLen  int
		want    string
	}{
		{
			name:    "short content unchanged",
			content: "Short content.",
			maxLen:  500,
			want:    "Short content.",
		},
		{
			name:    "exact length unchanged",
			content: "Exactly five.",
			maxLen:  13,
			want:    "Exactly five.",
		},
		{
			name:    "sentence boundary truncation with period",
			content: "First sentence. Second sentence that pushes over the limit.",
			maxLen:  20,
			// window[:20] = "First sentence. Seco"; last '. ' at index 15
			// result = content[:15] = "First sentence."
			want: "First sentence.",
		},
		{
			name:    "sentence boundary with exclamation",
			content: "Watch out! This part is too long for the limit.",
			maxLen:  15,
			// window[:15] = "Watch out! This"; last '! ' at index 10
			// result = content[:10] = "Watch out!"
			want: "Watch out!",
		},
		{
			name:    "sentence boundary with question mark",
			content: "Is this right? Yes it is but this makes it too long.",
			maxLen:  20,
			// window[:20] = "Is this right? Yes i"; last '? ' at index 14
			// result = content[:14] = "Is this right?"
			want: "Is this right?",
		},
		{
			name:    "no sentence boundary: hard truncation with ellipsis",
			content: "abcdefghijklmnopqrstuvwxyz",
			maxLen:  10,
			// no '. ', '! ', '? ' in window → window + "..."
			want: "abcdefghij...",
		},
		{
			name:    "empty string unchanged",
			content: "",
			maxLen:  500,
			want:    "",
		},
		{
			name:    "multiple sentence ends: uses last one within window",
			content: "One. Two. Three. This part should be cut at limit.",
			maxLen:  20,
			// window[:20] = "One. Two. Three. Thi"; last '. ' at index 16
			// result = content[:16] = "One. Two. Three."
			want: "One. Two. Three.",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := truncateContent(tt.content, tt.maxLen)
			if got != tt.want {
				t.Errorf("truncateContent(%q, %d) = %q; want %q", tt.content, tt.maxLen, got, tt.want)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// DisableLearnings: no learnings injected when flag is set
// ---------------------------------------------------------------------------

// shouldInjectLearnings replicates the guard in executePhase:
//
//	if e.learningDB != nil && !e.config.DisableLearnings { ... }
//
// Extracted here to pin the contract without spawning real workers.
func shouldInjectLearnings(hasDB bool, disableLearnings bool) bool {
	return hasDB && !disableLearnings
}

func TestDisableLearnings(t *testing.T) {
	tests := []struct {
		name     string
		hasDB    bool
		disabled bool
		want     bool
	}{
		{
			name:     "nil DB: no injection regardless of flag",
			hasDB:    false,
			disabled: false,
			want:     false,
		},
		{
			name:     "nil DB + disabled: no injection",
			hasDB:    false,
			disabled: true,
			want:     false,
		},
		{
			name:     "DB present, flag off: learnings are injected",
			hasDB:    true,
			disabled: false,
			want:     true,
		},
		{
			name:     "DB present, flag on: injection skipped",
			hasDB:    true,
			disabled: true,
			want:     false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := shouldInjectLearnings(tt.hasDB, tt.disabled)
			if got != tt.want {
				t.Errorf("shouldInjectLearnings(hasDB=%v, disabled=%v) = %v; want %v",
					tt.hasDB, tt.disabled, got, tt.want)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// Checkpoint round-trip: SaveCheckpoint preserves SessionID in Phase
// ---------------------------------------------------------------------------

func TestCheckpointPreservesSessionID(t *testing.T) {
	tests := []struct {
		name      string
		sessionID string
	}{
		{"non-empty session ID round-trips", "sess-abc123"},
		{"empty session ID round-trips as empty", ""},
		{"UUID format round-trips", "550e8400-e29b-41d4-a716-446655440000"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tmpDir := t.TempDir()

			plan := &core.Plan{
				ID:   "plan-test",
				Task: "test task",
				Phases: []*core.Phase{
					{
						ID:        "phase-1",
						Name:      "Test phase",
						SessionID: tt.sessionID,
					},
				},
			}

			if err := core.SaveCheckpoint(tmpDir, plan, "dev", "in_progress", time.Time{}); err != nil {
				t.Fatalf("SaveCheckpoint failed: %v", err)
			}

			loaded, err := core.LoadCheckpoint(tmpDir)
			if err != nil {
				t.Fatalf("LoadCheckpoint failed: %v", err)
			}

			if len(loaded.Plan.Phases) != 1 {
				t.Fatalf("want 1 phase, got %d", len(loaded.Plan.Phases))
			}

			got := loaded.Plan.Phases[0].SessionID
			if got != tt.sessionID {
				t.Errorf("SessionID = %q; want %q", got, tt.sessionID)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// handleArtifactMerge: warning events on CreatePhaseArtifactDir / MergeArtifacts errors
// ---------------------------------------------------------------------------

// captureEmitter records every event emitted during a test.
type captureEmitter struct {
	mu     sync.Mutex
	events []event.Event
}

func (c *captureEmitter) Emit(_ context.Context, ev event.Event) {
	c.mu.Lock()
	c.events = append(c.events, ev)
	c.mu.Unlock()
}

func (c *captureEmitter) Close() error { return nil }

func (c *captureEmitter) collected() []event.Event {
	c.mu.Lock()
	defer c.mu.Unlock()
	out := make([]event.Event, len(c.events))
	copy(out, c.events)
	return out
}

func TestHandleArtifactMerge_DirCreateError(t *testing.T) {
	wsDir := t.TempDir()
	// Make the artifacts directory unwritable so CreatePhaseArtifactDir fails.
	artifactsDir := filepath.Join(wsDir, "artifacts")
	if err := os.MkdirAll(artifactsDir, 0500); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.Chmod(artifactsDir, 0700) })

	em := &captureEmitter{}
	eng := &Engine{
		phases:    make(map[string]*core.Phase),
		emitter:   em,
		workspace: &core.Workspace{ID: "test-ws", Path: wsDir},
		config:    &core.OrchestratorConfig{},
	}

	phase := &core.Phase{ID: "p1", Name: "phase-one"}
	config := &core.WorkerConfig{Name: "phase-one", WorkerDir: t.TempDir()}

	eng.handleArtifactMerge(context.Background(), phase, config)

	evts := em.collected()
	if len(evts) == 0 {
		t.Fatal("expected a SystemError warning event, got none")
	}
	found := false
	for _, ev := range evts {
		if ev.Type == event.SystemError {
			if w, ok := ev.Data["warning"].(bool); ok && w {
				if msg, ok := ev.Data["error"].(string); ok && strings.Contains(msg, "creating artifact dir") {
					found = true
				}
			}
		}
	}
	if !found {
		t.Errorf("no SystemError warning event for artifact dir creation failure; events: %+v", evts)
	}
}

func TestHandleArtifactMerge_MergeError(t *testing.T) {
	wsDir := t.TempDir()
	workerDir := t.TempDir()

	// Put an unreadable artifact in the worker dir so MergeArtifacts fails.
	srcFile := filepath.Join(workerDir, "report.md")
	if err := os.WriteFile(srcFile, []byte("data"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(srcFile, 0000); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.Chmod(srcFile, 0600) })

	em := &captureEmitter{}
	eng := &Engine{
		phases:    make(map[string]*core.Phase),
		emitter:   em,
		workspace: &core.Workspace{ID: "test-ws", Path: wsDir},
		config:    &core.OrchestratorConfig{},
	}

	phase := &core.Phase{ID: "p2", Name: "phase-two"}
	config := &core.WorkerConfig{Name: "phase-two", WorkerDir: workerDir}

	eng.handleArtifactMerge(context.Background(), phase, config)

	evts := em.collected()
	if len(evts) == 0 {
		t.Fatal("expected a SystemError warning event, got none")
	}
	found := false
	for _, ev := range evts {
		if ev.Type == event.SystemError {
			if w, ok := ev.Data["warning"].(bool); ok && w {
				if msg, ok := ev.Data["error"].(string); ok && strings.Contains(msg, "merging artifacts") {
					found = true
				}
			}
		}
	}
	if !found {
		t.Errorf("no SystemError warning event for merge failure; events: %+v", evts)
	}
}

func TestHandleArtifactMerge_NoErrorsNoEvents(t *testing.T) {
	wsDir := t.TempDir()
	workerDir := t.TempDir()

	if err := os.WriteFile(filepath.Join(workerDir, "ok.md"), []byte("hello"), 0600); err != nil {
		t.Fatal(err)
	}

	em := &captureEmitter{}
	eng := &Engine{
		phases:    make(map[string]*core.Phase),
		emitter:   em,
		workspace: &core.Workspace{ID: "test-ws", Path: wsDir},
		config:    &core.OrchestratorConfig{},
	}

	phase := &core.Phase{ID: "p3", Name: "phase-three"}
	config := &core.WorkerConfig{Name: "phase-three", WorkerDir: workerDir}

	eng.handleArtifactMerge(context.Background(), phase, config)

	for _, ev := range em.collected() {
		if ev.Type == event.SystemError {
			t.Errorf("unexpected SystemError event on success: %+v", ev)
		}
	}
}

// ---------------------------------------------------------------------------
// 6. TargetDir inheritance: workspace.TargetDir propagates to phases without one
// ---------------------------------------------------------------------------

// applyTargetDirInheritance replicates the inheritance rule in executePhase:
//
//	if phase.TargetDir == "" && e.workspace.TargetDir != "" {
//	    phase.TargetDir = e.workspace.TargetDir
//	}
func applyTargetDirInheritance(phase *core.Phase, workspace *core.Workspace) {
	if phase.TargetDir == "" && workspace.TargetDir != "" {
		phase.TargetDir = workspace.TargetDir
	}
}

func TestTargetDirInheritanceTable(t *testing.T) {
	const wsTarget = "/Users/joey/skills/orchestrator"
	const phaseTarget = "/Users/joey/other-repo"

	tests := []struct {
		name            string
		workspaceTarget string
		phaseTarget     string
		wantTarget      string
	}{
		{
			name:            "workspace target inherited when phase has none",
			workspaceTarget: wsTarget,
			phaseTarget:     "",
			wantTarget:      wsTarget,
		},
		{
			name:            "phase explicit target preserved when workspace also has one",
			workspaceTarget: wsTarget,
			phaseTarget:     phaseTarget,
			wantTarget:      phaseTarget,
		},
		{
			name:            "neither set: phase stays empty",
			workspaceTarget: "",
			phaseTarget:     "",
			wantTarget:      "",
		},
		{
			name:            "workspace empty: phase explicit target preserved",
			workspaceTarget: "",
			phaseTarget:     phaseTarget,
			wantTarget:      phaseTarget,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			phase := &core.Phase{TargetDir: tt.phaseTarget}
			workspace := &core.Workspace{TargetDir: tt.workspaceTarget}

			applyTargetDirInheritance(phase, workspace)

			if phase.TargetDir != tt.wantTarget {
				t.Errorf("phase.TargetDir = %q; want %q", phase.TargetDir, tt.wantTarget)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// skipDependents: terminal-state guard
//
// Regression test for the bug where skipDependents would overwrite a phase
// that was already in a terminal state (completed, failed, or skipped from a
// prior checkpoint load). After the fix, only StatusPending phases are
// transitioned to StatusSkipped.
// ---------------------------------------------------------------------------

func TestSkipDependents_TerminalPhasesNotOverwritten(t *testing.T) {
	tests := []struct {
		name         string
		phases       []*core.Phase
		failedID     string
		pendingIDs   []string
		wantStatuses map[string]core.PhaseStatus
	}{
		{
			name: "already-completed dep target is not overwritten",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusFailed},
				{ID: "b", Name: "B", Status: core.StatusCompleted, Dependencies: []string{"a"}},
			},
			failedID:   "a",
			pendingIDs: []string{"b"},
			wantStatuses: map[string]core.PhaseStatus{
				"b": core.StatusCompleted, // must stay completed
			},
		},
		{
			name: "already-failed dep target is not overwritten",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusFailed},
				{ID: "b", Name: "B", Status: core.StatusFailed, Dependencies: []string{"a"}},
			},
			failedID:   "a",
			pendingIDs: []string{"b"},
			wantStatuses: map[string]core.PhaseStatus{
				"b": core.StatusFailed, // must stay failed (don't relabel as skipped)
			},
		},
		{
			name: "already-skipped dep target is not double-counted",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusFailed},
				{ID: "b", Name: "B", Status: core.StatusSkipped, Dependencies: []string{"a"}},
			},
			failedID:   "a",
			pendingIDs: []string{"b"},
			wantStatuses: map[string]core.PhaseStatus{
				"b": core.StatusSkipped, // still skipped, not changed
			},
		},
		{
			name: "pending phase is correctly skipped; terminal sibling is untouched",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusFailed},
				{ID: "b", Name: "B", Status: core.StatusPending, Dependencies: []string{"a"}},
				{ID: "c", Name: "C", Status: core.StatusCompleted, Dependencies: []string{"a"}},
			},
			failedID:   "a",
			pendingIDs: []string{"b", "c"},
			wantStatuses: map[string]core.PhaseStatus{
				"b": core.StatusSkipped,   // pending → skipped
				"c": core.StatusCompleted, // terminal → unchanged
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			eng := &Engine{
				phases:    make(map[string]*core.Phase),
				emitter:   event.NoOpEmitter{},
				workspace: &core.Workspace{ID: "test"},
			}
			for _, p := range tt.phases {
				eng.phases[p.ID] = p
			}
			pending := make(map[string]bool)
			for _, id := range tt.pendingIDs {
				pending[id] = true
			}

			eng.skipDependents(context.Background(), tt.failedID, pending)

			for id, want := range tt.wantStatuses {
				got := eng.phases[id].Status
				if got != want {
					t.Errorf("phase %s: status = %q; want %q", id, got, want)
				}
			}
		})
	}
}

// ---------------------------------------------------------------------------
// executeSequential: IsTerminal guard
//
// Verifies that executeSequential skips phases in any terminal state —
// not only StatusSkipped but also StatusCompleted and StatusFailed (which
// appear when resuming from a checkpoint).
// ---------------------------------------------------------------------------

// isTerminalGuard replicates the guard condition in executeSequential so we
// can test the exact predicate without running a full sequential execution.
func isTerminalGuard(status core.PhaseStatus) bool {
	return status.IsTerminal()
}

func TestExecuteSequentialTerminalGuardTable(t *testing.T) {
	tests := []struct {
		name       string
		status     core.PhaseStatus
		shouldSkip bool
	}{
		{"pending: not skipped", core.StatusPending, false},
		{"running: not skipped (mid-execution snapshot)", core.StatusRunning, false},
		{"completed: skipped (checkpoint resume)", core.StatusCompleted, true},
		{"failed: skipped (checkpoint resume)", core.StatusFailed, true},
		{"skipped: skipped (pre-marked)", core.StatusSkipped, true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := isTerminalGuard(tt.status)
			if got != tt.shouldSkip {
				t.Errorf("isTerminalGuard(%q) = %v; want %v", tt.status, got, tt.shouldSkip)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// Prior context pipeline: output.md artifact read
//
// Verifies that executePhase prefers the worker's output.md artifact as the
// prior context payload over raw SDK output. The helper replicates the read
// logic so it can be tested without spawning real workers.
// ---------------------------------------------------------------------------

// readOutputArtifact replicates the artifact-read logic at the end of executePhase:
// returns output.md content from workerDir when it exists, otherwise "".
func readOutputArtifact(workerDir string) string {
	if data, err := os.ReadFile(filepath.Join(workerDir, "output.md")); err == nil && len(data) > 0 {
		return string(data)
	}
	return ""
}

func TestReadOutputArtifact(t *testing.T) {
	tests := []struct {
		name     string
		write    string // content written to output.md ("" = don't write)
		wantOut  string // expected return value from readOutputArtifact
	}{
		{
			name:    "output.md present: returns its content",
			write:   "# Phase Output\n\nFindings here.",
			wantOut: "# Phase Output\n\nFindings here.",
		},
		{
			name:    "output.md absent: returns empty string",
			write:   "",
			wantOut: "",
		},
		{
			name:    "output.md empty file: returns empty string (falls through to raw output)",
			write:   "\x00", // sentinel: write an empty file
			wantOut: "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dir := t.TempDir()
			if tt.write != "" && tt.write != "\x00" {
				if err := os.WriteFile(filepath.Join(dir, "output.md"), []byte(tt.write), 0600); err != nil {
					t.Fatalf("write output.md: %v", err)
				}
			} else if tt.write == "\x00" {
				if err := os.WriteFile(filepath.Join(dir, "output.md"), []byte(""), 0600); err != nil {
					t.Fatalf("write empty output.md: %v", err)
				}
			}

			got := readOutputArtifact(dir)
			if got != tt.wantOut {
				t.Errorf("readOutputArtifact() = %q; want %q", got, tt.wantOut)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// Sequential prior context: dependency filtering
//
// Verifies that executeSequential passes only declared-dependency outputs as
// prior context, not the full accumulated output history.
// ---------------------------------------------------------------------------

// sequentialPriorContext replicates the dependency-filtered prior context build
// in executeSequential: only outputs from phase.Dependencies are included.
func sequentialPriorContext(phase *core.Phase, phaseOutputs map[string]string) string {
	var parts []string
	for _, depID := range phase.Dependencies {
		if out, ok := phaseOutputs[depID]; ok {
			parts = append(parts, out)
		}
	}
	return strings.Join(parts, "\n\n---\n\n")
}

func TestSequentialPriorContextFiltersByDependencies(t *testing.T) {
	tests := []struct {
		name         string
		phase        *core.Phase
		phaseOutputs map[string]string
		wantContext  string
	}{
		{
			name:         "no dependencies: empty prior context",
			phase:        &core.Phase{ID: "a", Dependencies: nil},
			phaseOutputs: map[string]string{"prev": "some output"},
			wantContext:  "",
		},
		{
			name:         "single dependency: only that output included",
			phase:        &core.Phase{ID: "b", Dependencies: []string{"a"}},
			phaseOutputs: map[string]string{"a": "output-a", "other": "output-other"},
			wantContext:  "output-a",
		},
		{
			name:  "two dependencies: both outputs joined with separator",
			phase: &core.Phase{ID: "c", Dependencies: []string{"a", "b"}},
			phaseOutputs: map[string]string{
				"a":     "output-a",
				"b":     "output-b",
				"other": "output-other",
			},
			wantContext: "output-a\n\n---\n\noutput-b",
		},
		{
			name:         "dependency output missing: silently omitted",
			phase:        &core.Phase{ID: "d", Dependencies: []string{"missing", "a"}},
			phaseOutputs: map[string]string{"a": "output-a"},
			wantContext:  "output-a",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := sequentialPriorContext(tt.phase, tt.phaseOutputs)
			if got != tt.wantContext {
				t.Errorf("sequentialPriorContext() = %q; want %q", got, tt.wantContext)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// Prior context flows correctly between dependent phases (integration)
//
// Verifies that executeSequential:
//  1. Passes artifact content (executor output) as PriorContext, not raw SDK transcript
//  2. Filters prior context by declared dependencies only
// ---------------------------------------------------------------------------

// contextCapturingExecutor records per-phase configs and returns pre-configured
// outputs keyed by phase name.
type contextCapturingExecutor struct {
	mu      sync.Mutex
	outputs map[string]string            // phase name → output to return
	configs map[string]*core.WorkerConfig // phase name → captured config
}

func (c *contextCapturingExecutor) Execute(_ context.Context, config *core.WorkerConfig, _ event.Emitter, _ bool) (string, string, *sdk.CostInfo, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.configs[config.Name] = config
	// Derive phase name from worker name (format: "persona-phaseID").
	// Use the pre-configured output map keyed by the full worker name.
	if out, ok := c.outputs[config.Name]; ok {
		return out, "sess-ctx-test", nil, nil
	}
	return "default-output", "sess-ctx-test", nil, nil
}

func TestExecuteSequential_PriorContextFlowsBetweenDependentPhases(t *testing.T) {
	wsPath := t.TempDir()
	ws := &core.Workspace{ID: "ws-prior-ctx", Path: wsPath, Domain: "dev"}

	exec := &contextCapturingExecutor{
		outputs: map[string]string{
			"researcher-phase-1": "Found 3 key patterns:\n1. Error wrapping with %w\n2. Sentinel errors\n3. Custom error types",
			"writer-phase-2":     "Article draft completed.",
			"independent-phase-3": "Independent result.",
		},
		configs: make(map[string]*core.WorkerConfig),
	}

	e := New(ws, &core.OrchestratorConfig{ForceSequential: true}, nil, nil)
	e.RegisterExecutor(core.RuntimeClaude, exec)

	plan := &core.Plan{
		ID:            "plan-prior-ctx",
		Task:          "test prior context flow",
		ExecutionMode: "sequential",
		Phases: []*core.Phase{
			{
				ID:        "phase-1",
				Name:      "research",
				Objective: "Research error handling",
				Persona:   "researcher",
				ModelTier: "work",
				Status:    core.StatusPending,
			},
			{
				ID:           "phase-2",
				Name:         "write",
				Objective:    "Write article from research",
				Persona:      "writer",
				ModelTier:    "work",
				Dependencies: []string{"phase-1"},
				Status:       core.StatusPending,
			},
			{
				ID:        "phase-3",
				Name:      "independent",
				Objective: "Do something unrelated",
				Persona:   "independent",
				ModelTier: "work",
				Status:    core.StatusPending,
			},
		},
	}

	result, err := e.Execute(context.Background(), plan)
	if err != nil {
		t.Fatalf("Execute() error: %v", err)
	}
	if !result.Success {
		t.Fatalf("Execute() failed: %s", result.Error)
	}

	// Phase-1 (no deps): PriorContext must be empty.
	cfg1 := exec.configs["researcher-phase-1"]
	if cfg1 == nil {
		t.Fatal("phase-1 config was not captured")
	}
	if cfg1.Bundle.PriorContext != "" {
		t.Errorf("phase-1 PriorContext = %q; want empty (no dependencies)", cfg1.Bundle.PriorContext)
	}

	// Phase-2 (depends on phase-1): PriorContext must contain phase-1's
	// artifact output — the clean text returned by the executor, not raw
	// SDK/JSON transcript.
	cfg2 := exec.configs["writer-phase-2"]
	if cfg2 == nil {
		t.Fatal("phase-2 config was not captured")
	}
	wantPrior := "Found 3 key patterns:\n1. Error wrapping with %w\n2. Sentinel errors\n3. Custom error types"
	if cfg2.Bundle.PriorContext != wantPrior {
		t.Errorf("phase-2 PriorContext = %q; want %q", cfg2.Bundle.PriorContext, wantPrior)
	}
	// Must not contain JSON or SDK transcript markers.
	for _, marker := range []string{`"type":"assistant"`, `"type":"result"`, `session_id`} {
		if strings.Contains(cfg2.Bundle.PriorContext, marker) {
			t.Errorf("phase-2 PriorContext contains raw SDK transcript marker %q", marker)
		}
	}

	// Phase-3 (no deps): PriorContext must be empty — dependency filtering
	// ensures independent phases don't receive other phases' output.
	cfg3 := exec.configs["independent-phase-3"]
	if cfg3 == nil {
		t.Fatal("phase-3 config was not captured")
	}
	if cfg3.Bundle.PriorContext != "" {
		t.Errorf("phase-3 PriorContext = %q; want empty (no dependencies)", cfg3.Bundle.PriorContext)
	}
}

// ---------------------------------------------------------------------------
// OBJECTIVE length guard: phase fails when Objective exceeds 8192 bytes
// ---------------------------------------------------------------------------

// TestObjectiveLengthGuard verifies that executePhase rejects phases whose
// Objective field exceeds maxObjectiveBytes (8192). The phase should fail
// without invoking the executor, and Execute should report failure.
func TestObjectiveLengthGuard(t *testing.T) {
	tests := []struct {
		name      string
		objective string
		wantFail  bool
	}{
		{
			name:      "exactly at limit passes",
			objective: strings.Repeat("x", maxObjectiveBytes),
			wantFail:  false,
		},
		{
			name:      "one byte over limit fails",
			objective: strings.Repeat("x", maxObjectiveBytes+1),
			wantFail:  true,
		},
		{
			name:      "far over limit fails",
			objective: strings.Repeat("x", maxObjectiveBytes*2),
			wantFail:  true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ws := &core.Workspace{ID: "ws-obj-guard", Path: t.TempDir(), Domain: "dev"}
			exec := instantExecutor{fail: false}
			e := New(ws, &core.OrchestratorConfig{ForceSequential: true}, nil, nil).
				WithEmitter(event.NoOpEmitter{})
			e.RegisterExecutor(core.RuntimeClaude, exec)

			plan := &core.Plan{
				ID:            "plan-obj-guard",
				Task:          "test objective guard",
				ExecutionMode: "sequential",
				Phases: []*core.Phase{
					{
						ID:        "phase-1",
						Name:      "guarded-phase",
						Objective: tt.objective,
						Persona:   "senior-backend-engineer",
						ModelTier: "work",
						Status:    core.StatusPending,
					},
				},
			}

			result, err := e.Execute(context.Background(), plan)
			if err != nil {
				t.Fatalf("Execute() returned unexpected error: %v", err)
			}
			if tt.wantFail && result.Success {
				t.Error("Execute() succeeded; want failure for oversized objective")
			}
			if !tt.wantFail && !result.Success {
				t.Errorf("Execute() failed; want success for objective at or below limit: %s", result.Error)
			}
		})
	}
}
