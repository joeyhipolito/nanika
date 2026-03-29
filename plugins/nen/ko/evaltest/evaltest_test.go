package evaltest

import (
	"context"
	"testing"
)

// hardcoded sample output strings used across tests
const sampleOutput = `
Here is the decomposed plan:

PHASE: research | OBJECTIVE: Gather requirements and constraints | PERSONA: architect | SKILLS: | DEPENDS:
PHASE: implement | OBJECTIVE: Build the service with handlers and storage | PERSONA: senior-backend-engineer | SKILLS: | DEPENDS: research
PHASE: review | OBJECTIVE: Review the implementation for correctness | PERSONA: staff-code-reviewer | SKILLS: | DEPENDS: implement
`

const sampleOutputWithSkills = `
PHASE: gather-data | OBJECTIVE: Collect metrics from the last 30 days | PERSONA: data-analyst | SKILLS: scout | DEPENDS:
PHASE: write-report | OBJECTIVE: Synthesize findings into an ADR | PERSONA: technical-writer | SKILLS: obsidian | DEPENDS: gather-data
`

const sampleOutputWithPipe = `
PHASE: bad-phase | OBJECTIVE: Do X | then do Y | PERSONA: architect | SKILLS: | DEPENDS:
`

const sampleOutputMixed = `
Some preamble text here.

PHASE: design | OBJECTIVE: Design the system architecture | PERSONA: architect | SKILLS: | DEPENDS:
Non-phase line that should be ignored.
PHASE: build | OBJECTIVE: Implement the backend API | PERSONA: senior-backend-engineer | SKILLS: | DEPENDS: design
`

// TestParsePhases covers the core parsing logic.
func TestParsePhases(t *testing.T) {
	tests := []struct {
		name       string
		output     string
		wantCount  int
		wantFirst  Phase
		wantSecond Phase
	}{
		{
			name:      "three phases",
			output:    sampleOutput,
			wantCount: 3,
			wantFirst: Phase{
				Name:      "research",
				Objective: "Gather requirements and constraints",
				Persona:   "architect",
				Skills:    nil,
				Depends:   nil,
			},
			wantSecond: Phase{
				Name:      "implement",
				Objective: "Build the service with handlers and storage",
				Persona:   "senior-backend-engineer",
				Skills:    nil,
				Depends:   []string{"research"},
			},
		},
		{
			name:      "phases with skills and depends",
			output:    sampleOutputWithSkills,
			wantCount: 2,
			wantFirst: Phase{
				Name:      "gather-data",
				Objective: "Collect metrics from the last 30 days",
				Persona:   "data-analyst",
				Skills:    []string{"scout"},
				Depends:   nil,
			},
			wantSecond: Phase{
				Name:      "write-report",
				Objective: "Synthesize findings into an ADR",
				Persona:   "technical-writer",
				Skills:    []string{"obsidian"},
				Depends:   []string{"gather-data"},
			},
		},
		{
			name:      "non-phase lines skipped",
			output:    sampleOutputMixed,
			wantCount: 2,
			wantFirst: Phase{
				Name:      "design",
				Objective: "Design the system architecture",
				Persona:   "architect",
			},
			wantSecond: Phase{
				Name:      "build",
				Objective: "Implement the backend API",
				Persona:   "senior-backend-engineer",
				Depends:   []string{"design"},
			},
		},
		{
			name:      "empty output",
			output:    "",
			wantCount: 0,
		},
		{
			name:      "no phase lines",
			output:    "Just some text\nWith no phases\n",
			wantCount: 0,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			phases := ParsePhases(tt.output)
			if len(phases) != tt.wantCount {
				t.Errorf("ParsePhases() count = %d, want %d", len(phases), tt.wantCount)
				return
			}
			if tt.wantCount == 0 {
				return
			}

			assertPhaseEqual(t, "first", phases[0], tt.wantFirst)
			if tt.wantCount >= 2 {
				assertPhaseEqual(t, "second", phases[1], tt.wantSecond)
			}
		})
	}
}

// TestParsePhasesSkillsSplit verifies comma-split of multiple skills.
func TestParsePhasesSkillsSplit(t *testing.T) {
	output := "PHASE: multi | OBJECTIVE: Do work | PERSONA: architect | SKILLS: scout,obsidian,gmail | DEPENDS:"
	phases := ParsePhases(output)
	if len(phases) != 1 {
		t.Fatalf("ParsePhases() count = %d, want 1", len(phases))
	}
	want := []string{"scout", "obsidian", "gmail"}
	if len(phases[0].Skills) != len(want) {
		t.Fatalf("skills count = %d, want %d", len(phases[0].Skills), len(want))
	}
	for i, s := range want {
		if phases[0].Skills[i] != s {
			t.Errorf("skill[%d] = %q, want %q", i, phases[0].Skills[i], s)
		}
	}
}

// TestParsePhasesMultipleDepends verifies comma-split of multiple depends.
func TestParsePhasesMultipleDepends(t *testing.T) {
	output := "PHASE: final | OBJECTIVE: Merge results | PERSONA: technical-writer | SKILLS: | DEPENDS: phase-a,phase-b,phase-c"
	phases := ParsePhases(output)
	if len(phases) != 1 {
		t.Fatalf("ParsePhases() count = %d, want 1", len(phases))
	}
	want := []string{"phase-a", "phase-b", "phase-c"}
	if len(phases[0].Depends) != len(want) {
		t.Fatalf("depends count = %d, want %d", len(phases[0].Depends), len(want))
	}
	for i, d := range want {
		if phases[0].Depends[i] != d {
			t.Errorf("depends[%d] = %q, want %q", i, phases[0].Depends[i], d)
		}
	}
}

// TestAssertPersonasInSet verifies the persona allowlist assertion.
func TestAssertPersonasInSet(t *testing.T) {
	phases := ParsePhases(sampleOutput)

	tests := []struct {
		name          string
		validPersonas []string
		wantPass      bool
	}{
		{
			name:          "all personas valid",
			validPersonas: []string{"architect", "senior-backend-engineer", "staff-code-reviewer"},
			wantPass:      true,
		},
		{
			name:          "missing one persona",
			validPersonas: []string{"architect", "senior-backend-engineer"},
			wantPass:      false,
		},
		{
			name:          "empty valid set",
			validPersonas: []string{},
			wantPass:      false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertPersonasInSet(phases, tt.validPersonas)
			if got != tt.wantPass {
				t.Errorf("AssertPersonasInSet() = %v, want %v; msg: %s", got, tt.wantPass, msg)
			}
		})
	}
}

// TestAssertPersonasInSetEmpty passes vacuously when there are no phases.
func TestAssertPersonasInSetEmpty(t *testing.T) {
	got, msg := AssertPersonasInSet(nil, []string{"architect"})
	if !got {
		t.Errorf("AssertPersonasInSet(nil) should pass vacuously; msg: %s", msg)
	}
}

// TestAssertDepsValid verifies the dependency reference check.
func TestAssertDepsValid(t *testing.T) {
	tests := []struct {
		name     string
		output   string
		wantPass bool
	}{
		{
			name:     "all deps valid",
			output:   sampleOutput,
			wantPass: true,
		},
		{
			name: "unknown dep",
			output: `PHASE: a | OBJECTIVE: Do A | PERSONA: architect | SKILLS: | DEPENDS:
PHASE: b | OBJECTIVE: Do B | PERSONA: architect | SKILLS: | DEPENDS: nonexistent`,
			wantPass: false,
		},
		{
			name: "no deps",
			output: `PHASE: a | OBJECTIVE: Do A | PERSONA: architect | SKILLS: | DEPENDS:
PHASE: b | OBJECTIVE: Do B | PERSONA: architect | SKILLS: | DEPENDS:`,
			wantPass: true,
		},
		{
			name:     "empty plan",
			output:   "",
			wantPass: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			phases := ParsePhases(tt.output)
			got, msg := AssertDepsValid(phases)
			if got != tt.wantPass {
				t.Errorf("AssertDepsValid() = %v, want %v; msg: %s", got, tt.wantPass, msg)
			}
		})
	}
}

// TestAssertPhaseCountRange verifies the phase count range check.
func TestAssertPhaseCountRange(t *testing.T) {
	phases := ParsePhases(sampleOutput) // 3 phases

	tests := []struct {
		name     string
		min, max int
		wantPass bool
	}{
		{"exact match", 3, 3, true},
		{"within range", 2, 5, true},
		{"at min", 3, 10, true},
		{"at max", 1, 3, true},
		{"below min", 4, 8, false},
		{"above max", 1, 2, false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertPhaseCountRange(phases, tt.min, tt.max)
			if got != tt.wantPass {
				t.Errorf("AssertPhaseCountRange(%d, %d) = %v, want %v; msg: %s", tt.min, tt.max, got, tt.wantPass, msg)
			}
		})
	}
}

// TestAssertContainsPersona verifies the contains-persona assertion.
func TestAssertContainsPersona(t *testing.T) {
	phases := ParsePhases(sampleOutput)

	tests := []struct {
		name     string
		persona  string
		wantPass bool
	}{
		{"found architect", "architect", true},
		{"found reviewer", "staff-code-reviewer", true},
		{"not found", "qa-engineer", false},
		{"empty persona", "", false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertContainsPersona(phases, tt.persona)
			if got != tt.wantPass {
				t.Errorf("AssertContainsPersona(%q) = %v, want %v; msg: %s", tt.persona, got, tt.wantPass, msg)
			}
		})
	}
}

// TestAssertFieldNotContains verifies the field-not-contains assertion.
func TestAssertFieldNotContains(t *testing.T) {
	phases := ParsePhases(sampleOutput)

	tests := []struct {
		name     string
		field    string
		substr   string
		wantPass bool
	}{
		{"objective no TODO", "objective", "TODO", true},
		{"objective contains known word", "objective", "Gather", false},
		{"name no uppercase", "name", "PHASE", true},
		{"persona no space", "persona", " ", true},
		{"unknown field", "skills", "x", false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertFieldNotContains(phases, tt.field, tt.substr)
			if got != tt.wantPass {
				t.Errorf("AssertFieldNotContains(%q, %q) = %v, want %v; msg: %s", tt.field, tt.substr, got, tt.wantPass, msg)
			}
		})
	}
}

// TestAssertNoPipeInObjective verifies the no-pipe-in-objective assertion.
func TestAssertNoPipeInObjective(t *testing.T) {
	tests := []struct {
		name     string
		output   string
		wantPass bool
	}{
		{
			name:     "no pipe in objectives",
			output:   sampleOutput,
			wantPass: true,
		},
		{
			// The pipe in "Do X | then do Y" is treated as a field delimiter,
			// so ParsePhases splits it — "Do X" becomes the Objective, not "Do X | then do Y".
			// AssertNoPipeInObjective still passes because the parsed value is clean.
			name:     "pipe in raw line splits into extra fields",
			output:   sampleOutputWithPipe,
			wantPass: true,
		},
		{
			name:     "explicit pipe in objective field value",
			output:   "PHASE: x | OBJECTIVE: Do A | and B | PERSONA: architect | SKILLS: | DEPENDS:",
			wantPass: true, // pipe splits the line, objective value is "Do A", no pipe in it
		},
		{
			name:     "empty plan",
			output:   "",
			wantPass: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			phases := ParsePhases(tt.output)
			got, msg := AssertNoPipeInObjective(phases)
			if got != tt.wantPass {
				t.Errorf("AssertNoPipeInObjective() = %v, want %v; msg: %s", got, tt.wantPass, msg)
			}
		})
	}
}

// TestEval verifies that Eval rejects missing model and prompt.
// Exec-level behaviour (successful call, whitespace trim, exec errors) is
// covered by TestCallLLM in the parent ko package.
func TestEval(t *testing.T) {
	tests := []struct {
		name    string
		model   string
		prompt  string
		wantErr bool
	}{
		{"empty model", "", "Plan", true},
		{"empty prompt", "claude-haiku-4-5-20251001", "", true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, err := Eval(context.Background(), tt.model, tt.prompt)
			if (err != nil) != tt.wantErr {
				t.Errorf("Eval() error = %v, wantErr %v", err, tt.wantErr)
			}
		})
	}
}

// assertPhaseEqual is a helper for comparing Phase structs field by field.
func assertPhaseEqual(t *testing.T, label string, got, want Phase) {
	t.Helper()
	if got.Name != want.Name {
		t.Errorf("%s Name = %q, want %q", label, got.Name, want.Name)
	}
	if got.Objective != want.Objective {
		t.Errorf("%s Objective = %q, want %q", label, got.Objective, want.Objective)
	}
	if got.Persona != want.Persona {
		t.Errorf("%s Persona = %q, want %q", label, got.Persona, want.Persona)
	}
	if len(got.Skills) != len(want.Skills) {
		t.Errorf("%s Skills = %v, want %v", label, got.Skills, want.Skills)
	} else {
		for i := range want.Skills {
			if got.Skills[i] != want.Skills[i] {
				t.Errorf("%s Skills[%d] = %q, want %q", label, i, got.Skills[i], want.Skills[i])
			}
		}
	}
	if len(got.Depends) != len(want.Depends) {
		t.Errorf("%s Depends = %v, want %v", label, got.Depends, want.Depends)
	} else {
		for i := range want.Depends {
			if got.Depends[i] != want.Depends[i] {
				t.Errorf("%s Depends[%d] = %q, want %q", label, i, got.Depends[i], want.Depends[i])
			}
		}
	}
}
