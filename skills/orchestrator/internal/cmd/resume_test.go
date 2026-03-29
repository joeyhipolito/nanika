package cmd

// Regression tests for resetPhasesForResume.
//
// The bug: resumeMission only reset running/failed phases to pending; skipped
// phases (which were skipped because their upstream failed) stayed skipped
// forever and were never re-dispatched on resume.
//
// The fix: resetPhasesForResume also revives transitive skipped descendants of
// any phase being reset to pending.

import (
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
)

func TestResetPhasesForResume(t *testing.T) {
	tests := []struct {
		name         string
		phases       []*core.Phase
		wantStatuses map[string]core.PhaseStatus
	}{
		{
			name: "failed phase reset to pending",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusFailed, Error: "boom"},
			},
			wantStatuses: map[string]core.PhaseStatus{
				"a": core.StatusPending,
			},
		},
		{
			name: "running phase reset to pending",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusRunning},
			},
			wantStatuses: map[string]core.PhaseStatus{
				"a": core.StatusPending,
			},
		},
		{
			name: "completed phase untouched",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusCompleted},
			},
			wantStatuses: map[string]core.PhaseStatus{
				"a": core.StatusCompleted,
			},
		},
		{
			// Regression: A fails, B is skipped as its direct dependent.
			// On resume, both A and B must be pending so B runs again.
			name: "skipped direct dependent is revived",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusFailed, Error: "err"},
				{ID: "b", Name: "B", Status: core.StatusSkipped, Dependencies: []string{"a"}},
			},
			wantStatuses: map[string]core.PhaseStatus{
				"a": core.StatusPending,
				"b": core.StatusPending,
			},
		},
		{
			// Regression: A fails, B is skipped (dep on A), C is skipped (dep on B).
			// On resume, A, B, and C must all be pending.
			name: "transitive skipped descendants revived (A->B->C)",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusFailed, Error: "err"},
				{ID: "b", Name: "B", Status: core.StatusSkipped, Dependencies: []string{"a"}},
				{ID: "c", Name: "C", Status: core.StatusSkipped, Dependencies: []string{"b"}},
			},
			wantStatuses: map[string]core.PhaseStatus{
				"a": core.StatusPending,
				"b": core.StatusPending,
				"c": core.StatusPending,
			},
		},
		{
			// Diamond: A fails, B and C depend on A (skipped), D depends on B+C (skipped).
			// All four must become pending on resume.
			name: "diamond DAG: all skipped descendants revived",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusFailed, Error: "err"},
				{ID: "b", Name: "B", Status: core.StatusSkipped, Dependencies: []string{"a"}},
				{ID: "c", Name: "C", Status: core.StatusSkipped, Dependencies: []string{"a"}},
				{ID: "d", Name: "D", Status: core.StatusSkipped, Dependencies: []string{"b", "c"}},
			},
			wantStatuses: map[string]core.PhaseStatus{
				"a": core.StatusPending,
				"b": core.StatusPending,
				"c": core.StatusPending,
				"d": core.StatusPending,
			},
		},
		{
			// Independent phase X has no dependency on A; its skipped status
			// must not be touched by the revival of A's descendants.
			name: "independent skipped phase is not revived",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusFailed, Error: "err"},
				{ID: "b", Name: "B", Status: core.StatusSkipped, Dependencies: []string{"a"}},
				{ID: "x", Name: "X", Status: core.StatusSkipped}, // independent — stays skipped
			},
			wantStatuses: map[string]core.PhaseStatus{
				"a": core.StatusPending,
				"b": core.StatusPending,
				"x": core.StatusSkipped,
			},
		},
		{
			// Completed phases whose skipped dependents exist should not revive
			// those dependents — only phases being reset (failed/running) trigger revival.
			name: "completed phase does not trigger revival of downstream skipped",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusCompleted},
				{ID: "b", Name: "B", Status: core.StatusSkipped, Dependencies: []string{"a"}},
			},
			wantStatuses: map[string]core.PhaseStatus{
				"a": core.StatusCompleted,
				"b": core.StatusSkipped,
			},
		},
		{
			// Error field is cleared when a failed phase is reset to pending.
			name: "error cleared on reset",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusFailed, Error: "worker died"},
				{ID: "b", Name: "B", Status: core.StatusSkipped, Error: "skipped: dependency a failed", Dependencies: []string{"a"}},
			},
			wantStatuses: map[string]core.PhaseStatus{
				"a": core.StatusPending,
				"b": core.StatusPending,
			},
		},
		{
			// Deep chain: A->B->C->D, A fails, B+C+D are skipped.
			// All must become pending on resume.
			name: "deep chain A->B->C->D all revived",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusFailed, Error: "err"},
				{ID: "b", Name: "B", Status: core.StatusSkipped, Dependencies: []string{"a"}},
				{ID: "c", Name: "C", Status: core.StatusSkipped, Dependencies: []string{"b"}},
				{ID: "d", Name: "D", Status: core.StatusSkipped, Dependencies: []string{"c"}},
			},
			wantStatuses: map[string]core.PhaseStatus{
				"a": core.StatusPending,
				"b": core.StatusPending,
				"c": core.StatusPending,
				"d": core.StatusPending,
			},
		},
		{
			// Mixed: A completed, B failed, C skipped (dep B), D completed.
			// Only B and C reset; A and D untouched.
			name: "mixed statuses: only failed and its skipped descendants reset",
			phases: []*core.Phase{
				{ID: "a", Name: "A", Status: core.StatusCompleted},
				{ID: "b", Name: "B", Status: core.StatusFailed, Error: "err", Dependencies: []string{"a"}},
				{ID: "c", Name: "C", Status: core.StatusSkipped, Dependencies: []string{"b"}},
				{ID: "d", Name: "D", Status: core.StatusCompleted},
			},
			wantStatuses: map[string]core.PhaseStatus{
				"a": core.StatusCompleted,
				"b": core.StatusPending,
				"c": core.StatusPending,
				"d": core.StatusCompleted,
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			resetPhasesForResume(tt.phases)

			for _, p := range tt.phases {
				want, ok := tt.wantStatuses[p.ID]
				if !ok {
					continue
				}
				if p.Status != want {
					t.Errorf("phase %s: status = %q; want %q", p.ID, p.Status, want)
				}
				// Error must be cleared on reset phases.
				if p.Status == core.StatusPending && p.Error != "" {
					t.Errorf("phase %s: Error = %q; want empty after reset", p.ID, p.Error)
				}
			}
		})
	}
}
