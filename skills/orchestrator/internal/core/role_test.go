package core

import (
	"strings"
	"testing"
)

func TestClassifyRole(t *testing.T) {
	tests := []struct {
		name      string
		persona   string
		phaseName string
		objective string
		want      Role
	}{
		{name: "reviewer persona wins", persona: "staff-code-reviewer", want: RoleReviewer},
		{name: "planner persona wins", persona: "architect", want: RolePlanner},
		{name: "phase name review", phaseName: "validate-output", want: RoleReviewer},
		{name: "phase name plan", phaseName: "design-architecture", want: RolePlanner},
		{name: "objective review", objective: "Review the implementation for regressions", want: RoleReviewer},
		{name: "objective plan", objective: "Design the rollout plan", want: RolePlanner},
		{name: "default implementer", objective: "Implement the feature", want: RoleImplementer},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := ClassifyRole(tt.persona, tt.phaseName, tt.objective); got != tt.want {
				t.Fatalf("ClassifyRole(%q, %q, %q) = %q, want %q", tt.persona, tt.phaseName, tt.objective, got, tt.want)
			}
		})
	}
}

func TestHandoffRecordFormatForWorker(t *testing.T) {
	h := HandoffRecord{
		FromRole:     RolePlanner,
		ToRole:       RoleImplementer,
		FromPersona:  "architect",
		Summary:      "Defined the rollout plan and constraints.",
		Expectations: []string{"Follow the plan", "Flag ambiguities as DECISION markers"},
	}

	rendered := h.FormatForWorker()
	for _, want := range []string{
		"You are the **implementer**",
		"preceding **planner** (architect)",
		"Defined the rollout plan and constraints.",
		"Follow the plan",
		"Flag ambiguities as DECISION markers",
	} {
		if !strings.Contains(rendered, want) {
			t.Fatalf("formatted handoff missing %q:\n%s", want, rendered)
		}
	}
}

func TestRoleTransitionExpectations(t *testing.T) {
	tests := []struct {
		name string
		from Role
		to   Role
		want int
	}{
		{name: "planner to implementer", from: RolePlanner, to: RoleImplementer, want: 3},
		{name: "implementer to reviewer", from: RoleImplementer, to: RoleReviewer, want: 3},
		{name: "reviewer to implementer", from: RoleReviewer, to: RoleImplementer, want: 3},
		{name: "planner to reviewer", from: RolePlanner, to: RoleReviewer, want: 2},
		{name: "unknown transition", from: RoleReviewer, to: RoleReviewer, want: 0},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := RoleTransitionExpectations(tt.from, tt.to)
			if len(got) != tt.want {
				t.Fatalf("len(RoleTransitionExpectations(%q,%q)) = %d, want %d", tt.from, tt.to, len(got), tt.want)
			}
		})
	}
}
