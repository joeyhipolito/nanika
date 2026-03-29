// Package decompose — passive audit for decomposition quality signals.
package decompose

import (
	"fmt"
	"strings"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/persona"
)

// Passive finding type constants. Scores are always 0, so they stay separate
// from audited findings in the routing DB. Repeated passive findings now feed
// future decomposition through a dedicated read path, while audited findings
// remain the stronger signal when both exist.
const (
	PassiveLowConfidence     = "low_confidence"
	PassivePhaseCountAnomaly = "phase_count_anomaly"
	PassiveAllSamePersona    = "all_same_persona"
	PassiveMissingCodeReview = "missing_code_review"
)

// PassiveFinding is one signal emitted by AuditPlan.
type PassiveFinding struct {
	FindingType         string
	PhaseName           string
	Detail              string
	Severity            string // "low", "medium", "high" — set by rules that support it
	SuggestedPhaseCount int    // non-zero when a specific decomposition depth is recommended
	ObjectiveText       string // excerpt of the task text that triggered the finding
}

// Under-decomposition detection thresholds. All are tunable.
const (
	// UnderDecompWordThreshold: single-phase tasks longer than this word count
	// are candidates for under-decomposition.
	UnderDecompWordThreshold = 20

	// UnderDecompVerbThreshold: at least this many distinct action verbs in the
	// task text triggers the heuristic.
	UnderDecompVerbThreshold = 3

	// UnderDecompConjThreshold: at least this many step-joining conjunctions in
	// the task text triggers the heuristic.
	UnderDecompConjThreshold = 2
)

// actionVerbs are words that signal distinct work items in a task description.
var actionVerbs = []string{
	"add", "analyze", "audit", "build", "check", "configure", "create",
	"define", "delete", "deploy", "design", "develop", "document", "emit",
	"evaluate", "expose", "extend", "fix", "generate", "implement",
	"install", "integrate", "investigate", "measure", "migrate", "optimize",
	"plan", "refactor", "remove", "research", "review", "run", "set up",
	"test", "update", "validate", "verify", "wire", "write",
}

// stepConjunctions are phrases that join sequential steps within a task.
// Patterns are chosen to be non-overlapping: " then " and ", and " are
// omitted because they are always subsumed by " and then " or "; " matches.
var stepConjunctions = []string{
	" and then ", "; ", " additionally ", " also, ",
	" furthermore ", " next, ", " after that ", " finally, ",
}

// countActionVerbs returns the count of distinct action verbs found in text.
func countActionVerbs(text string) int {
	lower := strings.ToLower(text)
	count := 0
	for _, v := range actionVerbs {
		if strings.Contains(lower, v) {
			count++
		}
	}
	return count
}

// countStepConjunctions returns the total number of step-joining conjunction
// occurrences found in text.
func countStepConjunctions(text string) int {
	lower := strings.ToLower(text)
	count := 0
	for _, c := range stepConjunctions {
		idx := 0
		for {
			pos := strings.Index(lower[idx:], c)
			if pos < 0 {
				break
			}
			count++
			idx += pos + len(c)
		}
	}
	return count
}

// estimateSuggestedPhases returns a reasonable phase count given verb and
// conjunction signals. Always returns at least 2.
func estimateSuggestedPhases(verbCount, conjCount int) int {
	suggested := verbCount
	if conjCount+1 > suggested {
		suggested = conjCount + 1
	}
	if suggested < 2 {
		suggested = 2
	}
	return suggested
}

// objectiveExcerpt returns the task text, truncated to 200 chars if needed.
func objectiveExcerpt(task string) string {
	if len(task) > 200 {
		return task[:200] + "..."
	}
	return task
}

// implementKeywords are task-text signals that indicate implementation work.
// Used by the MissingCodeReview rule to decide whether a review phase is warranted.
var implementKeywords = []string{
	"implement", "build", "create", "add", "write code", "develop", "refactor",
	"fix", "migrate", "extend", "update",
}

func hasImplementationSignals(task string) bool {
	taskLower := strings.ToLower(task)
	for _, kw := range implementKeywords {
		if strings.Contains(taskLower, kw) {
			return true
		}
	}
	return false
}

// hasCodeProducingPhase reports whether any phase in the plan is expected to
// produce code or other implementation artifacts. It uses core.ClassifyRole to
// identify implementer phases by persona name, phase name, and objective —
// the same heuristic used by annotateRoles after decomposition.
func hasCodeProducingPhase(phases []*core.Phase) bool {
	for _, p := range phases {
		if p == nil {
			continue
		}
		if core.ClassifyRole(p.Persona, p.Name, p.Objective) == core.RoleImplementer {
			return true
		}
	}
	return false
}

func hasCodeReviewPhase(phases []*core.Phase) bool {
	for _, p := range phases {
		// Primary: only reviewers that explicitly advertise code review count here.
		if persona.HasRole(p.Persona, "reviewer") && persona.HasCapability(p.Persona, "code review") {
			return true
		}
		// Fallback: hardcoded legacy slug for catalogs without frontmatter metadata.
		if p.Persona == "staff-code-reviewer" {
			return true
		}
	}
	return false
}

func isCodeTarget(tc *TargetContext) bool {
	if tc == nil {
		return false
	}
	if strings.HasPrefix(tc.TargetID, "repo:") {
		return true
	}
	return tc.Language != "" || tc.Runtime != ""
}

func taskNeedsCodeReview(task string, tc *TargetContext) bool {
	return isCodeTarget(tc) && hasImplementationSignals(task)
}

// AuditPlan inspects a freshly-decomposed plan for structural quality signals
// without touching the database. It applies four heuristic rules:
//
//  1. LowConfidence   — all phases selected by keyword or fallback (no LLM or target-context bias)
//  2. PhaseCountAnomaly — single phase when context existed, or more than 10 phases
//  3. AllSamePersona  — more than 3 phases all use the exact same persona
//  4. MissingCodeReview — repo target + implementation task + 3+ phases + no code-review phase
//
// tc may be nil; rules that require target context are silently skipped.
// Returns an empty slice (never nil) when no findings are triggered.
func AuditPlan(plan *core.Plan, tc *TargetContext) []PassiveFinding {
	if plan == nil || len(plan.Phases) == 0 {
		return []PassiveFinding{}
	}

	var findings []PassiveFinding

	// Rule 1: LowConfidence — all phases were assigned via keyword or fallback,
	// meaning neither the LLM nor target-context routing provided any signal.
	allLowConfidence := true
	for _, p := range plan.Phases {
		m := p.PersonaSelectionMethod
		if m != "keyword" && m != "fallback" && m != "" {
			allLowConfidence = false
			break
		}
	}
	if allLowConfidence {
		findings = append(findings, PassiveFinding{
			FindingType: PassiveLowConfidence,
			Detail:      "all phases assigned by keyword or fallback; no LLM or target-context signal",
		})
	}

	// Rule 2: PhaseCountAnomaly — detects both under- and over-decomposition.
	n := len(plan.Phases)
	switch {
	case n == 1:
		verbCount := countActionVerbs(plan.Task)
		conjCount := countStepConjunctions(plan.Task)
		wordCount := len(strings.Fields(plan.Task))

		heuristicFired := verbCount >= UnderDecompVerbThreshold ||
			conjCount >= UnderDecompConjThreshold ||
			wordCount > UnderDecompWordThreshold

		// Fire when context existed (original check) OR heuristics indicate a
		// complex multi-step task was collapsed into a single phase.
		if tc != nil || heuristicFired {
			severity := "low"
			suggested := 0
			var detail string

			if heuristicFired {
				suggested = estimateSuggestedPhases(verbCount, conjCount)
				severity = "medium"
				if verbCount >= UnderDecompVerbThreshold && conjCount >= UnderDecompConjThreshold {
					severity = "high"
				}
				detail = fmt.Sprintf(
					"single phase but task has %d action verbs, %d step conjunctions, %d words"+
						" — likely under-decomposed (suggested: %d phases); objective: %q",
					verbCount, conjCount, wordCount, suggested, objectiveExcerpt(plan.Task),
				)
			} else {
				detail = "single phase produced despite target context being available; task may be under-decomposed"
			}

			findings = append(findings, PassiveFinding{
				FindingType:         PassivePhaseCountAnomaly,
				Detail:              detail,
				Severity:            severity,
				SuggestedPhaseCount: suggested,
				ObjectiveText:       plan.Task,
			})
		}

	case n > 10:
		findings = append(findings, PassiveFinding{
			FindingType: PassivePhaseCountAnomaly,
			PhaseName:   plan.Phases[n-1].Name,
			Detail:      "more than 10 phases; plan may be over-decomposed or redundant phases present",
			Severity:    "low",
		})
	}

	// Rule 3: AllSamePersona — more than 3 phases, all using the same persona,
	// suggests the decomposer failed to differentiate work types.
	if n > 3 {
		first := plan.Phases[0].Persona
		all := true
		for _, p := range plan.Phases[1:] {
			if p.Persona != first {
				all = false
				break
			}
		}
		if all && first != "" {
			findings = append(findings, PassiveFinding{
				FindingType: PassiveAllSamePersona,
				Detail:      fmt.Sprintf("all %d phases use persona %s; consider whether distinct specializations are needed", n, first),
			})
		}
	}

	// Rule 4: MissingCodeReview — code target + implementation keywords
	// but no explicit code-review phase.
	if taskNeedsCodeReview(plan.Task, tc) && !hasCodeReviewPhase(plan.Phases) {
		findings = append(findings, PassiveFinding{
			FindingType: PassiveMissingCodeReview,
			Detail:      "implementation task on code target with no code-review phase",
		})
	}

	return findings
}
