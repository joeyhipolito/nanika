package core

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

// ---------------------------------------------------------------------------
// ReadSignalFile — missing file returns ok
// ---------------------------------------------------------------------------

func TestReadSignalFile_MissingFileReturnsOK(t *testing.T) {
	dir := t.TempDir()

	sig, err := ReadSignalFile(dir)
	if err != nil {
		t.Fatalf("ReadSignalFile: %v", err)
	}
	if sig.Kind != SignalOK {
		t.Errorf("Kind = %q; want %q", sig.Kind, SignalOK)
	}
	if sig.Summary != "" {
		t.Errorf("Summary = %q; want empty", sig.Summary)
	}
	if len(sig.MissingInput) != 0 {
		t.Errorf("MissingInput = %v; want nil", sig.MissingInput)
	}
	if len(sig.SuggestedPhases) != 0 {
		t.Errorf("SuggestedPhases = %v; want nil", sig.SuggestedPhases)
	}
}

// ---------------------------------------------------------------------------
// ReadSignalFile — valid signal files
// ---------------------------------------------------------------------------

func TestReadSignalFile_ValidSignals(t *testing.T) {
	tests := []struct {
		name    string
		signal  CompletionSignal
		checkFn func(t *testing.T, got CompletionSignal)
	}{
		{
			name: "ok signal",
			signal: CompletionSignal{
				Kind:    SignalOK,
				Summary: "all done",
			},
			checkFn: func(t *testing.T, got CompletionSignal) {
				if got.Kind != SignalOK {
					t.Errorf("Kind = %q; want %q", got.Kind, SignalOK)
				}
				if got.Summary != "all done" {
					t.Errorf("Summary = %q; want %q", got.Summary, "all done")
				}
			},
		},
		{
			name: "partial with remainder",
			signal: CompletionSignal{
				Kind:         SignalPartial,
				Summary:      "implemented 3 of 5 endpoints",
				ChangedFiles: []string{"api/handler.go", "api/routes.go"},
				Remainder:    "endpoints /users and /groups still need implementation",
			},
			checkFn: func(t *testing.T, got CompletionSignal) {
				if got.Kind != SignalPartial {
					t.Errorf("Kind = %q; want %q", got.Kind, SignalPartial)
				}
				if len(got.ChangedFiles) != 2 {
					t.Fatalf("ChangedFiles len = %d; want 2", len(got.ChangedFiles))
				}
				if got.ChangedFiles[0] != "api/handler.go" {
					t.Errorf("ChangedFiles[0] = %q; want %q", got.ChangedFiles[0], "api/handler.go")
				}
				if got.Remainder != "endpoints /users and /groups still need implementation" {
					t.Errorf("Remainder = %q", got.Remainder)
				}
			},
		},
		{
			name: "dependency_missing with missing inputs",
			signal: CompletionSignal{
				Kind:         SignalDependencyMissing,
				Summary:      "cannot proceed without API schema",
				MissingInput: []string{"openapi.yaml", "auth token spec"},
			},
			checkFn: func(t *testing.T, got CompletionSignal) {
				if got.Kind != SignalDependencyMissing {
					t.Errorf("Kind = %q; want %q", got.Kind, SignalDependencyMissing)
				}
				if len(got.MissingInput) != 2 {
					t.Fatalf("MissingInput len = %d; want 2", len(got.MissingInput))
				}
				if got.MissingInput[0] != "openapi.yaml" {
					t.Errorf("MissingInput[0] = %q; want %q", got.MissingInput[0], "openapi.yaml")
				}
			},
		},
		{
			name: "scope_expansion with suggested phases",
			signal: CompletionSignal{
				Kind:    SignalScopeExpansion,
				Summary: "discovered additional migration needed",
				SuggestedPhases: []PhaseDraft{
					{
						Name:      "migrate-schema",
						Objective: "add new columns to users table",
						Persona:   "senior-backend-engineer",
						Skills:    []string{"sqlite"},
					},
					{
						Name:         "backfill-data",
						Objective:    "populate new columns from legacy data",
						Persona:      "senior-backend-engineer",
						Dependencies: []string{"migrate-schema"},
					},
				},
			},
			checkFn: func(t *testing.T, got CompletionSignal) {
				if got.Kind != SignalScopeExpansion {
					t.Errorf("Kind = %q; want %q", got.Kind, SignalScopeExpansion)
				}
				if len(got.SuggestedPhases) != 2 {
					t.Fatalf("SuggestedPhases len = %d; want 2", len(got.SuggestedPhases))
				}
				ph := got.SuggestedPhases[0]
				if ph.Name != "migrate-schema" {
					t.Errorf("SuggestedPhases[0].Name = %q", ph.Name)
				}
				if ph.Persona != "senior-backend-engineer" {
					t.Errorf("SuggestedPhases[0].Persona = %q", ph.Persona)
				}
				if len(ph.Skills) != 1 || ph.Skills[0] != "sqlite" {
					t.Errorf("SuggestedPhases[0].Skills = %v", ph.Skills)
				}
				ph2 := got.SuggestedPhases[1]
				if len(ph2.Dependencies) != 1 || ph2.Dependencies[0] != "migrate-schema" {
					t.Errorf("SuggestedPhases[1].Dependencies = %v", ph2.Dependencies)
				}
			},
		},
		{
			name: "replan_required",
			signal: CompletionSignal{
				Kind:    SignalReplanRequired,
				Summary: "original plan assumptions invalidated by API changes",
			},
			checkFn: func(t *testing.T, got CompletionSignal) {
				if got.Kind != SignalReplanRequired {
					t.Errorf("Kind = %q; want %q", got.Kind, SignalReplanRequired)
				}
			},
		},
		{
			name: "human_decision_needed",
			signal: CompletionSignal{
				Kind:    SignalHumanDecisionNeeded,
				Summary: "two valid approaches, need human choice",
			},
			checkFn: func(t *testing.T, got CompletionSignal) {
				if got.Kind != SignalHumanDecisionNeeded {
					t.Errorf("Kind = %q; want %q", got.Kind, SignalHumanDecisionNeeded)
				}
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dir := t.TempDir()
			data, err := json.Marshal(tt.signal)
			if err != nil {
				t.Fatalf("marshal: %v", err)
			}
			if err := os.WriteFile(filepath.Join(dir, SignalFileName), data, 0600); err != nil {
				t.Fatalf("write: %v", err)
			}

			got, err := ReadSignalFile(dir)
			if err != nil {
				t.Fatalf("ReadSignalFile: %v", err)
			}
			tt.checkFn(t, got)
		})
	}
}

// ---------------------------------------------------------------------------
// ReadSignalFile — error cases
// ---------------------------------------------------------------------------

func TestReadSignalFile_CorruptJSON(t *testing.T) {
	tests := []struct {
		name    string
		content string
	}{
		{"empty file", ""},
		{"invalid json", "{not json!!!"},
		{"truncated", `{"kind": "ok", "summary":`},
		{"array instead of object", `[1, 2, 3]`},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dir := t.TempDir()
			if err := os.WriteFile(filepath.Join(dir, SignalFileName), []byte(tt.content), 0600); err != nil {
				t.Fatalf("write: %v", err)
			}

			_, err := ReadSignalFile(dir)
			if err == nil {
				t.Error("expected error for corrupt signal file")
			}
		})
	}
}

func TestReadSignalFile_UnreadableFile(t *testing.T) {
	dir := t.TempDir()
	p := filepath.Join(dir, SignalFileName)
	if err := os.WriteFile(p, []byte(`{"kind":"ok"}`), 0600); err != nil {
		t.Fatalf("write: %v", err)
	}
	if err := os.Chmod(p, 0000); err != nil {
		t.Fatalf("chmod: %v", err)
	}
	t.Cleanup(func() { os.Chmod(p, 0600) })

	_, err := ReadSignalFile(dir)
	if err == nil {
		t.Error("expected error for unreadable signal file")
	}
}

// ---------------------------------------------------------------------------
// JSON round-trip fidelity
// ---------------------------------------------------------------------------

func TestCompletionSignal_JSONRoundTrip(t *testing.T) {
	original := CompletionSignal{
		Kind:         SignalScopeExpansion,
		Summary:      "found extra work",
		MissingInput: []string{"schema.yaml"},
		SuggestedPhases: []PhaseDraft{
			{
				Name:         "extra-phase",
				Objective:    "handle the extra work",
				Persona:      "architect",
				Skills:       []string{"design"},
				Dependencies: []string{"phase-1"},
			},
		},
		ChangedFiles: []string{"main.go", "go.mod"},
		Remainder:    "still need tests",
	}

	data, err := json.Marshal(original)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}

	var restored CompletionSignal
	if err := json.Unmarshal(data, &restored); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}

	if restored.Kind != original.Kind {
		t.Errorf("Kind = %q; want %q", restored.Kind, original.Kind)
	}
	if restored.Summary != original.Summary {
		t.Errorf("Summary = %q; want %q", restored.Summary, original.Summary)
	}
	if len(restored.MissingInput) != len(original.MissingInput) {
		t.Fatalf("MissingInput len = %d; want %d", len(restored.MissingInput), len(original.MissingInput))
	}
	if restored.MissingInput[0] != "schema.yaml" {
		t.Errorf("MissingInput[0] = %q", restored.MissingInput[0])
	}
	if len(restored.SuggestedPhases) != 1 {
		t.Fatalf("SuggestedPhases len = %d; want 1", len(restored.SuggestedPhases))
	}
	ph := restored.SuggestedPhases[0]
	if ph.Name != "extra-phase" || ph.Objective != "handle the extra work" {
		t.Errorf("SuggestedPhases[0] = %+v", ph)
	}
	if len(ph.Dependencies) != 1 || ph.Dependencies[0] != "phase-1" {
		t.Errorf("SuggestedPhases[0].Dependencies = %v", ph.Dependencies)
	}
	if len(restored.ChangedFiles) != 2 {
		t.Fatalf("ChangedFiles len = %d; want 2", len(restored.ChangedFiles))
	}
	if restored.Remainder != original.Remainder {
		t.Errorf("Remainder = %q; want %q", restored.Remainder, original.Remainder)
	}
}

// ---------------------------------------------------------------------------
// CompletionSignal omitempty — minimal ok signal should produce clean JSON
// ---------------------------------------------------------------------------

func TestCompletionSignal_OmitEmptyFields(t *testing.T) {
	sig := CompletionSignal{Kind: SignalOK, Summary: "done"}
	data, err := json.Marshal(sig)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}

	var raw map[string]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		t.Fatalf("unmarshal raw: %v", err)
	}

	// Only kind and summary should be present
	for _, key := range []string{"kind", "summary"} {
		if _, ok := raw[key]; !ok {
			t.Errorf("missing expected key %q", key)
		}
	}
	for _, key := range []string{"missing_input", "suggested_phases", "changed_files", "remainder"} {
		if _, ok := raw[key]; ok {
			t.Errorf("key %q should be omitted for empty value", key)
		}
	}
}
