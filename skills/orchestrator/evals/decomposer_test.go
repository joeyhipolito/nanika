//go:build eval

package evals

import (
	"context"
	"os"
	"regexp"
	"strings"
	"testing"

	"github.com/joeyhipolito/nen/ko/evaltest"
)

// 10-persona catalog matching the decomposer prompt.
var validPersonas = []string{
	"academic-researcher",
	"architect",
	"data-analyst",
	"devops-engineer",
	"qa-engineer",
	"security-auditor",
	"senior-backend-engineer",
	"senior-frontend-engineer",
	"staff-code-reviewer",
	"technical-writer",
}

const decomposerPrompt = `You are a task decomposer for an AI orchestrator.

Break the following task into phases that can be executed by specialized workers.

## Available Personas

- academic-researcher: scholarly research, literature review, evidence synthesis, methodology critique
- architect: technical comparisons, system design, API contracts, storage/framework/protocol decisions
- data-analyst: metrics, logs, trends, quantitative analysis, before/after comparisons
- devops-engineer: CI/CD, Docker, deployment automation, release pipelines, observability
- qa-engineer: test strategy, regression suites, fixtures, flaky test diagnosis
- security-auditor: security audits, threat modeling, auth/secret handling, trust-boundary review
- senior-backend-engineer: backend implementation, CLIs, APIs, databases, migrations, service code
- senior-frontend-engineer: React, Next.js, TypeScript, UI implementation, accessibility
- staff-code-reviewer: code review, hidden regressions, API compatibility, maintainability review
- technical-writer: READMEs, docs, guides, ADRs, technical explainers, article-style synthesis

## Core Rules

1. Each phase must have exactly one persona.
2. Break by user value, not by thin technical layers.
3. Use 2-8 phases unless the task is genuinely trivial.
4. Independent work should run in parallel with no DEPENDS.
5. DEPENDS must only reference phase names already declared earlier in the output.
6. OBJECTIVE must not contain a pipe character.
7. Use academic-researcher only for scholarly or citation-heavy work.
8. Use architect for technical comparisons and decision-shaping research that is not academic.
9. Use technical-writer for docs, technical explainers, and article-style synthesis.
10. Implementation on a code-backed target should include a downstream staff-code-reviewer phase.

## Output Format

Output ONLY PHASE lines, nothing else.
PHASE: <name> | OBJECTIVE: <what to accomplish> | PERSONA: <persona-name> | SKILLS: <skill1,skill2> | DEPENDS: <phase-name1,phase-name2>

## Task

{{task}}`

func evalModel() string {
	if m := os.Getenv("EVAL_MODEL"); m != "" {
		return m
	}
	return "sonnet"
}

func buildPrompt(task string) string {
	return strings.Replace(decomposerPrompt, "{{task}}", task, 1)
}

// evalDecomposer calls the LLM with the decomposer prompt and returns raw output + parsed phases.
func evalDecomposer(t *testing.T, task string) (string, []evaltest.Phase) {
	t.Helper()
	output, err := evaltest.Eval(context.Background(), evalModel(), buildPrompt(task))
	if err != nil {
		t.Fatalf("Eval: %v", err)
	}
	return output, evaltest.ParsePhases(output)
}

// runDefaults runs the default assertions from decomposer.yaml that apply to every test:
// at least one PHASE line, all personas in catalog, forward-reference deps only, no pipe in OBJECTIVE.
func runDefaults(t *testing.T, phases []evaltest.Phase) {
	t.Helper()
	if len(phases) == 0 {
		t.Fatal("no PHASE lines found in output")
	}
	if ok, msg := evaltest.AssertPersonasInSet(phases, validPersonas); !ok {
		t.Error(msg)
	}
	// Strict forward-reference check: DEPENDS must only reference previously declared phases.
	declared := make(map[string]bool)
	for _, ph := range phases {
		for _, dep := range ph.Depends {
			if !declared[dep] {
				t.Errorf("phase %q has forward or unknown dependency %q", ph.Name, dep)
			}
		}
		declared[ph.Name] = true
	}
	if ok, msg := evaltest.AssertNoPipeInObjective(phases); !ok {
		t.Error(msg)
	}
}

// assertNoObjectiveMatch fails if any phase OBJECTIVE matches the regex pattern.
func assertNoObjectiveMatch(t *testing.T, phases []evaltest.Phase, pattern string) {
	t.Helper()
	re := regexp.MustCompile(pattern)
	for _, ph := range phases {
		if re.MatchString(ph.Objective) {
			t.Errorf("phase %q OBJECTIVE matches forbidden pattern /%s/: %q", ph.Name, pattern, ph.Objective)
		}
	}
}

// assertOutputNotMatch fails if the raw LLM output matches the regex pattern.
func assertOutputNotMatch(t *testing.T, output, pattern string) {
	t.Helper()
	re := regexp.MustCompile(pattern)
	if re.MatchString(output) {
		t.Errorf("output matches forbidden pattern /%s/", pattern)
	}
}

func TestDesignAndImplement(t *testing.T) {
	_, phases := evalDecomposer(t, "Design and implement a plugin loading system for the CLI")
	runDefaults(t, phases)

	if ok, msg := evaltest.AssertContainsPersona(phases, "architect"); !ok {
		t.Error(msg)
	}
	if ok, msg := evaltest.AssertContainsPersona(phases, "senior-backend-engineer"); !ok {
		t.Error(msg)
	}
	if ok, msg := evaltest.AssertContainsPersona(phases, "staff-code-reviewer"); !ok {
		t.Error(msg)
	}
}

func TestAcademicLiterature(t *testing.T) {
	_, phases := evalDecomposer(t, "Review the literature on pair programming productivity and turn it into a technical explainer")
	runDefaults(t, phases)

	if ok, msg := evaltest.AssertContainsPersona(phases, "academic-researcher"); !ok {
		t.Error(msg)
	}
	if ok, msg := evaltest.AssertContainsPersona(phases, "technical-writer"); !ok {
		t.Error(msg)
	}
	// Academic task should not collapse into generic technical research.
	if ok, msg := evaltest.AssertFieldNotContains(phases, "persona", "architect"); !ok {
		t.Error(msg)
	}
}

func TestTechnicalComparison(t *testing.T) {
	_, phases := evalDecomposer(t, "Compare deployment options for a solo-maintained Go service and recommend the best one")
	runDefaults(t, phases)

	if ok, msg := evaltest.AssertContainsPersona(phases, "architect"); !ok {
		t.Error(msg)
	}
	if ok, msg := evaltest.AssertFieldNotContains(phases, "persona", "academic-researcher"); !ok {
		t.Error(msg)
	}
}

func TestMetrics(t *testing.T) {
	_, phases := evalDecomposer(t, "Analyze the past 30 days of API errors and identify the endpoint with the worst regression")
	runDefaults(t, phases)

	if ok, msg := evaltest.AssertContainsPersona(phases, "data-analyst"); !ok {
		t.Error(msg)
	}
}

func TestSimpleBugFix(t *testing.T) {
	_, phases := evalDecomposer(t, "Fix the off-by-one bug in the CSV parser")
	runDefaults(t, phases)

	if ok, msg := evaltest.AssertPhaseCountRange(phases, 1, 3); !ok {
		t.Error(msg)
	}
	if ok, msg := evaltest.AssertFieldNotContains(phases, "persona", "technical-writer"); !ok {
		t.Error(msg)
	}
	if ok, msg := evaltest.AssertFieldNotContains(phases, "persona", "academic-researcher"); !ok {
		t.Error(msg)
	}
}
