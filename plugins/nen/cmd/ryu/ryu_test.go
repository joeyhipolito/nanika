package main

import (
	"testing"
)

func TestIsCoordinatorPhase(t *testing.T) {
	tests := []struct {
		name    string
		phase   string
		persona string
		want    bool
	}{
		// Coordinator by phase name
		{"review phase", "review", "senior-backend-engineer", true},
		{"code-review phase", "code-review", "data-analyst", true},
		{"post-review phase", "post-review", "data-analyst", true},
		{"coordinate phase", "coordinate", "senior-backend-engineer", true},
		{"multi-phase-coordinate", "coordinate-phases", "staff-code-reviewer", true},
		// Coordinator by persona
		{"staff-code-reviewer persona", "validate", "staff-code-reviewer", true},
		{"staff-code-reviewer uppercase", "validate", "STAFF-CODE-REVIEWER", true},
		// Non-coordinator phases
		{"implement phase", "implement", "senior-backend-engineer", false},
		{"research phase", "research", "data-analyst", false},
		{"build phase", "build", "senior-backend-engineer", false},
		{"deploy phase", "deploy", "devops-engineer", false},
		{"empty name", "", "senior-backend-engineer", false},
		{"empty persona", "implement", "", false},
		// Edge: review substring in longer word (still matches due to Contains)
		{"reviewer in name", "peer-reviewer", "data-analyst", true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := isCoordinatorPhase(tt.phase, tt.persona)
			if got != tt.want {
				t.Errorf("isCoordinatorPhase(%q, %q) = %v, want %v", tt.phase, tt.persona, got, tt.want)
			}
		})
	}
}
