package core

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
)

// SignalFileName is the well-known filename workers write to communicate
// structured completion information back to the engine.
const SignalFileName = "orchestrator.signal.json"

// CompletionSignalKind classifies the outcome of a phase beyond pass/fail.
type CompletionSignalKind string

const (
	// SignalOK means the phase completed normally. This is the default
	// when no signal file is present.
	SignalOK CompletionSignalKind = "ok"

	// SignalPartial means the phase did some work but couldn't finish
	// everything. The Remainder field describes what's left.
	SignalPartial CompletionSignalKind = "partial"

	// SignalDependencyMissing means the phase needed input that wasn't
	// provided by its dependencies.
	SignalDependencyMissing CompletionSignalKind = "dependency_missing"

	// SignalScopeExpansion means the phase discovered the task is
	// materially larger than expected.
	SignalScopeExpansion CompletionSignalKind = "scope_expansion"

	// SignalReplanRequired means the plan itself needs changing —
	// the current phase structure won't achieve the goal.
	SignalReplanRequired CompletionSignalKind = "replan_required"

	// SignalHumanDecisionNeeded means the phase hit an ambiguous
	// situation that requires human judgment.
	SignalHumanDecisionNeeded CompletionSignalKind = "human_decision_needed"
)

// PhaseDraft is a suggestion for a new phase that could be added to the plan.
// Used in SuggestedPhases when a worker signals scope expansion or replan.
type PhaseDraft struct {
	Name         string   `json:"name"`
	Objective    string   `json:"objective"`
	Persona      string   `json:"persona,omitempty"`
	DependsOn    string   `json:"depends_on,omitempty"`
	Skills       []string `json:"skills,omitempty"`
	Dependencies []string `json:"dependencies,omitempty"`
}

// CompletionSignal is the structured payload a worker writes to
// orchestrator.signal.json to communicate nuanced completion state.
type CompletionSignal struct {
	Kind            CompletionSignalKind `json:"kind"`
	Summary         string               `json:"summary,omitempty"`
	MissingInput    []string             `json:"missing_input,omitempty"`
	SuggestedPhases []PhaseDraft         `json:"suggested_phases,omitempty"`
	ChangedFiles    []string             `json:"changed_files,omitempty"`
	Remainder       string               `json:"remainder,omitempty"`
}

// ReadSignalFile reads orchestrator.signal.json from dir.
// Returns a zero-value ok signal if the file does not exist (backward compatible).
// Returns an error only if the file exists but cannot be parsed.
func ReadSignalFile(dir string) (CompletionSignal, error) {
	path := filepath.Join(dir, SignalFileName)
	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return CompletionSignal{Kind: SignalOK}, nil
		}
		return CompletionSignal{}, fmt.Errorf("reading signal file: %w", err)
	}

	var sig CompletionSignal
	if err := json.Unmarshal(data, &sig); err != nil {
		return CompletionSignal{}, fmt.Errorf("parsing signal file: %w", err)
	}

	// Default to ok if kind is empty (partial write or minimal signal).
	if sig.Kind == "" {
		sig.Kind = SignalOK
	}

	return sig, nil
}
