package engine

import (
	"context"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

func TestProcessSignal(t *testing.T) {
	tests := []struct {
		name            string
		signal          core.CompletionSignal
		inputOutput     string
		wantOutput      string  // substring that must appear in returned output
		wantErr         bool
		wantErrContains string
		wantRemainder   string            // expected phase.SignalRemainder
		wantEventType   event.EventType   // expected emitted event type (empty = no event expected)
		wantEventData   map[string]string // key-value pairs to check in event data
	}{
		{
			name:        "ok signal is a no-op",
			signal:      core.CompletionSignal{Kind: core.SignalOK},
			inputOutput: "phase output",
			wantOutput:  "phase output",
		},
		{
			name: "partial stores remainder and appends to output",
			signal: core.CompletionSignal{
				Kind:      core.SignalPartial,
				Summary:   "completed 3 of 5 items",
				Remainder: "items 4 and 5 still need implementation",
			},
			inputOutput:   "implemented items 1-3",
			wantOutput:    "## Remaining Work",
			wantRemainder: "items 4 and 5 still need implementation",
		},
		{
			name: "partial with empty remainder stores empty and does not append",
			signal: core.CompletionSignal{
				Kind:    core.SignalPartial,
				Summary: "partially done",
			},
			inputOutput:   "some work",
			wantOutput:    "some work",
			wantRemainder: "",
		},
		{
			name: "dependency_missing fails phase with descriptive error",
			signal: core.CompletionSignal{
				Kind:         core.SignalDependencyMissing,
				Summary:      "API schema not found",
				MissingInput: []string{"api_schema.json", "endpoint_list.md"},
			},
			inputOutput:     "attempted work",
			wantErr:         true,
			wantErrContains: "api_schema.json, endpoint_list.md",
			wantEventType:   event.PhaseFailed,
		},
		{
			name: "scope_expansion emits event and continues",
			signal: core.CompletionSignal{
				Kind:    core.SignalScopeExpansion,
				Summary: "discovered 3 additional services need migration",
				SuggestedPhases: []core.PhaseDraft{
					{Name: "migrate-svc-b", Objective: "Migrate service B"},
				},
			},
			inputOutput:   "migrated service A",
			wantOutput:    "migrated service A",
			wantEventType: event.SignalScopeExpansion,
			wantEventData: map[string]string{
				"summary": "discovered 3 additional services need migration",
			},
		},
		{
			name: "replan_required emits event and continues",
			signal: core.CompletionSignal{
				Kind:    core.SignalReplanRequired,
				Summary: "architecture incompatible with proposed approach",
			},
			inputOutput:   "research output",
			wantOutput:    "research output",
			wantEventType: event.SignalReplanRequired,
			wantEventData: map[string]string{
				"summary": "architecture incompatible with proposed approach",
			},
		},
		{
			name: "human_decision_needed emits event and continues",
			signal: core.CompletionSignal{
				Kind:    core.SignalHumanDecisionNeeded,
				Summary: "choose between PostgreSQL and SQLite for the data layer",
			},
			inputOutput:   "analysis of both options",
			wantOutput:    "analysis of both options",
			wantEventType: event.SignalHumanDecisionNeeded,
			wantEventData: map[string]string{
				"summary": "choose between PostgreSQL and SQLite for the data layer",
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			em := &captureEmitter{}
			eng := &Engine{
				phases:    make(map[string]*core.Phase),
				emitter:   em,
				workspace: &core.Workspace{ID: "test"},
				config:    &core.OrchestratorConfig{},
			}
			phase := &core.Phase{ID: "p1", Name: "test-phase"}

			got, err := eng.processSignal(context.Background(), tt.signal, phase, "p1", "worker-1", tt.inputOutput)

			// Check error
			if tt.wantErr {
				if err == nil {
					t.Fatal("expected error, got nil")
				}
				if tt.wantErrContains != "" {
					if !containsStr(err.Error(), tt.wantErrContains) {
						t.Errorf("error = %q, want containing %q", err.Error(), tt.wantErrContains)
					}
				}
			} else {
				if err != nil {
					t.Fatalf("unexpected error: %v", err)
				}
			}

			// Check output
			if !containsStr(got, tt.wantOutput) {
				t.Errorf("output = %q, want containing %q", got, tt.wantOutput)
			}

			// Check remainder
			if tt.wantRemainder != "" && phase.SignalRemainder != tt.wantRemainder {
				t.Errorf("SignalRemainder = %q, want %q", phase.SignalRemainder, tt.wantRemainder)
			}

			// Check events
			evts := em.collected()
			if tt.wantEventType != "" {
				found := false
				for _, ev := range evts {
					if ev.Type == tt.wantEventType {
						found = true
						for k, wantV := range tt.wantEventData {
							if gotV, ok := ev.Data[k]; !ok || gotV != wantV {
								t.Errorf("event data[%q] = %v, want %q", k, gotV, wantV)
							}
						}
						break
					}
				}
				if !found {
					t.Errorf("expected event %s not emitted (got %d events)", tt.wantEventType, len(evts))
				}
			}
		})
	}
}

// TestProcessSignal_DependencyMissingNoInputs verifies the error message when
// MissingInput is empty (edge case: worker signals dependency_missing but
// doesn't specify what's missing).
func TestProcessSignal_DependencyMissingNoInputs(t *testing.T) {
	em := &captureEmitter{}
	eng := &Engine{
		phases:    make(map[string]*core.Phase),
		emitter:   em,
		workspace: &core.Workspace{ID: "test"},
		config:    &core.OrchestratorConfig{},
	}
	phase := &core.Phase{ID: "p1", Name: "test-phase"}

	sig := core.CompletionSignal{
		Kind:    core.SignalDependencyMissing,
		Summary: "missing something",
	}

	_, err := eng.processSignal(context.Background(), sig, phase, "p1", "w1", "output")
	if err == nil {
		t.Fatal("expected error for dependency_missing signal")
	}
	if !containsStr(err.Error(), "dependency missing") {
		t.Errorf("error = %q, want containing 'dependency missing'", err.Error())
	}
}

// TestProcessSignal_ScopeExpansionWithSuggestedPhases verifies suggested phases
// are included in the event data.
func TestProcessSignal_ScopeExpansionWithSuggestedPhases(t *testing.T) {
	em := &captureEmitter{}
	eng := &Engine{
		phases:    make(map[string]*core.Phase),
		emitter:   em,
		workspace: &core.Workspace{ID: "test"},
		config:    &core.OrchestratorConfig{},
	}
	phase := &core.Phase{ID: "p1", Name: "test-phase"}

	drafts := []core.PhaseDraft{
		{Name: "extra-1", Objective: "Do extra work"},
		{Name: "extra-2", Objective: "Do more work", Persona: "researcher"},
	}
	sig := core.CompletionSignal{
		Kind:            core.SignalScopeExpansion,
		Summary:         "needs more work",
		SuggestedPhases: drafts,
	}

	_, err := eng.processSignal(context.Background(), sig, phase, "p1", "w1", "output")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	evts := em.collected()
	found := false
	for _, ev := range evts {
		if ev.Type == event.SignalScopeExpansion {
			found = true
			sp, ok := ev.Data["suggested_phases"]
			if !ok {
				t.Error("event data missing suggested_phases")
			}
			if phases, ok := sp.([]core.PhaseDraft); ok && len(phases) != 2 {
				t.Errorf("suggested_phases length = %d, want 2", len(phases))
			}
		}
	}
	if !found {
		t.Error("SignalScopeExpansion event not emitted")
	}
}

func containsStr(s, substr string) bool {
	return len(substr) == 0 || len(s) >= len(substr) && searchStr(s, substr)
}

func searchStr(s, substr string) bool {
	for i := 0; i <= len(s)-len(substr); i++ {
		if s[i:i+len(substr)] == substr {
			return true
		}
	}
	return false
}
