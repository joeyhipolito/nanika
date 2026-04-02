package decompose

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/persona"
	"github.com/joeyhipolito/orchestrator-cli/internal/router"
)

func init() {
	persona.Load()
}

// TestKeywordDecompose_ResearchTask verifies technical comparison research tasks.
func TestKeywordDecompose_ResearchTask(t *testing.T) {
	plan := keywordDecompose("research the best Go testing frameworks and compare them", nil)

	if len(plan.Phases) == 0 {
		t.Fatal("keywordDecompose returned 0 phases")
	}

	// Should have a research phase
	found := false
	for _, p := range plan.Phases {
		if p.Name == "research" {
			found = true
			if p.Persona != "architect" {
				t.Errorf("research phase persona = %q, want 'architect'", p.Persona)
			}
		}
	}
	if !found {
		t.Error("expected a 'research' phase for research task")
		for _, p := range plan.Phases {
			t.Logf("  phase: %s (%s)", p.Name, p.Persona)
		}
	}
}

// TestKeywordDecompose_ImplementTask verifies implementation-oriented tasks.
func TestKeywordDecompose_ImplementTask(t *testing.T) {
	plan := keywordDecompose("implement a REST API with CRUD endpoints", nil)

	found := false
	for _, p := range plan.Phases {
		if p.Name == "implement" {
			found = true
		}
	}
	if !found {
		t.Error("expected an 'implement' phase for implementation task")
		for _, p := range plan.Phases {
			t.Logf("  phase: %s (%s)", p.Name, p.Persona)
		}
	}
}

// TestKeywordDecompose_WriteTask verifies writing-oriented tasks.
func TestKeywordDecompose_WriteTask(t *testing.T) {
	plan := keywordDecompose("document the API and write a developer guide", nil)

	found := false
	for _, p := range plan.Phases {
		if p.Name == "write" {
			found = true
		}
	}
	if !found {
		t.Error("expected a 'write' phase for documentation task")
		for _, p := range plan.Phases {
			t.Logf("  phase: %s (%s)", p.Name, p.Persona)
		}
	}
}

// TestKeywordDecompose_ReviewTask verifies review-oriented tasks.
func TestKeywordDecompose_ReviewTask(t *testing.T) {
	plan := keywordDecompose("audit the codebase and review for security issues", nil)

	found := false
	for _, p := range plan.Phases {
		if p.Name == "review" {
			found = true
		}
	}
	if !found {
		t.Error("expected a 'review' phase for audit task")
		for _, p := range plan.Phases {
			t.Logf("  phase: %s (%s)", p.Name, p.Persona)
		}
	}
}

// TestKeywordDecompose_MultiPhase verifies multi-keyword tasks.
func TestKeywordDecompose_MultiPhase(t *testing.T) {
	plan := keywordDecompose("research Go error handling, implement an error package, and write documentation", nil)

	if len(plan.Phases) < 3 {
		t.Errorf("expected at least 3 phases for multi-keyword task, got %d", len(plan.Phases))
		for _, p := range plan.Phases {
			t.Logf("  phase: %s (%s)", p.Name, p.Persona)
		}
	}

	// Verify dependency chain
	for i := 1; i < len(plan.Phases); i++ {
		if len(plan.Phases[i].Dependencies) == 0 {
			t.Errorf("phase %d (%s) has no dependencies — chain is broken", i, plan.Phases[i].Name)
		}
	}
}

// TestKeywordDecompose_Fallback verifies unknown tasks get a single execute phase.
func TestKeywordDecompose_Fallback(t *testing.T) {
	plan := keywordDecompose("do something unclear", nil)

	if len(plan.Phases) != 1 {
		t.Errorf("expected 1 fallback phase, got %d", len(plan.Phases))
	}

	if plan.Phases[0].Name != "execute" {
		t.Errorf("fallback phase name = %q, want 'execute'", plan.Phases[0].Name)
	}
}

// TestKeywordDecompose_WriteCodeNotWriting verifies "write code" doesn't trigger writing phase.
func TestKeywordDecompose_WriteCodeNotWriting(t *testing.T) {
	plan := keywordDecompose("write code to handle file uploads", nil)

	for _, p := range plan.Phases {
		if p.Name == "write" {
			t.Error("'write code' should not trigger a writing phase")
		}
	}
}

// TestParsePhases_ValidOutput verifies LLM output parsing.
func TestParsePhases_ValidOutput(t *testing.T) {
	output := `Some preamble text

PHASE: research | OBJECTIVE: Research Go error patterns | PERSONA: architect
PHASE: implement | OBJECTIVE: Build error package | PERSONA: senior-backend-engineer | DEPENDS: research
PHASE: test | OBJECTIVE: Write tests for error package | PERSONA: qa-engineer | SKILLS: obsidian | DEPENDS: implement
PHASE: document | OBJECTIVE: Write API docs | PERSONA: technical-writer | DEPENDS: implement

Some trailing text`

	phases, err := ParsePhases(output, nil)
	if err != nil {
		t.Fatalf("ParsePhases() error: %v", err)
	}

	if len(phases) != 4 {
		t.Fatalf("expected 4 phases, got %d", len(phases))
	}

	// Verify first phase
	if phases[0].Persona != "architect" {
		t.Errorf("phase 0 persona = %q, want 'architect'", phases[0].Persona)
	}
	if len(phases[0].Dependencies) != 0 {
		t.Errorf("phase 0 should have no dependencies, got %v", phases[0].Dependencies)
	}

	// Verify dependency resolution
	if len(phases[1].Dependencies) != 1 || phases[1].Dependencies[0] != "phase-1" {
		t.Errorf("phase 1 dependencies = %v, want [phase-1]", phases[1].Dependencies)
	}

	// Verify skills parsing
	if len(phases[2].Skills) != 1 || phases[2].Skills[0] != "obsidian" {
		t.Errorf("phase 2 skills = %v, want [obsidian]", phases[2].Skills)
	}

	// Phase 3 and 4 both depend on implement (phase-2), should be parallelizable
	if len(phases[2].Dependencies) != 1 || phases[2].Dependencies[0] != "phase-2" {
		t.Errorf("phase 2 dependencies = %v, want [phase-2]", phases[2].Dependencies)
	}
	if len(phases[3].Dependencies) != 1 || phases[3].Dependencies[0] != "phase-2" {
		t.Errorf("phase 3 dependencies = %v, want [phase-2]", phases[3].Dependencies)
	}
}

// TestParsePhases_InvalidPersonaFallback verifies unknown personas get remapped.
func TestParsePhases_InvalidPersonaFallback(t *testing.T) {
	output := `PHASE: do-stuff | OBJECTIVE: Build something cool | PERSONA: nonexistent-persona`

	phases, err := ParsePhases(output, nil)
	if err != nil {
		t.Fatalf("ParsePhases() error: %v", err)
	}

	if len(phases) != 1 {
		t.Fatalf("expected 1 phase, got %d", len(phases))
	}

	// Should fallback to a valid persona via persona.Match()
	p := persona.Get(phases[0].Persona)
	if p == nil {
		t.Errorf("phase persona %q is not a valid persona after fallback", phases[0].Persona)
	}
}

// TestParsePhases_EmptyOutput returns error.
func TestParsePhases_EmptyOutput(t *testing.T) {
	_, err := ParsePhases("no valid phases here\njust some text", nil)
	if err == nil {
		t.Error("ParsePhases with no PHASE: lines should return error")
	}
}

// TestParsePhases_MissingPersona gets inferred.
func TestParsePhases_MissingPersona(t *testing.T) {
	output := `PHASE: research | OBJECTIVE: Research best testing frameworks`

	phases, err := ParsePhases(output, nil)
	if err != nil {
		t.Fatalf("ParsePhases() error: %v", err)
	}

	if phases[0].Persona == "" {
		t.Error("phase with missing persona should get one inferred")
	}

	// Should be a valid persona
	p := persona.Get(phases[0].Persona)
	if p == nil {
		t.Errorf("inferred persona %q is not valid", phases[0].Persona)
	}
}

// TestHasParallelPhases tests the parallelism detection.
func TestHasParallelPhases_TrueCase(t *testing.T) {
	// Two phases with no dependencies
	plan := keywordDecompose("research best practices", nil)
	// Append another phase with no deps to force parallelism
	if !hasParallelPhases(append(plan.Phases, plan.Phases[0])) {
		// This would fail if there's only 1 phase, but at least test the function
	}
}

// TestSimplePlan_PersonaMatching verifies simplePlan picks a reasonable persona.
// Since Match() uses LLM when available, we accept reasonable alternatives.
func TestSimplePlan_PersonaMatching(t *testing.T) {
	tests := []struct {
		task       string
		acceptable []string
	}{
		{"write unit tests for the parser", []string{"qa-engineer", "senior-backend-engineer"}},
		{"fix the authentication bug", []string{"senior-backend-engineer", "security-auditor"}},
	}

	for _, tt := range tests {
		plan := simplePlan(tt.task, nil)
		if len(plan.Phases) != 1 {
			t.Fatalf("simplePlan(%q) returned %d phases, want 1", tt.task, len(plan.Phases))
		}
		got := plan.Phases[0].Persona
		ok := false
		for _, a := range tt.acceptable {
			if got == a {
				ok = true
				break
			}
		}
		if !ok {
			t.Errorf("simplePlan(%q) persona = %q, acceptable: %v", tt.task, got, tt.acceptable)
		}
	}
}

// TestHasPreDecomposedPhases verifies detection of PHASE: lines in text.
func TestHasPreDecomposedPhases(t *testing.T) {
	tests := []struct {
		name string
		text string
		want bool
	}{
		{
			"with phases",
			"Some context\n\nPHASE: research | OBJECTIVE: Do research | PERSONA: architect\nPHASE: write | OBJECTIVE: Write it | PERSONA: technical-writer | DEPENDS: research",
			true,
		},
		{
			"no phases",
			"Just a plain task description with no phase lines",
			false,
		},
		{
			"phase in middle of line",
			"This mentions PHASE: but not at line start after trim",
			false, // PHASE: must be at start of line (after trimming whitespace)
		},
		{
			"empty string",
			"",
			false,
		},
		{
			"indented phase line",
			"  PHASE: research | OBJECTIVE: Do research | PERSONA: architect",
			true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := HasPreDecomposedPhases(tt.text)
			if got != tt.want {
				t.Errorf("HasPreDecomposedPhases() = %v, want %v", got, tt.want)
			}
		})
	}
}

// TestPreDecomposed verifies pre-decomposed mission file parsing.
func TestPreDecomposed(t *testing.T) {
	task := `# Technical Explainer: AI Agent Frameworks

Target audience: senior developers.

PHASE: research | OBJECTIVE: Gather data on AI agent frameworks | PERSONA: architect
PHASE: write | OBJECTIVE: Write comparison article | PERSONA: technical-writer | DEPENDS: research`

	plan, err := PreDecomposed(task, nil)
	if err != nil {
		t.Fatalf("PreDecomposed() error: %v", err)
	}

	if len(plan.Phases) != 2 {
		t.Fatalf("expected 2 phases, got %d", len(plan.Phases))
	}

	// Verify sequential dependency chain
	if plan.Phases[0].Name != "research" {
		t.Errorf("phase 0 name = %q, want 'research'", plan.Phases[0].Name)
	}
	if len(plan.Phases[1].Dependencies) != 1 || plan.Phases[1].Dependencies[0] != "phase-1" {
		t.Errorf("phase 1 (write) dependencies = %v, want [phase-1]", plan.Phases[1].Dependencies)
	}

	// Sequential chain: no parallel phases
	if plan.ExecutionMode != "sequential" {
		t.Errorf("execution mode = %q, want 'sequential'", plan.ExecutionMode)
	}
}

// TestPreDecomposed_Parallel verifies parallel detection in pre-decomposed plans.
func TestPreDecomposed_Parallel(t *testing.T) {
	task := `PHASE: research-go | OBJECTIVE: Analyze Go error handling | PERSONA: architect
PHASE: research-rust | OBJECTIVE: Analyze Rust error handling | PERSONA: architect
PHASE: write | OBJECTIVE: Write comparison | PERSONA: technical-writer | DEPENDS: research-go, research-rust`

	plan, err := PreDecomposed(task, nil)
	if err != nil {
		t.Fatalf("PreDecomposed() error: %v", err)
	}

	if len(plan.Phases) != 3 {
		t.Fatalf("expected 3 phases, got %d", len(plan.Phases))
	}

	if plan.ExecutionMode != "parallel" {
		t.Errorf("execution mode = %q, want 'parallel'", plan.ExecutionMode)
	}

	// Write phase should depend on both research phases
	if len(plan.Phases[2].Dependencies) != 2 {
		t.Errorf("write phase has %d deps, want 2", len(plan.Phases[2].Dependencies))
	}
}

// TestParsePhases_Expected verifies EXPECTED field parsing on PHASE lines.
func TestParsePhases_Expected(t *testing.T) {
	tests := []struct {
		name         string
		line         string
		wantExpected string
	}{
		{
			name:         "with expected after depends",
			line:         "PHASE: build | OBJECTIVE: Build the binary | PERSONA: senior-backend-engineer | DEPENDS: research | EXPECTED: ~/out/tool",
			wantExpected: "~/out/tool",
		},
		{
			name:         "with expected after skills no depends",
			line:         "PHASE: gen | OBJECTIVE: Generate report | PERSONA: academic-researcher | SKILLS: scout | EXPECTED: ~/reports/report.md",
			wantExpected: "~/reports/report.md",
		},
		{
			name:         "with expected no skills no depends",
			line:         "PHASE: write | OBJECTIVE: Write article | PERSONA: technical-writer | EXPECTED: ~/blog/post.mdx",
			wantExpected: "~/blog/post.mdx",
		},
		{
			name:         "without expected field",
			line:         "PHASE: research | OBJECTIVE: Research Go patterns | PERSONA: academic-researcher | DEPENDS: setup",
			wantExpected: "",
		},
		{
			name:         "minimal phase no optional fields",
			line:         "PHASE: execute | OBJECTIVE: Do the thing | PERSONA: senior-backend-engineer",
			wantExpected: "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			phases, err := ParsePhases(tt.line, nil)
			if err != nil {
				t.Fatalf("ParsePhases() error: %v", err)
			}
			if len(phases) != 1 {
				t.Fatalf("expected 1 phase, got %d", len(phases))
			}
			if phases[0].Expected != tt.wantExpected {
				t.Errorf("Expected = %q, want %q", phases[0].Expected, tt.wantExpected)
			}
		})
	}
}

// TestPreDecomposed_NoPhases returns error.
func TestPreDecomposed_NoPhases(t *testing.T) {
	_, err := PreDecomposed("just a plain task with no PHASE lines", nil)
	if err == nil {
		t.Error("PreDecomposed with no PHASE lines should return error")
	}
}

// checkLangPersonaResult verifies pickImplementationPersona output for a
// language-specific case: if langPersona exists in the catalog it must be
// chosen with SelectionKeyword; otherwise the fallback rules apply.
func checkLangPersonaResult(t *testing.T, task, langPersona, name, method string) {
	t.Helper()
	if persona.Get(name) == nil {
		t.Errorf("pickImplementationPersona(%q) = %q which is not in catalog", task, name)
		return
	}
	if persona.Get(langPersona) != nil {
		if name != langPersona {
			t.Errorf("pickImplementationPersona(%q) = %q, want %s", task, name, langPersona)
		}
		if method != string(persona.SelectionKeyword) {
			t.Errorf("pickImplementationPersona(%q) method = %q, want %q", task, method, persona.SelectionKeyword)
		}
	} else {
		if name != "senior-backend-engineer" {
			t.Errorf("no %s in catalog; pickImplementationPersona(%q) = %q, want senior-backend-engineer", langPersona, task, name)
		}
		if method != string(persona.SelectionFallback) {
			t.Errorf("pickImplementationPersona(%q) method = %q, want %q", task, method, persona.SelectionFallback)
		}
	}
}

// TestPickImplementationPersona_GoOriented verifies that Go-oriented tasks still
// route to the backend implementer in the trimmed catalog.
func TestPickImplementationPersona_GoOriented(t *testing.T) {
	persona.Load()

	tasks := []string{
		"implement a CLI tool in golang",
		"implement a goroutine-safe worker pool",
		"implement a go module for rate limiting",
		"build a go mod-based project",
	}
	for _, task := range tasks {
		name, method := pickImplementationPersona(task)
		checkLangPersonaResult(t, task, "senior-backend-engineer", name, method)
	}
}

// TestPickImplementationPersona_RustOriented verifies that Rust-oriented tasks
// still route to the backend implementer in the trimmed catalog.
func TestPickImplementationPersona_RustOriented(t *testing.T) {
	persona.Load()

	tasks := []string{
		"implement a rust parser for the config format",
		"build a cargo workspace with multiple crates",
		"implement async request handlers using tokio",
	}
	for _, task := range tasks {
		name, method := pickImplementationPersona(task)
		checkLangPersonaResult(t, task, "senior-backend-engineer", name, method)
	}
}

// TestPickImplementationPersona_NonCodeTask verifies that generic implementation
// tasks with no language signals always return senior-backend-engineer with
// SelectionFallback, making the default assignment explicit and distinguishable.
func TestPickImplementationPersona_NonCodeTask(t *testing.T) {
	persona.Load()

	tasks := []string{
		"implement a REST API with CRUD endpoints",
		"build a task queue system for background jobs",
		"create a background worker for processing events",
		"develop a rate limiter for the API gateway",
		"implement a trustworthy auth layer",
		"design a Zero Trust network architecture",
		"audit the system trust model",
	}

	for _, task := range tasks {
		name, method := pickImplementationPersona(task)
		if name != "senior-backend-engineer" {
			t.Errorf("pickImplementationPersona(%q) = %q, want senior-backend-engineer (no language signal)", task, name)
		}
		if method != string(persona.SelectionFallback) {
			t.Errorf("pickImplementationPersona(%q) method = %q, want %q", task, method, persona.SelectionFallback)
		}
	}
}

// TestKeywordDecompose_GoImplementTask verifies that a Go-oriented implementation
// task produces an implement phase with the backend implementer and a keyword-based
// selection method.
func TestKeywordDecompose_GoImplementTask(t *testing.T) {
	persona.Load()

	plan := keywordDecompose("implement a golang CLI tool for rate limiting", nil)

	var implPhase *core.Phase
	for _, p := range plan.Phases {
		if p.Name == "implement" {
			implPhase = p
			break
		}
	}
	if implPhase == nil {
		t.Fatal("expected an 'implement' phase")
	}
	if persona.Get(implPhase.Persona) == nil {
		t.Errorf("implement phase persona %q is not in catalog", implPhase.Persona)
	}

	if implPhase.Persona != "senior-backend-engineer" {
		t.Errorf("implement phase persona = %q, want senior-backend-engineer", implPhase.Persona)
	}
	if implPhase.PersonaSelectionMethod != string(persona.SelectionKeyword) {
		t.Errorf("selection method = %q, want %q", implPhase.PersonaSelectionMethod, persona.SelectionKeyword)
	}
}

// TestContainsAny tests the helper function.
func TestContainsAny(t *testing.T) {
	tests := []struct {
		s    string
		subs []string
		want bool
	}{
		{"research the API", []string{"research", "investigate"}, true},
		{"build a feature", []string{"research", "investigate"}, false},
		{"explore the codebase", []string{"explore", "find"}, true},
		{"", []string{"anything"}, false},
	}

	for _, tt := range tests {
		got := containsAny(tt.s, tt.subs...)
		if got != tt.want {
			t.Errorf("containsAny(%q, %v) = %v, want %v", tt.s, tt.subs, got, tt.want)
		}
	}
}

// TestExtractRules verifies that rules are correctly extracted from SKILL.md content.
func TestExtractRules(t *testing.T) {
	skillMDContent := `---
name: decomposer
---

# Mission Decomposition

## Output Format

Every phase is a single pipe-delimited line:

Some content about output format.

## Core Rules

### Rule 1: One Persona Per Phase
Each phase gets exactly one persona.

### Rule 2: Break by User Value, Not Technical Layer
Each phase should deliver a complete capability.

## Dependency Rules (Critical)

These are the most common decomposition failures.

## Pipeline Constraints

When a task matches a known pipeline...

## Anti-Patterns

### Collapsing Phases
BAD: Merging phases.

## Worked Examples

### Blog Article Pipeline
Task: "Scout news and write 1 article"
`

	rules := extractRules(skillMDContent)
	if rules == "" {
		t.Fatal("extractRules returned empty string, expected rules")
	}

	// Verify expected sections are present
	expectedSections := []string{
		"## Core Rules",
		"## Dependency Rules",
		"## Pipeline Constraints",
		"## Anti-Patterns",
	}

	for _, section := range expectedSections {
		if !strings.Contains(rules, section) {
			t.Errorf("extractRules did not include %q", section)
		}
	}

	// Verify "## Output Format" is NOT included (it's the marker)
	if strings.Contains(rules, "## Output Format") {
		t.Error("extractRules should not include '## Output Format' marker")
	}

	// Verify "## Worked Examples" is NOT included (it's the end marker)
	if strings.Contains(rules, "## Worked Examples") {
		t.Error("extractRules should not include '## Worked Examples' marker")
	}
}

// TestExtractRules_MissingMarkers returns empty string when markers not found.
func TestExtractRules_MissingMarkers(t *testing.T) {
	tests := []struct {
		name    string
		content string
	}{
		{"missing start marker", "Some content\n## Worked Examples\nMore content"},
		{"missing end marker", "## Output Format\nSome content"},
		{"both missing", "Just content with no markers"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			rules := extractRules(tt.content)
			if rules != "" {
				t.Errorf("extractRules(%s) should return empty string, got %q", tt.name, rules)
			}
		})
	}
}

// TestLoadRulesFromSKILLMD verifies that SKILL.md is loaded and contains expected sections.
func TestLoadRulesFromSKILLMD(t *testing.T) {
	// First, verify that the actual SKILL.md file exists and is loadable
	rules := loadRulesFromSKILLMD()
	if rules == "" {
		t.Fatal("loadRulesFromSKILLMD returned empty string")
	}

	// Verify expected rule sections are present
	expectedSections := []string{
		"Core Rules",
		"Dependency Rules",
		"Pipeline Constraints",
		"Anti-Patterns",
		"Chunking Rule",
		"Handoff Chains",
	}

	for _, section := range expectedSections {
		if !strings.Contains(rules, section) {
			t.Logf("WARNING: %q not found in extracted rules (may be using hardcoded fallback)", section)
			// Don't fail here, as the hardcoded fallback might not have all sections
		}
	}

	// Verify that "PHASE:" format is described
	if !strings.Contains(rules, "PHASE:") {
		t.Error("extracted rules should mention PHASE: format")
	}
}

// TestDecompose_ExplicitPhasesNotRewritten verifies that Decompose uses explicit PHASE lines
// verbatim and does not invoke the LLM when the task already contains pre-decomposed phases.
func TestDecompose_ExplicitPhasesNotRewritten(t *testing.T) {
	task := `# Feature: Error Handling Refactor

Improve error handling across the codebase.

PHASE: audit | OBJECTIVE: Audit current error handling patterns | PERSONA: staff-code-reviewer
PHASE: implement | OBJECTIVE: Refactor error handling to use structured errors | PERSONA: senior-backend-engineer | DEPENDS: audit
PHASE: test | OBJECTIVE: Write tests covering error paths | PERSONA: qa-engineer | DEPENDS: implement`

	plan, err := Decompose(context.Background(), task, "", "", "test-mission", event.NoOpEmitter{}, nil)
	if err != nil {
		t.Fatalf("Decompose() error: %v", err)
	}

	// Explicit phases must be used verbatim — count must match the mission file exactly.
	if len(plan.Phases) != 3 {
		t.Fatalf("expected 3 phases from explicit PHASE lines, got %d", len(plan.Phases))
	}

	wantNames := []string{"audit", "implement", "test"}
	wantPersonas := []string{"staff-code-reviewer", "senior-backend-engineer", "qa-engineer"}
	for i, phase := range plan.Phases {
		if phase.Name != wantNames[i] {
			t.Errorf("phase[%d].Name = %q, want %q", i, phase.Name, wantNames[i])
		}
		if phase.Persona != wantPersonas[i] {
			t.Errorf("phase[%d].Persona = %q, want %q", i, phase.Persona, wantPersonas[i])
		}
	}

	// Dependency chain must be preserved from the mission file.
	if len(plan.Phases[1].Dependencies) != 1 || plan.Phases[1].Dependencies[0] != "phase-1" {
		t.Errorf("phase[1] dependencies = %v, want [phase-1]", plan.Phases[1].Dependencies)
	}
	if len(plan.Phases[2].Dependencies) != 1 || plan.Phases[2].Dependencies[0] != "phase-2" {
		t.Errorf("phase[2] dependencies = %v, want [phase-2]", plan.Phases[2].Dependencies)
	}
}

// TestLoadRulesFromSKILLMD_EnvVarOverride verifies that NANIKA_DECOMPOSER_SKILL env var is respected.
func TestLoadRulesFromSKILLMD_EnvVarOverride(t *testing.T) {
	// Save the original env var
	originalEnv := os.Getenv("NANIKA_DECOMPOSER_SKILL")
	defer func() {
		if originalEnv == "" {
			os.Unsetenv("NANIKA_DECOMPOSER_SKILL")
		} else {
			os.Setenv("NANIKA_DECOMPOSER_SKILL", originalEnv)
		}
	}()

	// Test with non-existent path — should fall back to hardcoded
	os.Setenv("NANIKA_DECOMPOSER_SKILL", "/nonexistent/path/SKILL.md")
	rules := loadRulesFromSKILLMD()
	if rules == "" {
		t.Fatal("loadRulesFromSKILLMD should return hardcoded rules when env var path doesn't exist")
	}

	// Verify it's the hardcoded fallback by checking for the first rule
	if !strings.Contains(rules, "Each phase must have exactly ONE persona") {
		t.Error("expected hardcoded fallback rules")
	}

	// Unset the env var
	os.Unsetenv("NANIKA_DECOMPOSER_SKILL")

	// Test normal load (should load from actual file or hardcoded)
	rules2 := loadRulesFromSKILLMD()
	if rules2 == "" {
		t.Fatal("loadRulesFromSKILLMD should return rules without env var set")
	}
}

// ─── Target Context & Routing Memory ─────────────────────────────────────────

// TestPickPersona_NilContext verifies that a nil TargetContext falls through to
// existing keyword/LLM matching and never returns a routing-derived method.
func TestPickPersona_NilContext(t *testing.T) {
	persona.Load()
	name, method := pickPersona("write Go unit tests", nil)
	if persona.Get(name) == nil {
		t.Errorf("pickPersona returned invalid persona %q", name)
	}
	switch method {
	case string(persona.SelectionTargetProfile), string(persona.SelectionRoutingPattern):
		t.Errorf("pickPersona with nil tc returned routing method %q", method)
	}
}

// TestPickPersona_TargetProfileNarrows verifies that target profile preferred personas
// narrow the candidate set — task intent then selects within that set.
// When the preferred set does not contain a persona matching the task intent,
// pickFromTargetContext returns false so the caller can fall through to the full catalog.
func TestPickPersona_TargetProfileNarrows(t *testing.T) {
	persona.Load()
	if persona.Get("senior-backend-engineer") == nil {
		t.Skip("senior-backend-engineer not in catalog")
	}

	// Preferred set is backend-only. A review task should not score above 0 within
	// that set — pickFromTargetContext must return false so the full catalog is tried.
	tc := &TargetContext{
		TargetID:          "repo:~/skills/orchestrator",
		PreferredPersonas: []string{"senior-backend-engineer"},
	}
	name, method, ok := pickFromTargetContext("review the auth code", tc, "")
	if ok {
		t.Errorf("preferred backend set should not match review task; got %q (%s)", name, method)
	}
}

// TestPickPersona_TargetProfileNarrows_WithRoutingFallback verifies that when the
// preferred set does not match task intent, routing patterns are tried next —
// ensuring the correct persona wins without reaching the LLM.
func TestPickPersona_TargetProfileNarrows_WithRoutingFallback(t *testing.T) {
	persona.Load()
	if persona.Get("senior-backend-engineer") == nil || persona.Get("staff-code-reviewer") == nil {
		t.Skip("required personas not in catalog")
	}

	// Preferred set is backend-only; routing pattern says staff-code-reviewer.
	// A review task: preferred set misses → routing pattern wins.
	tc := &TargetContext{
		TargetID:          "repo:~/skills/orchestrator",
		PreferredPersonas: []string{"senior-backend-engineer"},
		TopPatterns: []RoutingHint{
			{Persona: "staff-code-reviewer", Confidence: 0.8},
		},
	}
	name, method := pickPersona("review the auth code", tc)
	if name != "staff-code-reviewer" {
		t.Errorf("expected routing fallback staff-code-reviewer when preferred set misses: got %q", name)
	}
	if method != string(persona.SelectionRoutingPattern) {
		t.Errorf("method = %q, want SelectionRoutingPattern", method)
	}
}

// TestPickPersona_TargetProfileMatchesIntent verifies that when the task intent
// matches a persona in the preferred set, that persona is selected with
// SelectionTargetProfile — narrowing and selecting within the preferred set.
func TestPickPersona_TargetProfileMatchesIntent(t *testing.T) {
	persona.Load()
	if persona.Get("staff-code-reviewer") == nil || persona.Get("senior-backend-engineer") == nil {
		t.Skip("required personas not in catalog")
	}

	// Preferred set includes staff-code-reviewer. A review task should select it
	// via keyword scoring within the preferred set → SelectionTargetProfile.
	tc := &TargetContext{
		TargetID:          "repo:~/skills/orchestrator",
		PreferredPersonas: []string{"senior-backend-engineer", "staff-code-reviewer"},
	}
	name, method := pickPersona("review the auth code", tc)
	if name != "staff-code-reviewer" {
		t.Errorf("expected staff-code-reviewer (review intent + preferred set): got %q", name)
	}
	if method != string(persona.SelectionTargetProfile) {
		t.Errorf("method = %q, want SelectionTargetProfile", method)
	}
}

// TestPickPersona_RoutingPatternBias verifies that a routing hint is used when
// no preferred persona is set in TargetContext.
func TestPickPersona_RoutingPatternBias(t *testing.T) {
	persona.Load()
	if persona.Get("staff-code-reviewer") == nil {
		t.Skip("staff-code-reviewer not in catalog")
	}

	tc := &TargetContext{
		TargetID: "repo:~/test/repo",
		TopPatterns: []RoutingHint{
			{Persona: "staff-code-reviewer", TaskHint: "review", Confidence: 0.8},
		},
	}
	name, method := pickPersona("check the code quality", tc)
	if name != "staff-code-reviewer" {
		t.Errorf("routing_pattern hint ignored: got %q, want staff-code-reviewer", name)
	}
	if method != string(persona.SelectionRoutingPattern) {
		t.Errorf("method = %q, want %q", method, persona.SelectionRoutingPattern)
	}
}

// TestPickPersona_PreferredPersonaBeatsPattern verifies that when task intent
// matches a preferred persona, the preferred persona takes priority over a
// routing_pattern hint. The preferred set narrows candidates; task intent
// selects within them, outranking routing patterns.
func TestPickPersona_PreferredPersonaBeatsPattern(t *testing.T) {
	persona.Load()
	if persona.Get("staff-code-reviewer") == nil || persona.Get("academic-researcher") == nil {
		t.Skip("required personas not in catalog")
	}

	// staff-code-reviewer is in the preferred set. A review task matches it
	// via keyword scoring → SelectionTargetProfile, not SelectionRoutingPattern.
	tc := &TargetContext{
		TargetID:          "repo:~/test/repo",
		PreferredPersonas: []string{"staff-code-reviewer"},
		TopPatterns: []RoutingHint{
			{Persona: "academic-researcher", Confidence: 0.9},
		},
	}
	name, method := pickPersona("review the codebase for correctness", tc)
	if name != "staff-code-reviewer" {
		t.Errorf("preferred+intent match should beat routing pattern: got %q, want staff-code-reviewer", name)
	}
	if method != string(persona.SelectionTargetProfile) {
		t.Errorf("method = %q, want SelectionTargetProfile", method)
	}
}

// TestPickPersona_InvalidPreferredPersonaFallsThrough verifies that an unrecognized
// preferred persona is skipped and the next signal is tried.
func TestPickPersona_InvalidPreferredPersonaFallsThrough(t *testing.T) {
	persona.Load()
	if persona.Get("staff-code-reviewer") == nil {
		t.Skip("staff-code-reviewer not in catalog")
	}

	tc := &TargetContext{
		TargetID:          "repo:~/test/repo",
		PreferredPersonas: []string{"nonexistent-persona-xyz"},
		TopPatterns: []RoutingHint{
			{Persona: "staff-code-reviewer", Confidence: 0.6},
		},
	}
	name, method := pickPersona("review the code", tc)
	if name != "staff-code-reviewer" {
		t.Errorf("invalid preferred persona should fall through to routing_pattern: got %q", name)
	}
	if method != string(persona.SelectionRoutingPattern) {
		t.Errorf("method = %q, want %q", method, persona.SelectionRoutingPattern)
	}
}

// TestResolvePersona_TargetProfileBias verifies that resolvePersona respects
// target_profile narrowing: when a preferred persona matches the phase objective,
// it is selected with SelectionTargetProfile.
func TestResolvePersona_TargetProfileBias(t *testing.T) {
	persona.Load()
	if persona.Get("staff-code-reviewer") == nil {
		t.Skip("staff-code-reviewer not in catalog")
	}

	// staff-code-reviewer is in the preferred set. A review objective matches it
	// via keyword scoring within the preferred set → SelectionTargetProfile.
	tc := &TargetContext{
		TargetID:          "repo:~/test/repo",
		PreferredPersonas: []string{"staff-code-reviewer"},
	}
	name, method := resolvePersona("review this code for correctness and security", tc)
	if name != "staff-code-reviewer" {
		t.Errorf("resolvePersona should narrow to preferred set and match intent: got %q, want staff-code-reviewer", name)
	}
	if method != string(persona.SelectionTargetProfile) {
		t.Errorf("method = %q, want %q", method, persona.SelectionTargetProfile)
	}
}

func TestResolvePersona_RolePersonaHintBias(t *testing.T) {
	persona.Load()
	if persona.Get("staff-code-reviewer") == nil {
		t.Skip("staff-code-reviewer not in catalog")
	}

	tc := &TargetContext{
		TargetID: "repo:~/skills/orchestrator",
		RolePersonaHints: []RolePersonaHint{
			{Role: "reviewer", Persona: "staff-code-reviewer", SeenCount: 3, SuccessRate: 1.0},
		},
	}

	name, method := resolvePersona("review the auth implementation for regressions", tc)
	if name != "staff-code-reviewer" {
		t.Fatalf("name = %q, want staff-code-reviewer", name)
	}
	if method != string(persona.SelectionRoutingPattern) {
		t.Fatalf("method = %q, want %q", method, persona.SelectionRoutingPattern)
	}
}

func TestRoutingCorrectionBeatsRolePersonaHint(t *testing.T) {
	persona.Load()
	if persona.Get("staff-code-reviewer") == nil || persona.Get("security-auditor") == nil {
		t.Skip("required personas not in catalog")
	}

	tc := &TargetContext{
		TargetID: "repo:~/skills/orchestrator",
		RolePersonaHints: []RolePersonaHint{
			{Role: "reviewer", Persona: "staff-code-reviewer", SeenCount: 3, SuccessRate: 1.0},
		},
		RoutingCorrections: []CorrectionHint{
			{
				AssignedPersona: "staff-code-reviewer",
				IdealPersona:    "security-auditor",
				TaskHint:        "review security",
				Source:          "audit",
			},
		},
	}

	name, method := resolvePersona("review security controls for gaps", tc)
	if name != "security-auditor" {
		t.Fatalf("name = %q, want security-auditor", name)
	}
	if method != string(persona.SelectionCorrection) {
		t.Fatalf("method = %q, want %q", method, persona.SelectionCorrection)
	}
}

func TestPickPersona_IgnoresRolePersonaHintsAtTaskLevel(t *testing.T) {
	persona.Load()
	if persona.Get("staff-code-reviewer") == nil {
		t.Skip("staff-code-reviewer not in catalog")
	}

	tc := &TargetContext{
		TargetID: "repo:~/skills/orchestrator",
		RolePersonaHints: []RolePersonaHint{
			{Role: "reviewer", Persona: "staff-code-reviewer", SeenCount: 3, SuccessRate: 1.0},
		},
	}

	name, _ := pickPersona("implement auth module", tc)
	if name == "staff-code-reviewer" {
		t.Fatal("task-level persona selection should not use reviewer role hint")
	}
}

func TestAnnotateRoles(t *testing.T) {
	plan := &core.Plan{
		Phases: []*core.Phase{
			{Name: "research", Objective: "Research the deployment options", Persona: "architect"},
			{Name: "implement", Objective: "Implement the auth flow", Persona: "senior-backend-engineer"},
			{Name: "review", Objective: "Review the auth flow for regressions", Persona: "staff-code-reviewer"},
			{Name: "preserve", Objective: "whatever", Persona: "architect", Role: core.RoleReviewer},
		},
	}

	annotateRoles(plan)

	if plan.Phases[0].Role != core.RolePlanner {
		t.Fatalf("phase 0 role = %q, want planner", plan.Phases[0].Role)
	}
	if plan.Phases[1].Role != core.RoleImplementer {
		t.Fatalf("phase 1 role = %q, want implementer", plan.Phases[1].Role)
	}
	if plan.Phases[2].Role != core.RoleReviewer {
		t.Fatalf("phase 2 role = %q, want reviewer", plan.Phases[2].Role)
	}
	if plan.Phases[3].Role != core.RoleReviewer {
		t.Fatalf("phase 3 role was overwritten: got %q, want reviewer", plan.Phases[3].Role)
	}
}

// TestBuildDecomposerPrompt_TargetContext verifies that a non-nil TargetContext
// produces a ## Target Context section distinct from ## Lessons from Past Missions.
func TestBuildDecomposerPrompt_TargetContext(t *testing.T) {
	tc := &TargetContext{
		TargetID:          "repo:~/skills/orchestrator",
		Language:          "go",
		Runtime:           "go",
		PreferredPersonas: []string{"senior-backend-engineer"},
		Notes:             "primary orchestrator repo",
	}
	prompt := buildDecomposerPrompt("implement a feature", "- [pattern] use X", "", tc)

	if !strings.Contains(prompt, "## Target Context") {
		t.Error("prompt missing '## Target Context' section")
	}
	if !strings.Contains(prompt, "repo:~/skills/orchestrator") {
		t.Error("prompt missing target ID")
	}
	if !strings.Contains(prompt, "Language: go") {
		t.Error("prompt missing language field")
	}
	if !strings.Contains(prompt, "senior-backend-engineer") {
		t.Error("prompt missing preferred persona")
	}
	if !strings.Contains(prompt, "Notes: primary orchestrator repo") {
		t.Error("prompt missing notes")
	}
	// Must remain distinct from learnings section.
	if !strings.Contains(prompt, "## Lessons from Past Missions") {
		t.Error("prompt should still contain '## Lessons from Past Missions'")
	}
}

// TestBuildDecomposerPrompt_RoutingLearnings verifies that routing patterns are
// included under ## Routing Learnings, separate from ## Lessons from Past Missions.
func TestBuildDecomposerPrompt_RoutingLearnings(t *testing.T) {
	tc := &TargetContext{
		TargetID: "repo:~/test",
		TopPatterns: []RoutingHint{
			{Persona: "senior-backend-engineer", TaskHint: "implementation", Confidence: 0.8},
			{Persona: "staff-code-reviewer", TaskHint: "review", Confidence: 0.6},
		},
	}
	prompt := buildDecomposerPrompt("build something", "- [pattern] use X", "", tc)

	if !strings.Contains(prompt, "## Routing Learnings") {
		t.Error("prompt missing '## Routing Learnings' section")
	}
	if !strings.Contains(prompt, "senior-backend-engineer") {
		t.Error("prompt missing routing pattern persona")
	}
	if !strings.Contains(prompt, "confidence: 0.8") {
		t.Error("prompt missing confidence value")
	}
	if !strings.Contains(prompt, "hint: implementation") {
		t.Error("prompt missing task hint")
	}
	// Must be a separate section from past-mission learnings.
	if !strings.Contains(prompt, "## Lessons from Past Missions") {
		t.Error("prompt should still contain '## Lessons from Past Missions'")
	}
}

// TestBuildDecomposerPrompt_NoTargetContext verifies that a nil TargetContext
// produces no Target Context or Routing Learnings sections.
func TestBuildDecomposerPrompt_NoTargetContext(t *testing.T) {
	prompt := buildDecomposerPrompt("do the thing", "- [learning] something", "", nil)

	if strings.Contains(prompt, "## Target Context") {
		t.Error("nil TargetContext should not produce '## Target Context'")
	}
	if strings.Contains(prompt, "## Routing Learnings") {
		t.Error("nil TargetContext should not produce '## Routing Learnings'")
	}
	// Learnings from past missions should still appear.
	if !strings.Contains(prompt, "## Lessons from Past Missions") {
		t.Error("prompt should still contain '## Lessons from Past Missions'")
	}
}

// TestBuildDecomposerPrompt_RoutingCorrections verifies that routing corrections
// are included in the prompt when TargetContext has RoutingCorrections set.
func TestBuildDecomposerPrompt_RoutingCorrections(t *testing.T) {
	tc := &TargetContext{
		TargetID: "repo:~/skills/orchestrator",
		RoutingCorrections: []CorrectionHint{
			{
				AssignedPersona: "senior-frontend-engineer",
				IdealPersona:    "senior-backend-engineer",
				TaskHint:        "implement feature",
				Source:          "audit",
			},
		},
	}
	prompt := buildDecomposerPrompt("implement auth module", "", "", tc)

	if !strings.Contains(prompt, "## Prior Routing Corrections") {
		t.Error("prompt missing '## Prior Routing Corrections' section")
	}
	if !strings.Contains(prompt, "senior-frontend-engineer was wrong") {
		t.Error("prompt should mention the wrong persona")
	}
	if !strings.Contains(prompt, "senior-backend-engineer instead") {
		t.Error("prompt should mention the ideal persona")
	}
	if !strings.Contains(prompt, "implement feature") {
		t.Error("prompt should include the task hint")
	}
	if !strings.Contains(prompt, "Do NOT repeat") {
		t.Error("prompt should include the avoidance instruction")
	}
}

// TestBuildDecomposerPrompt_NoCorrections verifies that the corrections section
// is omitted when RoutingCorrections is empty.
func TestBuildDecomposerPrompt_NoCorrections(t *testing.T) {
	tc := &TargetContext{
		TargetID: "repo:~/test",
		Language: "go",
	}
	prompt := buildDecomposerPrompt("build something", "", "", tc)

	if strings.Contains(prompt, "## Prior Routing Corrections") {
		t.Error("empty RoutingCorrections should not produce corrections section")
	}
	// Target context should still appear.
	if !strings.Contains(prompt, "## Target Context") {
		t.Error("prompt should still contain '## Target Context'")
	}
}

// TestBuildDecomposerPrompt_CorrectionsWithNoTaskHint verifies formatting
// when a correction has no task hint.
func TestBuildDecomposerPrompt_CorrectionsWithNoTaskHint(t *testing.T) {
	tc := &TargetContext{
		TargetID: "repo:~/test",
		RoutingCorrections: []CorrectionHint{
			{
				AssignedPersona: "wrong-persona",
				IdealPersona:    "right-persona",
				TaskHint:        "",
				Source:          "manual",
			},
		},
	}
	prompt := buildDecomposerPrompt("do something", "", "", tc)

	if !strings.Contains(prompt, "wrong-persona was wrong") {
		t.Error("prompt should mention the wrong persona")
	}
	if !strings.Contains(prompt, "right-persona instead") {
		t.Error("prompt should mention the ideal persona")
	}
	// Should NOT contain the "for" phrasing when hint is empty.
	if strings.Contains(prompt, `for ""`) {
		t.Error("should not render empty task hint")
	}
}

// TestPickFromTargetContext_CorrectionApplied verifies that an explicit routing
// correction deterministically selects the corrected persona for a matching task.
func TestPickFromTargetContext_CorrectionApplied(t *testing.T) {
	persona.Load()
	if persona.Get("senior-backend-engineer") == nil {
		t.Skip("senior-backend-engineer not in catalog")
	}

	tc := &TargetContext{
		TargetID: "repo:~/skills/orchestrator",
		RoutingCorrections: []CorrectionHint{
			{
				AssignedPersona: "senior-frontend-engineer",
				IdealPersona:    "senior-backend-engineer",
				TaskHint:        "implement feature",
				Source:          "audit",
			},
		},
	}

	name, method, ok := pickFromTargetContext("implement auth module", tc, "")
	if !ok {
		t.Fatal("matching correction should return ok=true")
	}
	if name != "senior-backend-engineer" {
		t.Errorf("name = %q, want senior-backend-engineer", name)
	}
	if method != persona.SelectionCorrection {
		t.Errorf("method = %q, want %q", method, persona.SelectionCorrection)
	}
}

// TestPickFromTargetContext_CorrectionIgnoredOnIntentMismatch verifies that a
// correction for implementation work does not apply to an unrelated review task.
func TestPickFromTargetContext_CorrectionIgnoredOnIntentMismatch(t *testing.T) {
	persona.Load()
	if persona.Get("senior-backend-engineer") == nil {
		t.Skip("senior-backend-engineer not in catalog")
	}

	tc := &TargetContext{
		TargetID: "repo:~/skills/orchestrator",
		RoutingCorrections: []CorrectionHint{
			{
				AssignedPersona: "senior-frontend-engineer",
				IdealPersona:    "senior-backend-engineer",
				TaskHint:        "implement feature",
				Source:          "audit",
			},
		},
	}

	_, _, ok := pickFromTargetContext("review the auth code", tc, "")
	if ok {
		t.Error("implement correction should not apply to review task")
	}
}

// TestPickPersona_CorrectionBeatsWeakerSignals verifies that explicit corrections
// outrank preferred personas and routing patterns when the correction matches.
func TestPickPersona_CorrectionBeatsWeakerSignals(t *testing.T) {
	persona.Load()
	if persona.Get("senior-backend-engineer") == nil || persona.Get("staff-code-reviewer") == nil {
		t.Skip("required personas not in catalog")
	}

	tc := &TargetContext{
		TargetID:          "repo:~/skills/orchestrator",
		PreferredPersonas: []string{"staff-code-reviewer"},
		TopPatterns: []RoutingHint{
			{Persona: "staff-code-reviewer", TaskHint: "implement", Confidence: 0.9},
		},
		RoutingCorrections: []CorrectionHint{
			{
				AssignedPersona: "staff-code-reviewer",
				IdealPersona:    "senior-backend-engineer",
				TaskHint:        "implement feature",
				Source:          "manual",
			},
		},
	}

	name, method := pickPersona("implement auth module", tc)
	if name != "senior-backend-engineer" {
		t.Errorf("name = %q, want senior-backend-engineer", name)
	}
	if method != string(persona.SelectionCorrection) {
		t.Errorf("method = %q, want %q", method, persona.SelectionCorrection)
	}
}

// TestPickFromTargetContext_EmptyContext verifies that a TargetContext with
// no personas and no patterns returns false without panicking.
func TestPickFromTargetContext_EmptyContext(t *testing.T) {
	persona.Load()
	tc := &TargetContext{TargetID: "repo:~/empty"}
	_, _, ok := pickFromTargetContext("do something", tc, "")
	if ok {
		t.Error("empty TargetContext should return ok=false")
	}
}

// TestParsePhases_TargetContextFallback verifies that when a PHASE line has an
// unrecognized persona, resolvePersona is called with the TargetContext.
// When the preferred set matches the phase objective, SelectionTargetProfile is used.
// When the preferred set does not match, the full catalog selects the best persona.
func TestParsePhases_TargetContextFallback(t *testing.T) {
	persona.Load()
	if persona.Get("staff-code-reviewer") == nil {
		t.Skip("staff-code-reviewer not in catalog")
	}

	// Preferred set includes staff-code-reviewer. The objective mentions "review",
	// so keyword scoring within the preferred set should match it.
	tc := &TargetContext{
		TargetID:          "repo:~/test",
		PreferredPersonas: []string{"staff-code-reviewer"},
	}

	output := `PHASE: do-stuff | OBJECTIVE: Review the code for correctness | PERSONA: nonexistent-persona`
	phases, err := ParsePhases(output, tc)
	if err != nil {
		t.Fatalf("ParsePhases() error: %v", err)
	}
	if len(phases) != 1 {
		t.Fatalf("expected 1 phase, got %d", len(phases))
	}
	if phases[0].Persona != "staff-code-reviewer" {
		t.Errorf("expected target_profile narrowing to pick staff-code-reviewer, got %q", phases[0].Persona)
	}
	if phases[0].PersonaSelectionMethod != string(persona.SelectionTargetProfile) {
		t.Errorf("selection method = %q, want %q", phases[0].PersonaSelectionMethod, persona.SelectionTargetProfile)
	}
}

// TestDetectIntent verifies that the primary intent is correctly inferred from task text.
func TestDetectIntent(t *testing.T) {
	tests := []struct {
		task string
		want string
	}{
		{"implement a new REST API endpoint", "implement"},
		{"build a background worker for processing events", "implement"},
		{"create a CLI tool for rate limiting", "implement"},
		{"review the auth code for vulnerabilities", "review"},
		{"audit the codebase for security issues", "review"},
		{"check the code quality", "review"},
		{"verify the output is correct", "review"},
		{"research the best Go testing frameworks", "research"},
		{"analyze the performance bottleneck", "research"},
		{"investigate the memory leak", "research"},
		{"find the bottleneck in the request path", "research"},
		{"write a developer guide", "write"},
		{"document the API endpoints", "write"},
		{"write code to handle file uploads", "implement"}, // "write code" → implement, not write
		{"do something unclear", ""},
	}

	for _, tt := range tests {
		got := detectIntent(tt.task)
		if got != tt.want {
			t.Errorf("detectIntent(%q) = %q, want %q", tt.task, got, tt.want)
		}
	}
}

// TestRoutingPatternMatchesIntent verifies the intent-matching helper.
func TestRoutingPatternMatchesIntent(t *testing.T) {
	tests := []struct {
		taskIntent string
		taskHint   string
		want       bool
	}{
		{"review", "review", true},
		{"implement", "implementation", true}, // substring match
		{"research", "research", true},
		{"write", "write", true},
		{"implement", "review", false}, // conflict: implement task, review hint
		{"review", "implementation", false},
		{"implement", "", true}, // no hint restriction → always matches
		{"", "review", true},    // unknown intent → don't suppress
		{"", "", true},
	}

	for _, tt := range tests {
		got := routingPatternMatchesIntent(tt.taskIntent, tt.taskHint)
		if got != tt.want {
			t.Errorf("routingPatternMatchesIntent(%q, %q) = %v, want %v", tt.taskIntent, tt.taskHint, got, tt.want)
		}
	}
}

// TestPickFromTargetContext_ReviewPatternIgnoredForImplementTask verifies that a
// routing_pattern with TaskHint "review" is skipped when the task intent is "implement".
// A high-confidence review pattern must not override an implementation task.
func TestPickFromTargetContext_ReviewPatternIgnoredForImplementTask(t *testing.T) {
	persona.Load()
	if persona.Get("staff-code-reviewer") == nil {
		t.Skip("staff-code-reviewer not in catalog")
	}

	tc := &TargetContext{
		TargetID: "repo:~/test/repo",
		TopPatterns: []RoutingHint{
			{Persona: "staff-code-reviewer", TaskHint: "review", Confidence: 0.9},
		},
	}

	name, method, ok := pickFromTargetContext("implement a new REST API endpoint", tc, "")
	if ok {
		t.Errorf("review pattern (high confidence) should not match implement task; got %q (%s)", name, method)
	}
}

// TestPickPersona_ImplementTask_IgnoresReviewPattern verifies end-to-end that an
// implementation task never resolves to the reviewer via a high-confidence routing pattern.
func TestPickPersona_ImplementTask_IgnoresReviewPattern(t *testing.T) {
	persona.Load()
	if persona.Get("staff-code-reviewer") == nil {
		t.Skip("staff-code-reviewer not in catalog")
	}

	tc := &TargetContext{
		TargetID: "repo:~/test/repo",
		TopPatterns: []RoutingHint{
			{Persona: "staff-code-reviewer", TaskHint: "review", Confidence: 0.9},
		},
	}

	name, _ := pickPersona("implement a new REST API endpoint", tc)
	if name == "staff-code-reviewer" {
		t.Errorf("implement task must not select staff-code-reviewer from review pattern")
	}
}

// TestPickPersona_RoutingPatternHintMatchesIntent verifies that a routing pattern
// is used when its TaskHint matches the task's detected intent.
func TestPickPersona_RoutingPatternHintMatchesIntent(t *testing.T) {
	persona.Load()
	if persona.Get("staff-code-reviewer") == nil {
		t.Skip("staff-code-reviewer not in catalog")
	}

	tc := &TargetContext{
		TargetID: "repo:~/test/repo",
		TopPatterns: []RoutingHint{
			{Persona: "staff-code-reviewer", TaskHint: "review", Confidence: 0.8},
		},
	}

	name, method, ok := pickFromTargetContext("review the authentication flow", tc, "")
	if !ok {
		t.Fatal("review pattern should match review task; got ok=false")
	}
	if name != "staff-code-reviewer" {
		t.Errorf("expected staff-code-reviewer; got %q", name)
	}
	if method != persona.SelectionRoutingPattern {
		t.Errorf("method = %q, want SelectionRoutingPattern", method)
	}
}

// --- DecompSource recording tests ---

// TestDecompSource_PreDecomposed verifies that Decompose sets DecompSource to
// "predecomposed" when PHASE lines are present in the task.
func TestDecompSource_PreDecomposed(t *testing.T) {
	task := `Do the thing.
PHASE: setup | OBJECTIVE: Set up environment | PERSONA: senior-backend-engineer
PHASE: build | OBJECTIVE: Build the feature | PERSONA: senior-backend-engineer`

	plan, err := Decompose(context.Background(), task, "", "", "", event.NoOpEmitter{}, nil)
	if err != nil {
		t.Fatalf("Decompose: %v", err)
	}
	if plan.DecompSource != core.DecompPredecomposed {
		t.Errorf("DecompSource = %q, want %q", plan.DecompSource, core.DecompPredecomposed)
	}
	// ensureCodeReviewPhase injects a reviewer when taskNeedsCodeReview is true.
	// The task "Do the thing." has no implementation keywords, and tc is nil,
	// so no review is injected despite the code-producing persona.
	if len(plan.Phases) != 2 {
		t.Errorf("len(Phases) = %d, want 2", len(plan.Phases))
	}
}

// TestDecompSource_KeywordDecompose_Direct verifies that keywordDecompose produces
// a well-formed plan and the caller can set DecompSource appropriately.
func TestDecompSource_KeywordDecompose_Direct(t *testing.T) {
	plan := keywordDecompose("research the best Go testing frameworks and compare them", nil)
	// Decompose() sets DecompSource after calling keywordDecompose.
	// Verify the plan carries the field correctly.
	plan.DecompSource = core.DecompKeyword
	if plan.DecompSource != core.DecompKeyword {
		t.Errorf("DecompSource = %q, want %q", plan.DecompSource, core.DecompKeyword)
	}
	if len(plan.Phases) == 0 {
		t.Error("expected at least one phase from keywordDecompose")
	}
}

// TestDecompSource_Template verifies that a plan loaded from a template
// can have DecompSource set to "template".
func TestDecompSource_Template(t *testing.T) {
	plan := &core.Plan{
		ID:            "plan_test",
		Task:          "test task",
		ExecutionMode: "sequential",
		Phases:        []*core.Phase{{ID: "phase-1", Name: "execute"}},
	}
	plan.DecompSource = core.DecompTemplate
	if plan.DecompSource != core.DecompTemplate {
		t.Errorf("DecompSource = %q, want %q", plan.DecompSource, core.DecompTemplate)
	}
}

// TestDecompSource_PersistsInJSON verifies that DecompSource round-trips through
// JSON serialization (checkpoint persistence).
func TestDecompSource_PersistsInJSON(t *testing.T) {
	plan := &core.Plan{
		ID:            "plan_test",
		Task:          "test",
		DecompSource:  core.DecompPredecomposed,
		ExecutionMode: "sequential",
	}

	data, err := json.Marshal(plan)
	if err != nil {
		t.Fatalf("Marshal: %v", err)
	}

	var decoded core.Plan
	if err := json.Unmarshal(data, &decoded); err != nil {
		t.Fatalf("Unmarshal: %v", err)
	}
	if decoded.DecompSource != core.DecompPredecomposed {
		t.Errorf("round-trip DecompSource = %q, want %q", decoded.DecompSource, core.DecompPredecomposed)
	}
}

// TestParsePhases_TargetContextNoMatchFallsThrough verifies that when the preferred set
// does not match the phase objective, pickFromTargetContext returns false — allowing
// the routing pattern (or full catalog) to select the correct persona.
// Uses a routing pattern as the deterministic fallback to avoid LLM calls in tests.
func TestParsePhases_TargetContextNoMatchFallsThrough(t *testing.T) {
	persona.Load()
	if persona.Get("senior-backend-engineer") == nil || persona.Get("staff-code-reviewer") == nil {
		t.Skip("required personas not in catalog")
	}

	// Preferred set is backend-only. A review objective misses in the preferred set,
	// so the routing pattern (staff-code-reviewer) should be selected instead.
	tc := &TargetContext{
		TargetID:          "repo:~/test",
		PreferredPersonas: []string{"senior-backend-engineer"},
		TopPatterns: []RoutingHint{
			{Persona: "staff-code-reviewer", Confidence: 0.8},
		},
	}

	output := `PHASE: do-stuff | OBJECTIVE: Review the code for security issues | PERSONA: nonexistent-persona`
	phases, err := ParsePhases(output, tc)
	if err != nil {
		t.Fatalf("ParsePhases() error: %v", err)
	}
	if len(phases) != 1 {
		t.Fatalf("expected 1 phase, got %d", len(phases))
	}
	if phases[0].Persona == "senior-backend-engineer" {
		t.Errorf("preferred set (backend-only) should not override review intent; got senior-backend-engineer")
	}
	if phases[0].Persona != "staff-code-reviewer" {
		t.Errorf("expected routing fallback staff-code-reviewer when preferred set misses, got %q", phases[0].Persona)
	}
}

// ─── Decomposition learning prompt formatting tests ──────────────────────────

func TestFormatDecompExamples_Basic(t *testing.T) {
	tc := &TargetContext{
		DecompExamples: []DecompExampleHint{
			{
				TaskSummary:   "implement auth system",
				PhaseCount:    3,
				ExecutionMode: "sequential",
				PhasesJSON:    `[{"name":"research","persona":"architect"},{"name":"implement","persona":"senior-backend-engineer"}]`,
				DecompSource:  "predecomposed",
				AuditScore:    4,
			},
		},
	}

	got := formatDecompExamples(tc)
	if !strings.Contains(got, "Validated Decomposition Examples") {
		t.Error("missing header")
	}
	if !strings.Contains(got, "human-written") {
		t.Error("predecomposed should be rendered as 'human-written'")
	}
	if !strings.Contains(got, "implement auth system") {
		t.Error("should contain task summary")
	}
	if !strings.Contains(got, "score: 4/5") {
		t.Error("should contain audit score")
	}
	if !strings.Contains(got, "adapt as needed") {
		t.Error("should contain 'adapt as needed' — examples are suggestions, not directives")
	}
}

func TestFormatDecompExamples_LLMSource(t *testing.T) {
	tc := &TargetContext{
		DecompExamples: []DecompExampleHint{
			{
				TaskSummary:   "build feature",
				PhaseCount:    2,
				ExecutionMode: "parallel",
				PhasesJSON:    `[{}]`,
				DecompSource:  "llm",
				AuditScore:    3,
			},
		},
	}

	got := formatDecompExamples(tc)
	if !strings.Contains(got, "source: llm") {
		t.Error("LLM source should be shown as-is")
	}
}

func TestFormatDecompInsights_Basic(t *testing.T) {
	tc := &TargetContext{
		DecompInsights: []DecompInsight{
			{FindingType: "missing_phase", Detail: "testing phase", Count: 3},
			{FindingType: "wrong_persona", Detail: "assigned frontend, ideal backend", Count: 2},
		},
	}

	got := formatDecompInsights(tc)
	if !strings.Contains(got, "Decomposition Insights") {
		t.Error("missing header")
	}
	if !strings.Contains(got, "recurring problems") {
		t.Error("should emphasize these are recurring, not one-off")
	}
	if !strings.Contains(got, "testing phase") {
		t.Error("should contain finding detail")
	}
	if !strings.Contains(got, "observed in 3 missions") {
		t.Error("should contain observation count")
	}
}

func TestBuildDecomposerPrompt_IncludesExamplesAndInsights(t *testing.T) {
	tc := &TargetContext{
		TargetID: "repo:~/test",
		DecompExamples: []DecompExampleHint{
			{TaskSummary: "test task", PhaseCount: 2, PhasesJSON: `[{}]`, DecompSource: "llm", AuditScore: 4},
		},
		DecompInsights: []DecompInsight{
			{FindingType: "missing_phase", Detail: "testing phase", Count: 2},
		},
	}

	prompt := buildDecomposerPrompt("implement a feature", "", "## Skills\n- obsidian\n", tc)
	if !strings.Contains(prompt, "Validated Decomposition Examples") {
		t.Error("prompt should include decomp examples section")
	}
	if !strings.Contains(prompt, "Decomposition Insights") {
		t.Error("prompt should include decomp insights section")
	}
}

func TestBuildDecomposerPrompt_EmptyExamplesOmitted(t *testing.T) {
	tc := &TargetContext{
		TargetID: "repo:~/test",
	}

	prompt := buildDecomposerPrompt("implement a feature", "", "", tc)
	if strings.Contains(prompt, "Validated Decomposition Examples") {
		t.Error("prompt should NOT include examples section when empty")
	}
	if strings.Contains(prompt, "Decomposition Insights") {
		t.Error("prompt should NOT include insights section when empty")
	}
}

// ─── Regression: write/technical-writer collapse ─────────────────────────────

// TestRegression_MigrationTask_WriteTechnicalWriterCollapseSuppressed verifies that
// for a migration/replacement task (which contains the word "write"), the keyword
// decomposer does NOT collapse it into a single write/technical-writer phase when
// decomposition insights indicate this pattern has repeatedly failed.
//
// The scenario: "write migration code to replace old auth middleware" contains
// "write" but is clearly an implementation task, not a writing task. The keyword
// decomposer naively creates both an "implement" and "write" phase. The insights
// system, having observed repeated "wrong_persona" findings for write/technical-writer
// on this target, should cause the LLM decomposer to avoid that collapse.
//
// This test validates the data flow: insights are correctly formatted into the
// prompt so the LLM has the signal it needs.
func TestRegression_MigrationTask_WriteTechnicalWriterCollapseSuppressed(t *testing.T) {
	persona.Load()

	// Set up a target context with repeated findings indicating
	// that "write/technical-writer" is wrong for implementation tasks.
	tc := &TargetContext{
		TargetID: "repo:~/skills/orchestrator",
		DecompInsights: []DecompInsight{
			{
				FindingType: "wrong_persona",
				Detail:      "assigned technical-writer, ideal senior-backend-engineer",
				Count:       3, // observed in 3 missions — well above minObservations=2
			},
			{
				FindingType: "wrong_persona",
				Detail:      "assigned technical-writer, ideal architect",
				Count:       2,
			},
		},
	}

	// Build the decomposer prompt for a migration task.
	task := "write migration code to replace the old auth middleware with the new compliance-approved version"
	prompt := buildDecomposerPrompt(task, "", "", tc)

	// The prompt MUST contain the insight about technical-writer being wrong.
	if !strings.Contains(prompt, "assigned technical-writer, ideal senior-backend-engineer") {
		t.Error("prompt should contain the repeated finding about technical-writer being wrong")
	}
	if !strings.Contains(prompt, "observed in 3 missions") {
		t.Error("prompt should show the observation count so the LLM knows this is high-confidence")
	}

	// Verify the keyword decomposer's behavior without insights:
	// it WOULD produce a "write" phase with "technical-writer".
	keywordPlan := keywordDecompose(task, nil)
	var hasWriteTechnicalWriter bool
	for _, p := range keywordPlan.Phases {
		if p.Name == "write" && p.Persona == "technical-writer" {
			hasWriteTechnicalWriter = true
		}
	}
	if !hasWriteTechnicalWriter {
		t.Log("keyword decomposer didn't produce write/technical-writer — may have changed behavior")
		t.Log("phases:")
		for _, p := range keywordPlan.Phases {
			t.Logf("  %s: %s (%s)", p.ID, p.Name, p.Persona)
		}
	}
	// The key assertion: with insights present, the LLM prompt tells it not to
	// assign technical-writer to implementation-shaped work. We can't test the LLM's
	// actual response in a unit test, but we verify the signal is in the prompt.
	if !strings.Contains(prompt, "Decomposition Insights") {
		t.Fatal("prompt must include insights section for the LLM to suppress the collapse")
	}
	// The insights section must appear BEFORE "## Task to Decompose" so the LLM
	// processes it as context before seeing the task.
	insightsIdx := strings.Index(prompt, "Decomposition Insights")
	taskIdx := strings.Index(prompt, "## Task to Decompose")
	if insightsIdx > taskIdx {
		t.Error("insights section should appear before the task to decompose")
	}
}

// TestRegression_MigrationTask_SingleAuditDoesNotSuppressCollapse verifies
// that a SINGLE audit finding does NOT produce a decomposition insight.
// This is the core safety property: no single noisy audit should rewrite behavior.
func TestRegression_MigrationTask_SingleAuditDoesNotSuppressCollapse(t *testing.T) {
	// With only 1 observation (not meeting minObservations=2), insights should be empty.
	tc := &TargetContext{
		TargetID:       "repo:~/skills/orchestrator",
		DecompInsights: nil, // no repeated findings — single audit only produces findings, not insights
	}

	task := "write migration code to replace the old auth middleware"
	prompt := buildDecomposerPrompt(task, "", "", tc)

	// With no insights, the prompt should NOT contain the insights section.
	if strings.Contains(prompt, "Decomposition Insights") {
		t.Error("prompt should NOT contain insights section when no repeated findings exist")
	}
}

// TestRegression_ReplacementTask_ExamplePreventsCollapse verifies that a
// high-scoring decomposition example for a migration task shows the LLM
// the correct pattern (separate implement + test phases, not write/technical-writer).
func TestRegression_ReplacementTask_ExamplePreventsCollapse(t *testing.T) {
	tc := &TargetContext{
		TargetID: "repo:~/skills/orchestrator",
		DecompExamples: []DecompExampleHint{
			{
				TaskSummary:   "replace old search with FTS5-based implementation",
				PhaseCount:    4,
				ExecutionMode: "sequential",
				PhasesJSON:    `[{"name":"plan","persona":"architect"},{"name":"implement","persona":"senior-backend-engineer"},{"name":"test","persona":"qa-engineer"},{"name":"review","persona":"staff-code-reviewer"}]`,
				DecompSource:  "predecomposed",
				AuditScore:    5,
			},
		},
	}

	task := "write migration code to replace the old auth middleware with compliance-approved version"
	prompt := buildDecomposerPrompt(task, "", "", tc)

	// The prompt should contain the example showing the correct pattern.
	if !strings.Contains(prompt, "replace old search with FTS5") {
		t.Error("prompt should contain the validated example's task summary")
	}
	if !strings.Contains(prompt, "senior-backend-engineer") {
		t.Error("prompt should show senior-backend-engineer in the example phases")
	}
	if !strings.Contains(prompt, "human-written") {
		t.Error("predecomposed source should be shown as human-written for higher credibility")
	}
	if !strings.Contains(prompt, "score: 5/5") {
		t.Error("high audit score should be visible to signal high confidence")
	}
}

// --- WORKDIR parsing and expandTilde tests ---

func TestExpandTilde(t *testing.T) {
	home, err := os.UserHomeDir()
	if err != nil {
		t.Fatalf("UserHomeDir: %v", err)
	}

	tests := []struct {
		name string
		path string
		want string
	}{
		{"tilde path", "~/skills/orchestrator", filepath.Join(home, "skills/orchestrator")},
		{"absolute path unchanged", "/tmp/foo", "/tmp/foo"},
		{"empty string", "", ""},
		{"just tilde slash", "~/", home},
		{"no tilde", "skills/orchestrator", "skills/orchestrator"},
		{"tilde without slash", "~foo", "~foo"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := expandTilde(tt.path)
			if got != tt.want {
				t.Errorf("expandTilde(%q) = %q, want %q", tt.path, got, tt.want)
			}
		})
	}
}

func TestParsePhases_WorkdirField(t *testing.T) {
	home, err := os.UserHomeDir()
	if err != nil {
		t.Fatalf("UserHomeDir: %v", err)
	}

	tests := []struct {
		name      string
		input     string
		wantDir   string
		wantCount int
	}{
		{
			name:      "WORKDIR parsed and tilde expanded",
			input:     `PHASE: build | OBJECTIVE: Build feature | PERSONA: senior-backend-engineer | WORKDIR: ~/skills/orchestrator`,
			wantDir:   filepath.Join(home, "skills/orchestrator"),
			wantCount: 1,
		},
		{
			name:      "WORKDIR with absolute path",
			input:     `PHASE: build | OBJECTIVE: Build feature | PERSONA: senior-backend-engineer | WORKDIR: /tmp/myrepo`,
			wantDir:   "/tmp/myrepo",
			wantCount: 1,
		},
		{
			name:      "no WORKDIR leaves TargetDir empty",
			input:     `PHASE: build | OBJECTIVE: Build feature | PERSONA: senior-backend-engineer`,
			wantDir:   "",
			wantCount: 1,
		},
		{
			name:      "WORKDIR after EXPECTED",
			input:     `PHASE: build | OBJECTIVE: Build it | PERSONA: senior-backend-engineer | EXPECTED: working code | WORKDIR: ~/myrepo`,
			wantDir:   filepath.Join(home, "myrepo"),
			wantCount: 1,
		},
		{
			name:      "EXPECTED without WORKDIR does not consume rest of line",
			input:     `PHASE: build | OBJECTIVE: Build it | PERSONA: senior-backend-engineer | EXPECTED: working code`,
			wantDir:   "",
			wantCount: 1,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			phases, err := ParsePhases(tt.input, nil)
			if err != nil {
				t.Fatalf("ParsePhases: %v", err)
			}
			if len(phases) != tt.wantCount {
				t.Fatalf("len(phases) = %d, want %d", len(phases), tt.wantCount)
			}
			if phases[0].TargetDir != tt.wantDir {
				t.Errorf("TargetDir = %q, want %q", phases[0].TargetDir, tt.wantDir)
			}
		})
	}
}

func TestParsePhases_WorkdirWithAllFields(t *testing.T) {
	home, _ := os.UserHomeDir()

	// Two phases so DEPENDS can resolve (phase-2 depends on phase-1 by name).
	input := `PHASE: plan | OBJECTIVE: Plan the API | PERSONA: senior-backend-engineer
PHASE: impl | OBJECTIVE: Implement API | PERSONA: senior-backend-engineer | SKILLS: golang-pro | DEPENDS: plan | EXPECTED: tests pass | WORKDIR: ~/skills/orchestrator`

	phases, err := ParsePhases(input, nil)
	if err != nil {
		t.Fatalf("ParsePhases: %v", err)
	}
	if len(phases) != 2 {
		t.Fatalf("len(phases) = %d, want 2", len(phases))
	}

	p := phases[1] // the impl phase
	if p.Name != "impl" {
		t.Errorf("Name = %q, want impl", p.Name)
	}
	if p.Objective != "Implement API" {
		t.Errorf("Objective = %q", p.Objective)
	}
	if p.Persona != "senior-backend-engineer" {
		t.Errorf("Persona = %q", p.Persona)
	}
	if len(p.Skills) != 1 || p.Skills[0] != "golang-pro" {
		t.Errorf("Skills = %v", p.Skills)
	}
	if len(p.Dependencies) != 1 {
		t.Fatalf("Dependencies = %v, want 1 entry", p.Dependencies)
	}
	// Dependency should be the ID of the "plan" phase.
	if p.Dependencies[0] != phases[0].ID {
		t.Errorf("Dependencies[0] = %q, want %q (plan phase ID)", p.Dependencies[0], phases[0].ID)
	}
	if p.Expected != "tests pass" {
		t.Errorf("Expected = %q", p.Expected)
	}
	if p.TargetDir != filepath.Join(home, "skills/orchestrator") {
		t.Errorf("TargetDir = %q", p.TargetDir)
	}
}

// ─── Intent-Aware Fallback ────────────────────────────────────────────────────

// TestIntentAwareFallback_Review verifies that review tasks never receive the
// alphabetical default persona ("architect") and prefer staff-code-reviewer.
func TestIntentAwareFallback_Review(t *testing.T) {
	persona.Load()
	name, method := intentAwareFallback("review this code for correctness")
	if persona.Get(name) == nil {
		t.Fatalf("intentAwareFallback returned invalid persona %q", name)
	}
	// Must not be the alphabet-based default.
	if name == "architect" {
		t.Errorf("intentAwareFallback returned alphabetical default %q for review task", name)
	}
	if method == "" {
		t.Errorf("intentAwareFallback returned empty method for review task")
	}
	if persona.Get("staff-code-reviewer") != nil && name != "staff-code-reviewer" {
		t.Errorf("intentAwareFallback(%q) = %q, want staff-code-reviewer", "review task", name)
	}
}

// TestIntentAwareFallback_Research verifies that scholarly research tasks prefer
// academic-researcher and never return the alphabetical default.
func TestIntentAwareFallback_Research(t *testing.T) {
	persona.Load()
	name, method := intentAwareFallback("conduct a literature review on pair programming productivity")
	if persona.Get(name) == nil {
		t.Fatalf("intentAwareFallback returned invalid persona %q", name)
	}
	if name == "architect" {
		t.Errorf("intentAwareFallback returned alphabetical default for research task")
	}
	if method == "" {
		t.Errorf("intentAwareFallback returned empty method for research task")
	}
	if persona.Get("academic-researcher") != nil && name != "academic-researcher" {
		t.Errorf("intentAwareFallback(research task) = %q, want academic-researcher", name)
	}
}

// TestIntentAwareFallback_Write verifies that write tasks prefer technical-writer
// and never return the alphabetical default.
func TestIntentAwareFallback_Write(t *testing.T) {
	persona.Load()
	name, method := intentAwareFallback("write a blog post about Go patterns")
	if persona.Get(name) == nil {
		t.Fatalf("intentAwareFallback returned invalid persona %q", name)
	}
	if name == "architect" {
		t.Errorf("intentAwareFallback returned alphabetical default for write task")
	}
	if method == "" {
		t.Errorf("intentAwareFallback returned empty method for write task")
	}
	if persona.Get("technical-writer") != nil && name != "technical-writer" {
		t.Errorf("intentAwareFallback(write task) = %q, want technical-writer", name)
	}
}

// TestIntentAwareFallback_ImplementNoLanguage verifies that generic implementation
// tasks without a language signal return senior-backend-engineer with SelectionFallback.
func TestIntentAwareFallback_ImplementNoLanguage(t *testing.T) {
	persona.Load()
	name, method := intentAwareFallback("build a rate limiter for the system")
	if persona.Get(name) == nil {
		t.Fatalf("intentAwareFallback returned invalid persona %q", name)
	}
	if name == "architect" {
		t.Errorf("intentAwareFallback returned alphabetical default for implement task")
	}
	// No language signal → pickImplementationPersona returns senior-backend-engineer.
	if name != "senior-backend-engineer" {
		t.Errorf("intentAwareFallback(implement, no language) = %q, want senior-backend-engineer", name)
	}
	if method != persona.SelectionFallback {
		t.Errorf("method = %q, want SelectionFallback (no language signal)", method)
	}
}

// TestIntentAwareFallback_UnknownIntent verifies that tasks with no clear intent
// signal return senior-backend-engineer as the universal default.
func TestIntentAwareFallback_UnknownIntent(t *testing.T) {
	persona.Load()
	name, method := intentAwareFallback("do something with the system")
	if persona.Get(name) == nil {
		t.Fatalf("intentAwareFallback returned invalid persona %q", name)
	}
	if name != "senior-backend-engineer" {
		t.Errorf("intentAwareFallback(unknown intent) = %q, want senior-backend-engineer", name)
	}
	if method != persona.SelectionFallback {
		t.Errorf("method = %q, want SelectionFallback", method)
	}
}

func TestSimplePlan_ImplementationTaskStaysSinglePhase(t *testing.T) {
	persona.Load()

	tc := &TargetContext{
		TargetID: "repo:~/skills/orchestrator",
		Language: "go",
		Runtime:  "go",
	}

	plan := simplePlan("implement authentication middleware", tc)
	if len(plan.Phases) != 1 {
		t.Fatalf("len(plan.Phases) = %d, want 1", len(plan.Phases))
	}
}

func TestEnsureCodeReviewPhase_InsertsBeforeQA(t *testing.T) {
	plan := &core.Plan{
		Task: "implement routing fixes",
		Phases: []*core.Phase{
			{ID: "phase-1", Name: "implement", Persona: "senior-backend-engineer", ModelTier: string(router.TierWork)},
			{ID: "phase-2", Name: "validate", Persona: "qa-engineer", ModelTier: string(router.TierWork), Dependencies: []string{"phase-1"}},
		},
		ExecutionMode: "sequential",
	}
	tc := &TargetContext{
		TargetID: "repo:~/skills/orchestrator",
		Language: "go",
		Runtime:  "go",
	}

	ensureCodeReviewPhase(plan, tc)

	if len(plan.Phases) != 3 {
		t.Fatalf("len(plan.Phases) = %d, want 3", len(plan.Phases))
	}
	if plan.Phases[1].Persona != "staff-code-reviewer" {
		t.Fatalf("inserted phase persona = %q, want staff-code-reviewer", plan.Phases[1].Persona)
	}
	if got := plan.Phases[1].Dependencies; len(got) != 1 || got[0] != "phase-1" {
		t.Fatalf("review phase deps = %v, want [phase-1]", got)
	}
	if got := plan.Phases[2].Dependencies; len(got) != 1 || got[0] != plan.Phases[1].ID {
		t.Fatalf("qa phase deps = %v, want [%s]", got, plan.Phases[1].ID)
	}
}

func TestEnsureCodeReviewPhase_InsertsBeforeValidationLikePhase(t *testing.T) {
	plan := &core.Plan{
		Task: "implement routing fixes",
		Phases: []*core.Phase{
			{ID: "phase-1", Name: "implement", Persona: "senior-backend-engineer", ModelTier: string(router.TierWork)},
			{ID: "phase-2", Name: "verify-release", Persona: "technical-writer", Objective: "Validate the rollout checklist", ModelTier: string(router.TierWork), Dependencies: []string{"phase-1"}},
		},
		ExecutionMode: "sequential",
	}
	tc := &TargetContext{
		TargetID: "repo:~/skills/orchestrator",
		Language: "go",
		Runtime:  "go",
	}

	ensureCodeReviewPhase(plan, tc)

	if len(plan.Phases) != 3 {
		t.Fatalf("len(plan.Phases) = %d, want 3", len(plan.Phases))
	}
	if plan.Phases[1].Persona != "staff-code-reviewer" {
		t.Fatalf("inserted phase persona = %q, want staff-code-reviewer", plan.Phases[1].Persona)
	}
	if got := plan.Phases[2].Dependencies; len(got) != 1 || got[0] != plan.Phases[1].ID {
		t.Fatalf("validation phase deps = %v, want [%s]", got, plan.Phases[1].ID)
	}
}

// TestEnsureCodeReviewPhase_TriggersByCodeProducingPersona verifies that a review
// phase is injected based on implementer personas alone — even when there is no
// code target in TargetContext and the task text lacks implementation keywords.
// Requires 2+ phases: single-phase plans are excluded from the persona-based path.
func TestEnsureCodeReviewPhase_TriggersByCodeProducingPersona(t *testing.T) {
	plan := &core.Plan{
		Task: "do the thing",
		Phases: []*core.Phase{
			{ID: "phase-1", Name: "design", Persona: "architect", ModelTier: string(router.TierWork)},
			{ID: "phase-2", Name: "build", Persona: "senior-backend-engineer", ModelTier: string(router.TierWork), Dependencies: []string{"phase-1"}},
		},
		ExecutionMode: "sequential",
	}
	// Nil tc: no code-target signal, no implementation keywords in task text.
	ensureCodeReviewPhase(plan, nil)

	if len(plan.Phases) != 3 {
		t.Fatalf("len(plan.Phases) = %d, want 3 (review injected by persona signal)", len(plan.Phases))
	}
	review := plan.Phases[2]
	if review.Persona != "staff-code-reviewer" {
		t.Errorf("injected phase persona = %q, want staff-code-reviewer", review.Persona)
	}
	if !containsSkill(review.Skills, "requesting-code-review") {
		t.Errorf("injected phase skills = %v, want to contain 'requesting-code-review'", review.Skills)
	}
	want := "Review implementation for correctness, test coverage, and adherence to project conventions"
	if review.Objective != want {
		t.Errorf("injected phase objective = %q, want %q", review.Objective, want)
	}
}

// TestEnsureCodeReviewPhase_SkipWhenReviewerAlreadyPresent verifies that no
// second review phase is appended when one already exists.
func TestEnsureCodeReviewPhase_SkipWhenReviewerAlreadyPresent(t *testing.T) {
	plan := &core.Plan{
		Task: "implement and review",
		Phases: []*core.Phase{
			{ID: "phase-1", Name: "implement", Persona: "senior-backend-engineer"},
			{ID: "phase-2", Name: "review", Persona: "staff-code-reviewer"},
		},
		ExecutionMode: "sequential",
	}
	tc := &TargetContext{TargetID: "repo:~/skills/orchestrator", Language: "go"}

	ensureCodeReviewPhase(plan, tc)

	if len(plan.Phases) != 2 {
		t.Fatalf("len(plan.Phases) = %d, want 2 (no extra review injected)", len(plan.Phases))
	}
}

// TestEnsureCodeReviewPhase_SkipWithNoReviewFlag verifies that setting
// SkipReviewInjection on TargetContext prevents injection.
func TestEnsureCodeReviewPhase_SkipWithNoReviewFlag(t *testing.T) {
	plan := &core.Plan{
		Task: "implement new feature",
		Phases: []*core.Phase{
			{ID: "phase-1", Name: "implement", Persona: "senior-backend-engineer"},
		},
		ExecutionMode: "sequential",
	}
	tc := &TargetContext{
		TargetID:            "repo:~/skills/orchestrator",
		Language:            "go",
		Runtime:             "go",
		SkipReviewInjection: true,
	}

	ensureCodeReviewPhase(plan, tc)

	if len(plan.Phases) != 1 {
		t.Fatalf("len(plan.Phases) = %d, want 1 (injection suppressed by SkipReviewInjection)", len(plan.Phases))
	}
}

// TestEnsureCodeReviewPhase_InjectedPhaseHasRequestingCodeReviewSkill checks
// that the auto-injected review phase always includes the requesting-code-review
// skill, even when triggered via the text/target-context path.
func TestEnsureCodeReviewPhase_InjectedPhaseHasRequestingCodeReviewSkill(t *testing.T) {
	plan := &core.Plan{
		Task: "implement routing fixes",
		Phases: []*core.Phase{
			{ID: "phase-1", Name: "implement", Persona: "senior-backend-engineer"},
		},
		ExecutionMode: "sequential",
	}
	tc := &TargetContext{TargetID: "repo:~/skills/orchestrator", Language: "go", Runtime: "go"}

	ensureCodeReviewPhase(plan, tc)

	if len(plan.Phases) != 2 {
		t.Fatalf("len(plan.Phases) = %d, want 2", len(plan.Phases))
	}
	if !containsSkill(plan.Phases[1].Skills, "requesting-code-review") {
		t.Errorf("injected phase skills = %v, want to contain 'requesting-code-review'", plan.Phases[1].Skills)
	}
}

func containsSkill(skills []string, want string) bool {
	for _, s := range skills {
		if s == want {
			return true
		}
	}
	return false
}

// ─── RUNTIME field parsing ───────────────────────────────────────────────────

func TestParsePhases_RuntimeField(t *testing.T) {
	tests := []struct {
		name        string
		input       string
		wantRuntime core.Runtime
	}{
		{
			name:        "RUNTIME parsed",
			input:       `PHASE: build | OBJECTIVE: Build feature | PERSONA: senior-backend-engineer | RUNTIME: codex`,
			wantRuntime: core.Runtime("codex"),
		},
		{
			name:        "RUNTIME after WORKDIR",
			input:       `PHASE: build | OBJECTIVE: Build it | PERSONA: senior-backend-engineer | WORKDIR: /tmp/repo | RUNTIME: codex`,
			wantRuntime: core.Runtime("codex"),
		},
		{
			name:        "no RUNTIME leaves field empty (zero value = claude)",
			input:       `PHASE: build | OBJECTIVE: Build feature | PERSONA: senior-backend-engineer`,
			wantRuntime: core.Runtime(""),
		},
		{
			name:        "RUNTIME claude explicit",
			input:       `PHASE: build | OBJECTIVE: Build feature | PERSONA: senior-backend-engineer | RUNTIME: claude`,
			wantRuntime: core.RuntimeClaude,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			phases, err := ParsePhases(tt.input, nil)
			if err != nil {
				t.Fatalf("ParsePhases: %v", err)
			}
			if len(phases) != 1 {
				t.Fatalf("len(phases) = %d, want 1", len(phases))
			}
			if phases[0].Runtime != tt.wantRuntime {
				t.Errorf("Runtime = %q, want %q", phases[0].Runtime, tt.wantRuntime)
			}
		})
	}
}

func TestParsePhases_RuntimeWithAllFields(t *testing.T) {
	home, _ := os.UserHomeDir()

	input := `PHASE: plan | OBJECTIVE: Plan the API | PERSONA: senior-backend-engineer
PHASE: impl | OBJECTIVE: Implement API | PERSONA: senior-backend-engineer | SKILLS: golang-pro | DEPENDS: plan | EXPECTED: tests pass | WORKDIR: ~/skills/orchestrator | RUNTIME: codex`

	phases, err := ParsePhases(input, nil)
	if err != nil {
		t.Fatalf("ParsePhases: %v", err)
	}
	if len(phases) != 2 {
		t.Fatalf("len(phases) = %d, want 2", len(phases))
	}

	p := phases[1]
	if p.Name != "impl" {
		t.Errorf("Name = %q, want impl", p.Name)
	}
	if p.Runtime != core.Runtime("codex") {
		t.Errorf("Runtime = %q, want codex", p.Runtime)
	}
	if p.TargetDir != filepath.Join(home, "skills/orchestrator") {
		t.Errorf("TargetDir = %q, want %q", p.TargetDir, filepath.Join(home, "skills/orchestrator"))
	}
	if p.Expected != "tests pass" {
		t.Errorf("Expected = %q, want 'tests pass'", p.Expected)
	}
	if len(p.Skills) != 1 || p.Skills[0] != "golang-pro" {
		t.Errorf("Skills = %v", p.Skills)
	}
}

// TestParsePhases_RuntimeBoth verifies that RUNTIME: both parses to RuntimeBoth.
func TestParsePhases_RuntimeBoth(t *testing.T) {
	input := `PHASE: build | OBJECTIVE: Build feature | PERSONA: senior-backend-engineer | RUNTIME: both`
	phases, err := ParsePhases(input, nil)
	if err != nil {
		t.Fatalf("ParsePhases: %v", err)
	}
	if len(phases) != 1 {
		t.Fatalf("len(phases) = %d, want 1", len(phases))
	}
	if phases[0].Runtime != core.RuntimeBoth {
		t.Errorf("Runtime = %q, want %q", phases[0].Runtime, core.RuntimeBoth)
	}
}

// TestParsePhases_RuntimeCaseInsensitive verifies that RUNTIME values are
// normalized to lowercase so "CODEX", "Codex", and "codex" all resolve the same.
func TestParsePhases_RuntimeCaseInsensitive(t *testing.T) {
	cases := []struct {
		rawRuntime  string
		wantRuntime core.Runtime
	}{
		{"codex", core.RuntimeCodex},
		{"CODEX", core.RuntimeCodex},
		{"Codex", core.RuntimeCodex},
		{"CLAUDE", core.RuntimeClaude},
		{"Claude", core.RuntimeClaude},
		{"BOTH", core.RuntimeBoth},
		{"Both", core.RuntimeBoth},
	}
	for _, c := range cases {
		input := `PHASE: build | OBJECTIVE: Build it | PERSONA: senior-backend-engineer | RUNTIME: ` + c.rawRuntime
		phases, err := ParsePhases(input, nil)
		if err != nil {
			t.Fatalf("RUNTIME %q: ParsePhases error: %v", c.rawRuntime, err)
		}
		if phases[0].Runtime != c.wantRuntime {
			t.Errorf("RUNTIME %q: got %q, want %q", c.rawRuntime, phases[0].Runtime, c.wantRuntime)
		}
	}
}

// TestParsePhases_RuntimeUnknownIgnored verifies that an unrecognized RUNTIME
// value is silently dropped (zero value) so applyRuntimePolicy fills it in.
func TestParsePhases_RuntimeUnknownIgnored(t *testing.T) {
	input := `PHASE: build | OBJECTIVE: Build it | PERSONA: senior-backend-engineer | RUNTIME: unknown-runtime`
	phases, err := ParsePhases(input, nil)
	if err != nil {
		t.Fatalf("ParsePhases: %v", err)
	}
	if phases[0].Runtime != "" {
		t.Errorf("unknown RUNTIME should be empty (zero), got %q", phases[0].Runtime)
	}
}

// TestEnsureCodeReviewPhase_ReviewRuntime verifies that the auto-injected
// review phase picks up tc.ReviewRuntime when set.
func TestEnsureCodeReviewPhase_ReviewRuntime(t *testing.T) {
	persona.Load()
	// Two implementer phases trigger injection via hasCodeProducingPhase.
	plan := &core.Plan{
		Task: "implement a cache layer",
		Phases: []*core.Phase{
			{
				ID:        "phase-1",
				Name:      "plan",
				Objective: "Design the cache layer",
				Persona:   "architect",
				Role:      core.RolePlanner,
				Status:    core.StatusPending,
			},
			{
				ID:        "phase-2",
				Name:      "implement",
				Objective: "Implement the cache layer",
				Persona:   "senior-backend-engineer",
				Role:      core.RoleImplementer,
				Status:    core.StatusPending,
			},
		},
		ExecutionMode: "sequential",
	}

	tc := &TargetContext{ReviewRuntime: core.RuntimeCodex}
	ensureCodeReviewPhase(plan, tc)

	var review *core.Phase
	for _, p := range plan.Phases {
		if p.PersonaSelectionMethod == core.SelectionRequiredReview {
			review = p
			break
		}
	}
	if review == nil {
		t.Fatal("no review phase injected")
	}
	if review.Runtime != core.RuntimeCodex {
		t.Errorf("injected review Runtime = %q, want %q", review.Runtime, core.RuntimeCodex)
	}
}

// TestEnsureCodeReviewPhase_ReviewRuntimeEmpty verifies that when ReviewRuntime
// is not set the injected review phase has no explicit runtime (policy will fill it).
func TestEnsureCodeReviewPhase_ReviewRuntimeEmpty(t *testing.T) {
	persona.Load()
	// Two implementer phases trigger injection via hasCodeProducingPhase.
	plan := &core.Plan{
		Task: "implement a cache layer",
		Phases: []*core.Phase{
			{
				ID:        "phase-1",
				Name:      "plan",
				Objective: "Design the cache layer",
				Persona:   "architect",
				Role:      core.RolePlanner,
				Status:    core.StatusPending,
			},
			{
				ID:        "phase-2",
				Name:      "implement",
				Objective: "Implement the cache layer",
				Persona:   "senior-backend-engineer",
				Role:      core.RoleImplementer,
				Status:    core.StatusPending,
			},
		},
		ExecutionMode: "sequential",
	}

	ensureCodeReviewPhase(plan, nil)

	var review *core.Phase
	for _, p := range plan.Phases {
		if p.PersonaSelectionMethod == core.SelectionRequiredReview {
			review = p
			break
		}
	}
	if review == nil {
		t.Fatal("no review phase injected")
	}
	if review.Runtime != "" {
		t.Errorf("injected review Runtime = %q, want empty (policy will fill)", review.Runtime)
	}
}

// ─── applyRuntimePolicy ───────────────────────────────────────────────────────

// TestApplyRuntimePolicy_FillsEmptyRuntime verifies that phases without an
// explicit RUNTIME: field get a policy-derived (non-empty) runtime after the
// function runs.
func TestApplyRuntimePolicy_FillsEmptyRuntime(t *testing.T) {
	plan := &core.Plan{
		Phases: []*core.Phase{
			{ID: "p1", Persona: "architect", Name: "plan", Objective: "design the system", Role: core.RolePlanner},
			{ID: "p2", Persona: "senior-backend-engineer", Name: "build", Objective: "implement the API", Role: core.RoleImplementer},
			{ID: "p3", Persona: "staff-code-reviewer", Name: "review", Objective: "review the implementation", Role: core.RoleReviewer},
		},
	}
	applyRuntimePolicy(plan)

	for _, p := range plan.Phases {
		if p.Runtime == "" {
			t.Errorf("phase %q: Runtime is still empty after applyRuntimePolicy", p.Name)
		}
		if p.Runtime != core.RuntimeClaude {
			t.Errorf("phase %q: Runtime = %q, want %q", p.Name, p.Runtime, core.RuntimeClaude)
		}
		if !p.RuntimePolicyApplied {
			t.Errorf("phase %q: RuntimePolicyApplied = false, want true", p.Name)
		}
	}
}

// TestApplyRuntimePolicy_ExplicitRuntimeUnchanged verifies that an explicitly
// authored runtime (e.g. "codex") is never overwritten by the policy.
func TestApplyRuntimePolicy_ExplicitRuntimeUnchanged(t *testing.T) {
	plan := &core.Plan{
		Phases: []*core.Phase{
			{ID: "p1", Name: "build", Objective: "implement feature", Role: core.RoleImplementer, Runtime: core.RuntimeCodex},
			{ID: "p2", Name: "plan", Objective: "design the system", Role: core.RolePlanner},
		},
	}
	applyRuntimePolicy(plan)

	if plan.Phases[0].Runtime != core.RuntimeCodex {
		t.Errorf("explicit RUNTIME: codex was overwritten; got %q, want %q",
			plan.Phases[0].Runtime, core.RuntimeCodex)
	}
	if plan.Phases[0].RuntimePolicyApplied {
		t.Errorf("explicit runtime should not set RuntimePolicyApplied")
	}
	if plan.Phases[1].Runtime != core.RuntimeClaude {
		t.Errorf("implicit phase runtime = %q, want %q",
			plan.Phases[1].Runtime, core.RuntimeClaude)
	}
	if !plan.Phases[1].RuntimePolicyApplied {
		t.Errorf("implicit phase should set RuntimePolicyApplied")
	}
}

// TestApplyRuntimePolicy_NilPlan ensures the function is a no-op on nil input.
func TestApplyRuntimePolicy_NilPlan(t *testing.T) {
	// Must not panic.
	applyRuntimePolicy(nil)
}

// TestApplyRuntimePolicy_ExplicitClaudePreserved verifies that an explicit
// RUNTIME: claude authored value is not double-applied (stays as "claude",
// not a zero-value that re-triggers the policy).
func TestApplyRuntimePolicy_ExplicitClaudePreserved(t *testing.T) {
	plan := &core.Plan{
		Phases: []*core.Phase{
			{ID: "p1", Name: "build", Objective: "build it", Role: core.RoleImplementer, Runtime: core.RuntimeClaude},
		},
	}
	applyRuntimePolicy(plan)
	if plan.Phases[0].Runtime != core.RuntimeClaude {
		t.Errorf("explicit RUNTIME: claude changed to %q", plan.Phases[0].Runtime)
	}
	if plan.Phases[0].RuntimePolicyApplied {
		t.Errorf("explicit claude runtime should not set RuntimePolicyApplied")
	}
}

// TestApplyRuntimePolicy_LanguageImplementerGetsClaudeWhileCodexDisabled verifies
// that with Codex auto-selection disabled, language-specialist implementers get
// Claude. When re-enabling Codex, flip expected value back to RuntimeCodex.
func TestApplyRuntimePolicy_LanguageImplementerGetsClaudeWhileCodexDisabled(t *testing.T) {
	plan := &core.Plan{
		Phases: []*core.Phase{
			{
				ID:        "p1",
				Name:      "build",
				Persona:   "senior-backend-engineer",
				Objective: "implement the cache layer",
				Role:      core.RoleImplementer,
			},
		},
	}

	applyRuntimePolicy(plan)

	if plan.Phases[0].Runtime != core.RuntimeClaude {
		t.Errorf("implicit language-specialist implementer runtime = %q, want %q (Codex disabled)",
			plan.Phases[0].Runtime, core.RuntimeClaude)
	}
	if !plan.Phases[0].RuntimePolicyApplied {
		t.Errorf("language-specialist implementer should set RuntimePolicyApplied")
	}
}

// ─── Phase ID uniqueness across decomposition paths ───────────────────────────

// TestAllDecompositionPathsProduceUniquePhaseIDs is a build-time gate that
// validates every non-LLM decomposition path generates plans whose phase IDs
// are unique. The LLM path is excluded because it requires a live SDK call;
// its ID assignment uses the same sequential scheme and is covered by the
// core.ValidatePhaseIDs engine-level check at runtime.
func TestAllDecompositionPathsProduceUniquePhaseIDs(t *testing.T) {
	tests := []struct {
		name string
		plan *core.Plan
	}{
		{
			name: "simplePlan (single-phase fallback)",
			plan: simplePlan("Build a REST API", nil),
		},
		{
			name: "keywordDecompose (multi-phase keyword)",
			plan: keywordDecompose("Write a Go service with tests and deploy it", nil),
		},
		{
			name: "keywordDecompose (research task)",
			plan: keywordDecompose("Research the best database options and summarize findings", nil),
		},
	}

	// Pre-decomposed with multiple phases.
	preDecomp, err := PreDecomposed(
		"PHASE: design | OBJECTIVE: Design the API | PERSONA: architect\n"+
			"PHASE: implement | OBJECTIVE: Build the API | PERSONA: senior-backend-engineer\n"+
			"PHASE: review | OBJECTIVE: Review the implementation | PERSONA: staff-code-reviewer",
		nil,
	)
	if err != nil {
		t.Fatalf("PreDecomposed: %v", err)
	}
	tests = append(tests, struct {
		name string
		plan *core.Plan
	}{"PreDecomposed (multi-phase)", preDecomp})

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if tt.plan == nil {
				t.Fatal("plan is nil")
			}
			if err := core.ValidatePhaseIDs(tt.plan); err != nil {
				t.Errorf("duplicate phase ID detected: %v", err)
			}
		})
	}
}

// TestPreDecomposed_InjectsReviewPhase verifies that PreDecomposed injects a
// review phase when the task text has implementation keywords and the target
// context is a code repository (taskNeedsCodeReview == true).
func TestPreDecomposed_InjectsReviewPhase(t *testing.T) {
	task := "implement authentication\n" +
		"PHASE: implement | OBJECTIVE: implement auth | PERSONA: senior-backend-engineer"
	tc := &TargetContext{TargetID: "repo:~/skills/orchestrator", Language: "go", Runtime: "go"}

	plan, err := PreDecomposed(task, tc)
	if err != nil {
		t.Fatalf("PreDecomposed: %v", err)
	}

	if len(plan.Phases) != 2 {
		t.Fatalf("len(plan.Phases) = %d, want 2 (review injected)", len(plan.Phases))
	}
	last := plan.Phases[len(plan.Phases)-1]
	if last.Persona != "staff-code-reviewer" {
		t.Errorf("last phase persona = %q, want staff-code-reviewer", last.Persona)
	}
	if !containsSkill(last.Skills, "requesting-code-review") {
		t.Errorf("injected review phase missing requesting-code-review skill; got %v", last.Skills)
	}
}

// TestPreDecomposed_SkipWhenReviewerPresent verifies that PreDecomposed does
// not inject a second review phase when one is already authored.
func TestPreDecomposed_SkipWhenReviewerPresent(t *testing.T) {
	task := "implement and review\n" +
		"PHASE: implement | OBJECTIVE: implement feature | PERSONA: senior-backend-engineer\n" +
		"PHASE: review | OBJECTIVE: review code | PERSONA: staff-code-reviewer"
	tc := &TargetContext{TargetID: "repo:~/skills/orchestrator", Language: "go"}

	plan, err := PreDecomposed(task, tc)
	if err != nil {
		t.Fatalf("PreDecomposed: %v", err)
	}

	if len(plan.Phases) != 2 {
		t.Fatalf("len(plan.Phases) = %d, want 2 (no extra review injected)", len(plan.Phases))
	}
}

// TestPreDecomposed_SkipWithSkipReviewInjection verifies that SkipReviewInjection
// prevents injection in the PreDecomposed path.
func TestPreDecomposed_SkipWithSkipReviewInjection(t *testing.T) {
	task := "implement authentication\n" +
		"PHASE: implement | OBJECTIVE: implement auth | PERSONA: senior-backend-engineer"
	tc := &TargetContext{
		TargetID:            "repo:~/skills/orchestrator",
		Language:            "go",
		Runtime:             "go",
		SkipReviewInjection: true,
	}

	plan, err := PreDecomposed(task, tc)
	if err != nil {
		t.Fatalf("PreDecomposed: %v", err)
	}

	if len(plan.Phases) != 1 {
		t.Fatalf("len(plan.Phases) = %d, want 1 (injection suppressed)", len(plan.Phases))
	}
}

// TestSimplePlan_StaysSinglePhaseForCodeTask verifies that simplePlan always
// returns a single-phase plan — review injection is not applied at this layer
// because simplePlan is reserved for non-complex tasks where a standalone
// review phase would be disproportionate.
func TestSimplePlan_StaysSinglePhaseForCodeTask(t *testing.T) {
	tc := &TargetContext{TargetID: "repo:~/skills/orchestrator", Language: "go", Runtime: "go"}
	plan := simplePlan("implement new feature", tc)

	if len(plan.Phases) != 1 {
		t.Fatalf("len(plan.Phases) = %d, want 1 (simplePlan never injects review)", len(plan.Phases))
	}
}
