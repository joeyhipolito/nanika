// Package decompose breaks tasks into phases using LLM-based decomposition.
package decompose

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/persona"
	"github.com/joeyhipolito/orchestrator-cli/internal/router"
	"github.com/joeyhipolito/orchestrator-cli/internal/sdk"
	"github.com/joeyhipolito/orchestrator-cli/internal/worker"
)

var rustWordRE = regexp.MustCompile(`\brust\b`)

// TargetContext carries resolved target-profile and routing-memory data into
// the decomposer. It is populated by cmd/run.go from routing.RoutingDB and
// passed through Decompose; the decompose package does not import routing.
// All fields are optional — a nil *TargetContext degrades gracefully to the
// existing LLM + keyword matching behavior.
type TargetContext struct {
	// TargetID is the canonical identifier, e.g. "repo:~/skills/orchestrator".
	TargetID string
	// Language is the primary programming language from target_profile.
	Language string
	// Runtime is the build runtime from target_profile, e.g. "go", "cargo".
	Runtime string
	// PreferredPersonas is an ordered list of preferred persona names from target_profile.
	// The decomposer tries each in order and picks the first one found in the catalog.
	PreferredPersonas []string
	// Notes is free-form context from target_profile injected into the LLM prompt.
	Notes string
	// TopPatterns holds routing_patterns for this target, ordered by confidence desc.
	TopPatterns []RoutingHint
	// RolePersonaHints holds historically successful persona assignments for a
	// specific role on this target (e.g. reviewer -> staff-code-reviewer).
	// These only apply when selecting a persona for a phase, not for top-level
	// task routing.
	RolePersonaHints []RolePersonaHint
	// RoutingCorrections holds explicit corrections (audit or manual) indicating
	// that a prior persona assignment for this target was wrong. Injected into
	// the decomposer prompt so the LLM avoids repeating past mistakes.
	RoutingCorrections []CorrectionHint
	// DecompExamples holds validated decomposition patterns for this target,
	// injected into the decomposer prompt as reference patterns. Only examples
	// scoring >= 3 on both audit_score and decomp_quality are included.
	DecompExamples []DecompExampleHint
	// DecompInsights holds aggregate signals from repeated findings.
	// Only populated when the same finding appears in >= 2 missions.
	DecompInsights []DecompInsight
	// HandoffHints holds handoff_pattern rows for this target, ordered by confidence desc.
	// The decomposer uses these to decide whether to append a downstream review or audit phase.
	HandoffHints []HandoffHint
	// PlanShapeStats is the distilled historical decomposition shape for this target.
	// Nil when fewer than 2 qualifying examples exist.
	PlanShapeStats *PlanShapeStats
	// SuccessfulShapes holds phase shape patterns that appeared in 3+ successful
	// missions for this target. These are recorded automatically after every
	// successful run — no audit cycle required. The decomposer uses them to
	// confirm (or question) the shape it is about to produce.
	SuccessfulShapes []PhaseShapeHint
	// TaskType is the classified type of the incoming task (e.g., "implementation",
	// "bugfix"). Used to drive cross-target shape lookup when this target has
	// insufficient execution history.
	TaskType string
	// CrossTargetShapes holds phase shapes that succeeded in 3+ missions for
	// OTHER targets with the same TaskType. Populated only as a cold-start
	// fallback when SuccessfulShapes is empty — i.e., this target is new or
	// hasn't run enough missions to accumulate its own proven shapes.
	// These are weaker signals than SuccessfulShapes: they show what has worked
	// in similar tasks elsewhere, not what has worked for this target specifically.
	CrossTargetShapes []PhaseShapeHint
	// RoutingFailureWarnings holds advisory warnings derived from recent routing
	// failures for personas being considered for this target. Each entry is a
	// human-readable sentence describing a recent failure, e.g.:
	//   "senior-backend-engineer failed phase 'implement' 2 days ago: context window overflow"
	// Injected into the decomposer prompt as a ## Routing Failure Warnings section.
	// Warnings are advisory only — the decomposer may still select the persona.
	RoutingFailureWarnings []string
	// ConflictingFiles is the list of files already claimed by other active
	// missions on the same repository. The decomposer is asked to avoid
	// scheduling edits to these files when possible. Populated by cmd/run.go
	// after git isolation setup and claims conflict detection.
	ConflictingFiles []string
	// SkipReviewInjection suppresses automatic review-phase injection when true.
	// Set by the --no-review CLI flag for missions where a review phase is
	// not wanted (e.g. hot-fix, local-only experiments).
	SkipReviewInjection bool
	// TestCommand is the command used to run tests for this target, e.g. "go test ./....".
	// Injected into the decomposer prompt so phases can reference the correct test invocation.
	TestCommand string
	// BuildCommand is the command used to build this target, e.g. "make build".
	// Injected into the decomposer prompt so phases can reference the correct build step.
	BuildCommand string
	// Framework is the primary framework or library used in this target, e.g. "cobra", "react".
	Framework string
	// KeyDirectories lists notable directories at the repo root, e.g. ["cmd", "internal"].
	KeyDirectories []string
}

// CorrectionHint carries one routing_correction row for the decomposer prompt.
// It records that AssignedPersona was wrong and IdealPersona should have been used.
type CorrectionHint struct {
	AssignedPersona string
	IdealPersona    string
	TaskHint        string
	Source          string // "manual" or "audit"
}

// RoutingHint carries one routing_pattern row for persona bias in decomposition.
type RoutingHint struct {
	Persona    string
	TaskHint   string
	Confidence float64 // 0.0–1.0
}

// RolePersonaHint carries one role-aware routing pattern for a target.
type RolePersonaHint struct {
	Role        string
	Persona     string
	SeenCount   int
	SuccessRate float64
}

// DecompExampleHint is the minimal payload injected into the decomposer prompt.
// It carries only what the LLM needs to pattern-match — no DB metadata.
type DecompExampleHint struct {
	TaskSummary   string // what task this example was for
	PhaseCount    int
	ExecutionMode string
	PhasesJSON    string // compact [{name, objective, persona}]
	DecompSource  string // "predecomposed" vs "llm" — signals confidence
	AuditScore    int    // 1-5
}

// DecompInsight is an aggregate signal derived from repeated findings.
// Only surfaced when the same finding appears in >= 2 independent missions.
type DecompInsight struct {
	FindingType string // missing_phase, wrong_persona, etc.
	Detail      string // what was observed
	Count       int    // number of distinct workspaces with this finding
}

// HandoffHint carries one handoff_pattern row for the decomposer prompt.
// It records that FromPersona commonly hands work to ToPersona for this target,
// which the LLM uses to decide whether to add a downstream review/audit phase.
type HandoffHint struct {
	FromPersona string
	ToPersona   string
	TaskHint    string
	Confidence  float64 // 0.0–1.0
}

// PlanShapeStats summarises the historical decomposition shape for a target.
// Nil means insufficient examples exist; callers must treat nil as absent.
type PlanShapeStats struct {
	AvgPhaseCount  float64  // average across qualifying examples
	MostCommonMode string   // "sequential" or "parallel"
	TopPersonas    []string // top-3 most-frequently-used personas, ordered by frequency
	ExampleCount   int      // number of examples that contributed to these stats
}

// PhaseShapeHint describes a phase structure that has worked in 3+ successful
// missions for a target. It is derived from phase_shape_patterns and carries
// only what the decomposer needs: the confirmed-good phase count, execution
// mode, and persona sequence, along with how many missions used it successfully.
// Unlike DecompExamples (which require manual audit scores), PhaseShapeHints
// are recorded automatically from every successful execution.
type PhaseShapeHint struct {
	PhaseCount    int
	ExecutionMode string
	PersonaSeq    []string // ordered persona names
	SuccessCount  int      // distinct successful missions with this shape
}

// Decompose breaks a task into a Plan with phases.
// skillIndex is the AGENTS-MD routing block; skill names are derived from it.
// Uses LLM for complex tasks, falls back to keyword matching.
// missionID is used for event correlation. em receives decompose lifecycle events.
// tc carries optional target-profile and routing-memory data that biases persona selection.
func Decompose(ctx context.Context, task, learnings, skillIndex, missionID string, em event.Emitter, tc *TargetContext) (*core.Plan, error) {
	model := router.Resolve(router.TierThink)

	summary := task
	if len(summary) > 200 {
		summary = summary[:200] + "..."
	}
	em.Emit(ctx, event.New(event.DecomposeStarted, missionID, "", "", map[string]any{
		"task_summary": summary,
		"model":        model,
	}))

	// Passthrough: if the task already contains explicit PHASE lines (hand-written in a
	// mission file or produced by the decomposer skill), use them verbatim without invoking
	// the LLM. This preserves intentional phase structure and avoids unnecessary API calls.
	// cmd/run.go also checks this before calling Decompose, but enforcing it here makes
	// Decompose safe to call regardless of call site.
	if HasPreDecomposedPhases(task) {
		plan, err := PreDecomposed(task, tc)
		if err != nil {
			return nil, fmt.Errorf("pre-decomposed passthrough: %w", err)
		}
		plan.DecompSource = core.DecompPredecomposed
		emitDecomposeCompleted(ctx, em, missionID, plan)
		return plan, nil
	}

	if !router.ClassifyComplexity(task) {
		plan := simplePlan(task, tc)
		plan.DecompSource = core.DecompKeyword
		emitDecomposeCompleted(ctx, em, missionID, plan)
		return plan, nil
	}

	plan, err := llmDecompose(ctx, task, learnings, skillIndex, tc)
	if err != nil {
		em.Emit(ctx, event.New(event.DecomposeFallback, missionID, "", "", map[string]any{
			"reason": err.Error(),
		}))
		plan = keywordDecompose(task, tc)
		plan.DecompSource = core.DecompKeyword
		emitDecomposeCompleted(ctx, em, missionID, plan)
		return plan, nil
	}

	plan.DecompSource = core.DecompLLM
	emitDecomposeCompleted(ctx, em, missionID, plan)
	return plan, nil
}

// emitDecomposeCompleted emits a decompose.completed event with plan summary.
func emitDecomposeCompleted(ctx context.Context, em event.Emitter, missionID string, plan *core.Plan) {
	phases := make([]map[string]any, len(plan.Phases))
	for i, p := range plan.Phases {
		phases[i] = map[string]any{
			"name":                   p.Name,
			"persona":                p.Persona,
			"selection_method":       p.PersonaSelectionMethod,
			"skills":                 p.Skills,
			"runtime":                p.Runtime,
			"runtime_policy_applied": p.RuntimePolicyApplied,
		}
	}
	em.Emit(ctx, event.New(event.DecomposeCompleted, missionID, "", "", map[string]any{
		"phase_count":    len(plan.Phases),
		"execution_mode": plan.ExecutionMode,
		"phases":         phases,
	}))
}

func simplePlan(task string, tc *TargetContext) *core.Plan {
	personaName, selectionMethod := pickPersona(task, tc)
	tier := router.ClassifyTier(task, personaName)

	plan := &core.Plan{
		ID:            fmt.Sprintf("plan_%d", time.Now().UnixNano()),
		Task:          task,
		ExecutionMode: "sequential",
		CreatedAt:     time.Now(),
		Phases: []*core.Phase{
			{
				ID:                     "phase-1",
				Name:                   "execute",
				Objective:              task,
				Persona:                personaName,
				PersonaSelectionMethod: selectionMethod,
				ModelTier:              string(tier),
				Status:                 core.StatusPending,
			},
		},
	}
	annotateRoles(plan)
	applyRuntimePolicy(plan)
	return plan
}

func llmDecompose(ctx context.Context, task, learnings, skillIndex string, tc *TargetContext) (*core.Plan, error) {
	prompt := buildDecomposerPrompt(task, learnings, skillIndex, tc)

	output, err := sdk.QueryText(ctx, prompt, &sdk.AgentOptions{
		Model:    router.Resolve(router.TierThink),
		MaxTurns: 1,
	})
	if err != nil {
		return nil, err
	}

	phases, err := ParsePhases(output, tc)
	if err != nil {
		return nil, err
	}

	// Determine execution mode
	mode := "sequential"
	if hasParallelPhases(phases) {
		mode = "parallel"
	}

	plan := &core.Plan{
		ID:            fmt.Sprintf("plan_%d", time.Now().UnixNano()),
		Task:          task,
		Phases:        phases,
		ExecutionMode: mode,
		CreatedAt:     time.Now(),
	}
	ensureCodeReviewPhase(plan, tc)
	annotateRoles(plan)
	applyRuntimePolicy(plan)
	return plan, nil
}

// loadRulesFromSKILLMD reads and extracts decomposition rules from SKILL.md.
// It respects the NANIKA_DECOMPOSER_SKILL environment variable for path override,
// falling back to VIA_DECOMPOSER_SKILL (legacy).
// If the file is not found or can't be parsed, it returns the hardcoded fallback rules.
func loadRulesFromSKILLMD() string {
	// Determine path: env var override, then default locations
	skillPath := os.Getenv("NANIKA_DECOMPOSER_SKILL")
	if skillPath == "" {
		skillPath = os.Getenv("VIA_DECOMPOSER_SKILL") // legacy
	}
	if skillPath == "" {
		// Try ~/nanika/.claude/skills/decomposer/SKILL.md first
		skillPath = filepath.Join(os.Getenv("HOME"), "nanika", ".claude", "skills", "decomposer", "SKILL.md")
		if _, err := os.Stat(skillPath); err != nil {
			// Fall back to ~/skills/orchestrator/.claude/skills/decomposer/SKILL.md
			skillPath = filepath.Join(os.Getenv("HOME"), "skills", "orchestrator", ".claude", "skills", "decomposer", "SKILL.md")
			if _, err := os.Stat(skillPath); err != nil {
				// File not found, use hardcoded fallback
				return hardcodedRules()
			}
		}
	}

	content, err := os.ReadFile(skillPath)
	if err != nil {
		// File read failed, use hardcoded fallback
		return hardcodedRules()
	}

	rules := extractRules(string(content))
	if rules == "" {
		// Extraction failed, use hardcoded fallback
		return hardcodedRules()
	}

	return rules
}

// extractRules parses SKILL.md content and extracts the rules section.
// It extracts everything between "## Output Format" and "## Worked Examples".
func extractRules(content string) string {
	lines := strings.Split(content, "\n")

	startIdx := -1
	endIdx := -1

	for i, line := range lines {
		if strings.HasPrefix(line, "## Output Format") {
			startIdx = i
		}
		if startIdx != -1 && strings.HasPrefix(line, "## Worked Examples") {
			endIdx = i
			break
		}
	}

	if startIdx == -1 || endIdx == -1 {
		// Markers not found, return empty string to trigger fallback
		return ""
	}

	// Extract lines between startIdx and endIdx (exclusive)
	rulesLines := lines[startIdx+1 : endIdx]
	rules := strings.TrimSpace(strings.Join(rulesLines, "\n"))

	return rules
}

// hardcodedRules returns the fallback rules when SKILL.md is not available.
func hardcodedRules() string {
	return `## Core Rules

1. Each phase must have exactly ONE persona.
2. Break by USER VALUE, not by technical layer.
   - GOOD: "Implement user auth (DB + API + tests)" — complete capability
   - BAD: "Set up database" — technical layer with no user value
3. Aim for 3-8 phases. Prefer fewer phases with rich objectives over many thin ones. Maximum 12.
4. Split independent sub-tasks into separate phases so they run in parallel.
   If the task contains work that can proceed simultaneously (e.g., reading articles AND
   reading a codebase), create one phase per independent task with no DEPENDS.
   Only add DEPENDS when a phase genuinely needs the output of another phase.
5. Pick the MOST SPECIFIC persona from the catalog above. Match by WhenToUse triggers.
6. Assign SKILLS only when a phase genuinely needs a specific tool's commands.
   Match by skill description — do not guess.
7. Respect persona handoff chains. When a persona lists "Hands off to: X",
   consider adding a follow-up phase using persona X that DEPENDS on the handing-off phase.
   Example: academic-researcher hands off to technical-writer → add a writing
   phase after the research that turns findings into clear developer-facing output.
   Only add the handoff phase when the task would benefit from it — not every phase needs it.
8. For code implementation on code-backed targets, always include a downstream
   reviewer-role persona phase (e.g. staff-code-reviewer or security-auditor) before final validation/QA.`
}

func buildDecomposerPrompt(task, learnings, skillIndex string, tc *TargetContext) string {
	personaSummary := persona.FormatForDecomposer()

	// Build skill summary with names and descriptions from the routing index
	skillSummary := worker.FormatSkillsForDecomposer(skillIndex)
	if skillSummary == "" {
		skillSummary = "No skills available."
	}

	// Load rules from SKILL.md with fallback to hardcoded rules
	rules := loadRulesFromSKILLMD()

	prompt := fmt.Sprintf(`You are a task decomposer for an AI orchestrator.

Break the following task into phases that can be executed by specialized workers.

## Available Personas
%s

## Available Skills
%s

%s

## Output Format
Output ONLY PHASE lines, nothing else. No preamble, no summary, no commentary.
One phase per line, pipe-delimited:
PHASE: <name> | OBJECTIVE: <what to accomplish> | PERSONA: <persona-name> | SKILLS: <skill1,skill2> | DEPENDS: <phase-name1,phase-name2>

DEPENDS is optional. SKILLS is optional. Do NOT repeat or revise the plan — output it exactly once.

`, personaSummary, skillSummary, rules)

	// Target Context: stable facts about the target that bias persona selection.
	// Injected before learnings so the LLM treats it as a stronger prior.
	if tc != nil {
		prompt += formatTargetContext(tc)
		if len(tc.TopPatterns) > 0 {
			prompt += formatRoutingLearnings(tc)
		}
		if len(tc.RoutingCorrections) > 0 {
			prompt += formatRoutingCorrections(tc)
		}
		if len(tc.DecompExamples) > 0 {
			prompt += formatDecompExamples(tc)
		}
		if len(tc.DecompInsights) > 0 {
			prompt += formatDecompInsights(tc)
		}
		if len(tc.HandoffHints) > 0 {
			prompt += formatHandoffPatterns(tc)
		}
		if tc.PlanShapeStats != nil {
			prompt += formatPlanShapeStats(tc)
		}
		if len(tc.SuccessfulShapes) > 0 {
			prompt += formatSuccessfulShapes(tc)
		} else if len(tc.CrossTargetShapes) > 0 {
			prompt += formatCrossTargetShapes(tc)
		}
	}

	if tc != nil && len(tc.RoutingFailureWarnings) > 0 {
		prompt += formatRoutingFailureWarnings(tc.RoutingFailureWarnings)
	}

	if tc != nil && len(tc.ConflictingFiles) > 0 {
		prompt += formatConflictingFiles(tc.ConflictingFiles)
	}

	if learnings != "" {
		prompt += fmt.Sprintf("## Lessons from Past Missions\n%s\n\n", learnings)
	}

	if taskNeedsCodeReview(task, tc) {
		prompt += "\n## Mandatory Review Policy\nFor code implementation on this target, include a downstream PHASE using a reviewer-role persona (e.g. staff-code-reviewer, or security-auditor when security is the core concern) before final QA/validation.\n"
	}

	prompt += fmt.Sprintf("## Task to Decompose\n%s\n", task)

	return prompt
}

// BuildBaselinePrompt returns the decomposer system prompt with no target context,
// learnings, or skill index. It is the canonical baseline used by the promptfoo eval
// in ~/nanika/evals/decomposer.yaml; regenerate evals/prompts/decomposer.txt via
// `make gen-eval-prompt` whenever this function's output changes.
func BuildBaselinePrompt(task string) string {
	return buildDecomposerPrompt(task, "", "", nil)
}

// formatRoutingFailureWarnings injects an advisory section listing recent routing
// failures for personas being considered. Warnings are advisory — the decomposer
// should weigh them but is not required to avoid the persona.
func formatRoutingFailureWarnings(warnings []string) string {
	var b strings.Builder
	b.WriteString("## Routing Failure Warnings\n")
	b.WriteString("The following personas had recent failures on similar tasks. Consider alternatives if the failure pattern matches the current task:\n")
	for _, w := range warnings {
		b.WriteString(fmt.Sprintf("- %s\n", w))
	}
	b.WriteString("\n")
	return b.String()
}

// formatConflictingFiles injects a constraint section listing files claimed by
// other active missions. The LLM is asked to avoid scheduling edits to them.
func formatConflictingFiles(files []string) string {
	var b strings.Builder
	b.WriteString("## Parallel Mission Constraints\n")
	b.WriteString("The following files are currently claimed by other active missions on this repository.\n")
	b.WriteString("Avoid scheduling modifications to these files when possible:\n")
	for _, f := range files {
		b.WriteString(fmt.Sprintf("- %s\n", f))
	}
	b.WriteString("\n")
	return b.String()
}

// formatTargetContext formats a TargetContext into the ## Target Context prompt section.
func formatTargetContext(tc *TargetContext) string {
	var b strings.Builder
	b.WriteString("## Target Context\n")
	if tc.TargetID != "" {
		b.WriteString(fmt.Sprintf("Target: %s\n", tc.TargetID))
	}
	if tc.Language != "" {
		b.WriteString(fmt.Sprintf("Language: %s\n", tc.Language))
	}
	if tc.Runtime != "" {
		b.WriteString(fmt.Sprintf("Runtime: %s\n", tc.Runtime))
	}
	if len(tc.PreferredPersonas) > 0 {
		b.WriteString(fmt.Sprintf("Preferred personas: %s\n", strings.Join(tc.PreferredPersonas, ", ")))
	}
	if tc.TaskType != "" {
		b.WriteString(fmt.Sprintf("Task type: %s\n", tc.TaskType))
	}
	if tc.Framework != "" {
		b.WriteString(fmt.Sprintf("Framework: %s\n", tc.Framework))
	}
	if tc.TestCommand != "" {
		b.WriteString(fmt.Sprintf("Test command: %s\n", tc.TestCommand))
	}
	if tc.BuildCommand != "" {
		b.WriteString(fmt.Sprintf("Build command: %s\n", tc.BuildCommand))
	}
	if len(tc.KeyDirectories) > 0 {
		b.WriteString(fmt.Sprintf("Key directories: %s\n", strings.Join(tc.KeyDirectories, ", ")))
	}
	if tc.Notes != "" {
		b.WriteString(fmt.Sprintf("Notes: %s\n", tc.Notes))
	}
	b.WriteString("\n")
	return b.String()
}

// formatRoutingLearnings formats routing patterns into the ## Routing Learnings prompt section.
// This section is distinct from ## Lessons from Past Missions: it records observed
// persona assignments for this target, not general insights from past missions.
func formatRoutingLearnings(tc *TargetContext) string {
	var b strings.Builder
	b.WriteString("## Routing Learnings\n")
	b.WriteString("Based on past routing for this target:\n")
	for _, h := range tc.TopPatterns {
		if h.TaskHint != "" {
			fmt.Fprintf(&b, "- %s (confidence: %.1f, hint: %s)\n", h.Persona, h.Confidence, h.TaskHint)
		} else {
			fmt.Fprintf(&b, "- %s (confidence: %.1f)\n", h.Persona, h.Confidence)
		}
	}
	b.WriteString("\n")
	return b.String()
}

// formatRoutingCorrections formats routing corrections into the ## Prior Routing Corrections
// prompt section. This tells the decomposer which persona assignments were wrong in the past
// so it can avoid repeating those mistakes.
func formatRoutingCorrections(tc *TargetContext) string {
	var b strings.Builder
	b.WriteString("## Prior Routing Corrections\n")
	b.WriteString("The following persona assignments were previously flagged as incorrect for this target.\n")
	b.WriteString("Do NOT repeat these assignments for similar tasks:\n")
	for _, c := range tc.RoutingCorrections {
		if c.TaskHint != "" {
			fmt.Fprintf(&b, "- %s was wrong for %q → use %s instead (source: %s)\n",
				c.AssignedPersona, c.TaskHint, c.IdealPersona, c.Source)
		} else {
			fmt.Fprintf(&b, "- %s was wrong → use %s instead (source: %s)\n",
				c.AssignedPersona, c.IdealPersona, c.Source)
		}
	}
	b.WriteString("\n")
	return b.String()
}

// formatDecompExamples formats validated decomposition examples into a prompt
// section. Examples are suggestions, not directives — the LLM retains discretion.
func formatDecompExamples(tc *TargetContext) string {
	var b strings.Builder
	b.WriteString("## Validated Decomposition Examples\n")
	b.WriteString("The following decomposition patterns scored well for this target in past audits.\n")
	b.WriteString("Use them as reference patterns for similar tasks — adapt as needed, do not copy blindly.\n\n")
	for i, ex := range tc.DecompExamples {
		source := ex.DecompSource
		if source == "predecomposed" {
			source = "human-written"
		}
		fmt.Fprintf(&b, "### Example %d (score: %d/5, source: %s, %d phases, %s)\n",
			i+1, ex.AuditScore, source, ex.PhaseCount, ex.ExecutionMode)
		if ex.TaskSummary != "" {
			fmt.Fprintf(&b, "Task: %s\n", ex.TaskSummary)
		}
		fmt.Fprintf(&b, "Phases: %s\n\n", ex.PhasesJSON)
	}
	return b.String()
}

// Audit finding type constants — mirrored from internal/audit/types.go to avoid
// importing audit into decompose. Values are DB-stable: changing them would
// require a migration on decomposition_findings, so drift risk is near zero.
const (
	findingMissingPhase   = "missing_phase"
	findingRedundantPhase = "redundant_phase"
	findingPhaseDrift     = "phase_drift"
	findingWrongPersona   = "wrong_persona"
	findingLowPhaseScore  = "low_phase_score"
)

// insightDirectives maps finding types to actionable directives for the decomposer.
// Observational output ("observed in N missions") alone gives the LLM no guidance
// on what to change. These directives close the loop.
var insightDirectives = map[string]string{
	findingMissingPhase:      "Add a dedicated phase for this capability.",
	findingRedundantPhase:    "Fold this work into adjacent phases; avoid a standalone phase for it.",
	findingPhaseDrift:        "Tighten this phase's objective to one concrete outcome.",
	findingWrongPersona:      "Use the ideal persona named in the detail field for this kind of work.",
	findingLowPhaseScore:     "Split or clarify this phase type's objective so it is actionable.",
	PassiveLowConfidence:     "Prefer a decomposition path with stronger target-context, routing, or review signal before trusting this plan.",
	PassivePhaseCountAnomaly: "Reconsider the plan size and split or compress phases until the structure matches the task's real complexity.",
	PassiveAllSamePersona:    "Use more than one specialization if the task spans distinct work types.",
	PassiveMissingCodeReview: "Add a downstream reviewer-role phase (e.g. staff-code-reviewer or security-auditor) when implementation work warrants review.",
}

// formatDecompInsights formats aggregate decomposition insights into a prompt
// section. Only findings that appear across multiple independent missions are
// included — this is the core damping mechanism. Each finding is paired with
// a per-type directive so the LLM knows what action to take, not just what was observed.
func formatDecompInsights(tc *TargetContext) string {
	var b strings.Builder
	b.WriteString("## Decomposition Insights (from repeated audit findings)\n")
	b.WriteString("The following structural issues have been observed across multiple missions for this target.\n")
	b.WriteString("Apply the corresponding action when decomposing — these are recurring problems, not one-off issues.\n\n")
	for _, ins := range tc.DecompInsights {
		directive := insightDirectives[ins.FindingType]
		if directive == "" {
			directive = "Avoid repeating this pattern."
		}
		fmt.Fprintf(&b, "- [%s] %s (observed in %d missions) → %s\n",
			ins.FindingType, ins.Detail, ins.Count, directive)
	}
	b.WriteString("\n")
	return b.String()
}

// formatHandoffPatterns formats handoff_pattern rows into a ## Handoff Patterns
// prompt section. This tells the decomposer which persona transitions are commonly
// observed for this target so it can append downstream phases when appropriate.
func formatHandoffPatterns(tc *TargetContext) string {
	var b strings.Builder
	b.WriteString("## Observed Handoff Patterns\n")
	b.WriteString("The following persona transitions have been observed for this target in past missions.\n")
	b.WriteString("When the task would benefit from the downstream persona, add a follow-up phase that DEPENDS on the handing-off phase:\n")
	for _, h := range tc.HandoffHints {
		if h.TaskHint != "" {
			fmt.Fprintf(&b, "- %s → %s (confidence: %.1f, for: %s)\n",
				h.FromPersona, h.ToPersona, h.Confidence, h.TaskHint)
		} else {
			fmt.Fprintf(&b, "- %s → %s (confidence: %.1f)\n",
				h.FromPersona, h.ToPersona, h.Confidence)
		}
	}
	b.WriteString("\n")
	return b.String()
}

// formatPlanShapeStats formats historical plan-shape statistics into a ## Plan Shape
// prompt section. This gives the LLM a distilled summary ("typical: 5 phases,
// sequential, backend→reviewer") rather than requiring it to infer shape from raw examples.
func formatPlanShapeStats(tc *TargetContext) string {
	s := tc.PlanShapeStats
	var b strings.Builder
	b.WriteString("## Historical Plan Shape (from past validated decompositions)\n")
	fmt.Fprintf(&b, "Based on %d validated examples for this target:\n", s.ExampleCount)
	fmt.Fprintf(&b, "- Typical phase count: %.1f\n", s.AvgPhaseCount)
	if s.MostCommonMode != "" {
		fmt.Fprintf(&b, "- Typical execution mode: %s\n", s.MostCommonMode)
	}
	if len(s.TopPersonas) > 0 {
		fmt.Fprintf(&b, "- Most-used personas: %s\n", strings.Join(s.TopPersonas, ", "))
	}
	b.WriteString("Use this as a reference for scale — not a rigid template. Deviate when the task genuinely requires it.\n\n")
	return b.String()
}

// formatSuccessfulShapes formats phase_shape_pattern rows into a ## Proven Phase
// Shapes prompt section. These shapes have been confirmed by real execution
// outcomes — not just audit scores — so they carry stronger directional weight
// than PlanShapeStats averages. Each entry shows the full persona sequence so
// the decomposer can validate its current plan against known-good structures.
func formatSuccessfulShapes(tc *TargetContext) string {
	var b strings.Builder
	b.WriteString("## Proven Phase Shapes (from successful mission executions)\n")
	b.WriteString("The following phase sequences have each produced successful outcomes across multiple missions for this target.\n")
	b.WriteString("When your decomposition matches or resembles one of these shapes, you are on solid ground.\n")
	b.WriteString("When it diverges, verify you have a reason to deviate:\n\n")
	for i, s := range tc.SuccessfulShapes {
		fmt.Fprintf(&b, "%d. [%d phases, %s, succeeded %d times] %s\n",
			i+1, s.PhaseCount, s.ExecutionMode, s.SuccessCount, strings.Join(s.PersonaSeq, " → "))
	}
	b.WriteString("\n")
	return b.String()
}

// formatCrossTargetShapes formats cross-target phase shapes into a prompt section.
// This is a cold-start fallback shown only when no target-specific SuccessfulShapes
// exist. It surfaces shapes that worked for OTHER targets on the same task type —
// a weaker prior than target-specific history, so the framing is deliberately
// tentative ("have tended to work", "use as a starting point only").
func formatCrossTargetShapes(tc *TargetContext) string {
	var b strings.Builder
	b.WriteString("## Cross-Target Phase Shapes (same task type, different targets)\n")
	if tc.TaskType != "" {
		fmt.Fprintf(&b, "Task type: %s. This target has no proven shapes yet.\n", tc.TaskType)
	}
	b.WriteString("The following shapes have tended to work for similar tasks on other targets.\n")
	b.WriteString("Treat these as a starting point only — not a directive. Deviate freely when target specifics require it:\n\n")
	for i, s := range tc.CrossTargetShapes {
		fmt.Fprintf(&b, "%d. [%d phases, %s, succeeded %d times across targets] %s\n",
			i+1, s.PhaseCount, s.ExecutionMode, s.SuccessCount, strings.Join(s.PersonaSeq, " → "))
	}
	b.WriteString("\n")
	return b.String()
}

// maxPhases caps the number of phases to prevent runaway decomposition.
const maxPhases = 12

// parsePhaseLine parses a single PHASE: line in pipe-delimited format.
// Fields may appear in any order after PHASE: and OBJECTIVE:. Leading and
// trailing backticks are stripped so LLM output wrapped in code fences parses
// correctly (e.g. `PHASE: foo | OBJECTIVE: bar | RUNTIME: claude | DEPENDS: baz`).
// Returns the extracted fields and ok=true when both PHASE and OBJECTIVE are present.
func parsePhaseLine(line string) (fields map[string]string, ok bool) {
	// Strip leading/trailing backticks.
	line = strings.Trim(line, "`")

	if !strings.HasPrefix(line, "PHASE:") {
		return nil, false
	}

	fields = make(map[string]string, 9)
	for _, part := range strings.Split(line, "|") {
		part = strings.TrimSpace(part)
		idx := strings.IndexByte(part, ':')
		if idx <= 0 {
			continue
		}
		key := strings.TrimSpace(part[:idx])
		value := strings.TrimSpace(part[idx+1:])
		if key != "" {
			fields[key] = value
		}
	}

	if fields["PHASE"] == "" || fields["OBJECTIVE"] == "" {
		return nil, false
	}
	return fields, true
}

// HasPreDecomposedPhases returns true if the text contains PHASE: lines,
// indicating it was pre-decomposed (e.g., by the decomposer skill) and
// should bypass the LLM decomposer.
func HasPreDecomposedPhases(text string) bool {
	for _, line := range strings.Split(text, "\n") {
		line = strings.Trim(strings.TrimSpace(line), "`")
		if strings.HasPrefix(line, "PHASE:") {
			return true
		}
	}
	return false
}

// PreDecomposed parses pre-decomposed PHASE lines from a mission file and
// wraps them in a Plan. Returns an error if no valid phases are found.
// tc biases persona selection when a phase specifies an unrecognized persona.
func PreDecomposed(task string, tc *TargetContext) (*core.Plan, error) {
	phases, err := ParsePhases(task, tc)
	if err != nil {
		return nil, fmt.Errorf("parse pre-decomposed phases: %w", err)
	}

	mode := "sequential"
	if hasParallelPhases(phases) {
		mode = "parallel"
	}

	plan := &core.Plan{
		ID:            fmt.Sprintf("plan_%d", time.Now().UnixNano()),
		Task:          task,
		Phases:        phases,
		ExecutionMode: mode,
		CreatedAt:     time.Now(),
	}
	// For human-authored pre-decomposed plans, only inject a review phase when the
	// target+task signal clearly indicates code work. The persona-based path in
	// ensureCodeReviewPhase is too aggressive here because non-code personas
	// (academic-researcher, technical-writer, data-analyst, etc.) default to
	// RoleImplementer and would spuriously trigger injection on non-code plans.
	if taskNeedsCodeReview(task, tc) {
		ensureCodeReviewPhase(plan, tc)
	}
	annotateRoles(plan)
	applyRuntimePolicy(plan)
	return plan, nil
}

// ParsePhases extracts PHASE lines from LLM or pre-decomposed output and
// returns them as core.Phase structs with resolved dependencies.
// tc biases persona fallback when the LLM returns an unrecognized persona name.
func ParsePhases(output string, tc *TargetContext) ([]*core.Phase, error) {
	var phases []*core.Phase
	phaseMap := make(map[string]string) // name -> id
	seenNames := make(map[string]bool)  // dedup: skip duplicate phase names

	for _, line := range strings.Split(output, "\n") {
		line = strings.TrimSpace(line)
		f, ok := parsePhaseLine(line)
		if !ok {
			continue
		}

		name := f["PHASE"]
		objective := f["OBJECTIVE"]
		personaName := f["PERSONA"]
		skills := f["SKILLS"]
		depends := f["DEPENDS"]
		expected := f["EXPECTED"]
		workdir := f["WORKDIR"]
		runtimeStr := f["RUNTIME"]
		timeoutStr := f["TIMEOUT"]
		priorityStr := f["PRIORITY"]

		// Dedup: skip if we've already seen this phase name.
		// This catches the common LLM pattern of repeating the plan
		// across multiple turns or in a "revised" section.
		if seenNames[name] {
			continue
		}
		seenNames[name] = true

		// If LLM gave no persona or an unrecognised one, fall back to persona resolution
		// with target-context bias applied.
		var selectionMethod string
		if persona.Get(personaName) == nil {
			personaName, selectionMethod = resolvePersona(objective, tc)
		} else {
			selectionMethod = "llm"
		}

		id := fmt.Sprintf("phase-%d", len(phases)+1)
		phaseMap[name] = id

		tier := router.ClassifyTier(objective, personaName)

		phase := &core.Phase{
			ID:                     id,
			Name:                   name,
			Objective:              objective,
			Persona:                personaName,
			PersonaSelectionMethod: selectionMethod,
			ModelTier:              string(tier),
			Status:                 core.StatusPending,
			Expected:               expected,
			TargetDir:              expandTilde(workdir),
			Runtime:                core.Runtime(runtimeStr),
			Priority:               strings.ToUpper(strings.TrimSpace(priorityStr)),
		}

		// Parse per-phase stall timeout (e.g. TIMEOUT: 30m).
		if timeoutStr != "" {
			if d, err := time.ParseDuration(timeoutStr); err == nil && d > 0 {
				phase.StallTimeout = d
			}
		}

		// Parse skills
		if skills != "" {
			for _, s := range strings.Split(skills, ",") {
				s = strings.TrimSpace(s)
				if s != "" {
					phase.Skills = append(phase.Skills, s)
				}
			}
		}

		// Parse dependencies (resolve names to IDs)
		if depends != "" {
			for _, dep := range strings.Split(depends, ",") {
				dep = strings.TrimSpace(dep)
				if depID, ok := phaseMap[dep]; ok {
					phase.Dependencies = append(phase.Dependencies, depID)
				}
			}
		}

		phases = append(phases, phase)

		// Cap phase count to prevent runaway decomposition.
		if len(phases) >= maxPhases {
			fmt.Fprintf(os.Stderr, "[decompose] warning: phase count capped at %d; extra phases dropped\n", maxPhases)
			break
		}
	}

	if len(phases) == 0 {
		return nil, fmt.Errorf("no phases parsed from output")
	}

	return phases, nil
}

// expandTilde replaces a leading "~/" with the user's home directory.
// Returns path unchanged when it does not start with "~/", or when
// os.UserHomeDir fails (path is returned as-is so callers stay functional).
// Returns "" when path is "".
func expandTilde(path string) string {
	if path == "" || !strings.HasPrefix(path, "~/") {
		return path
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return path
	}
	return filepath.Join(home, path[2:])
}

func hasParallelPhases(phases []*core.Phase) bool {
	if len(phases) < 2 {
		return false
	}

	// Check 1: Any non-first phase with no dependencies → parallel
	for i, p := range phases {
		if i > 0 && len(p.Dependencies) == 0 {
			return true
		}
	}

	// Check 2: Multiple phases share the same dependency set → they can run in parallel
	// E.g., phase-3 depends on phase-2, and phase-4 depends on phase-2 → parallel
	depSets := make(map[string]int) // serialized dep set → count
	for _, p := range phases {
		if len(p.Dependencies) > 0 {
			key := strings.Join(p.Dependencies, ",")
			depSets[key]++
			if depSets[key] > 1 {
				return true
			}
		}
	}

	return false
}

func keywordDecompose(task string, tc *TargetContext) *core.Plan {
	lower := strings.ToLower(task)

	var phases []*core.Phase

	// Research phase if task mentions research/analyze/investigate
	if containsAny(lower, "research", "analyze", "investigate", "explore", "find", "compare") {
		researchPersona := pickResearchPersona(task)
		phases = append(phases, &core.Phase{
			ID:                     "phase-1",
			Name:                   "research",
			Objective:              "Research and gather information: " + task,
			Persona:                researchPersona,
			PersonaSelectionMethod: "keyword",
			ModelTier:              string(router.TierWork),
			Status:                 core.StatusPending,
		})
	}

	// Implementation phase
	if containsAny(lower, "implement", "build", "create", "write code", "develop", "add") {
		depID := ""
		if len(phases) > 0 {
			depID = phases[len(phases)-1].ID
		}

		implPersona, implMethod := pickImplementationPersona(task)
		p := &core.Phase{
			ID:                     fmt.Sprintf("phase-%d", len(phases)+1),
			Name:                   "implement",
			Objective:              "Implement: " + task,
			Persona:                implPersona,
			PersonaSelectionMethod: implMethod,
			ModelTier:              string(router.TierWork),
			Status:                 core.StatusPending,
		}
		if depID != "" {
			p.Dependencies = []string{depID}
		}
		phases = append(phases, p)
	}

	// Writing phase
	if containsAny(lower, "write", "document", "blog", "post", "article") && !containsAny(lower, "write code") {
		depID := ""
		if len(phases) > 0 {
			depID = phases[len(phases)-1].ID
		}

		p := &core.Phase{
			ID:                     fmt.Sprintf("phase-%d", len(phases)+1),
			Name:                   "write",
			Objective:              "Write: " + task,
			Persona:                "technical-writer",
			PersonaSelectionMethod: "keyword",
			ModelTier:              string(router.TierWork),
			Status:                 core.StatusPending,
		}
		if depID != "" {
			p.Dependencies = []string{depID}
		}
		phases = append(phases, p)
	}

	// Review phase
	if containsAny(lower, "review", "audit", "check", "verify") {
		depID := ""
		if len(phases) > 0 {
			depID = phases[len(phases)-1].ID
		}

		// Primary: find reviewer with code review capability.
		reviewerName := pickByCapability("code review", "reviewer")
		if reviewerName == "" {
			reviewerName = "staff-code-reviewer" // fallback
		}

		p := &core.Phase{
			ID:                     fmt.Sprintf("phase-%d", len(phases)+1),
			Name:                   "review",
			Objective:              "Review: " + task,
			Persona:                reviewerName,
			PersonaSelectionMethod: "keyword",
			ModelTier:              string(router.TierWork),
			Status:                 core.StatusPending,
		}
		if depID != "" {
			p.Dependencies = []string{depID}
		}
		phases = append(phases, p)
	}

	// Fallback: single phase using target-context-biased persona selection.
	if len(phases) == 0 {
		name, method := pickPersona(task, tc)
		phases = append(phases, &core.Phase{
			ID:                     "phase-1",
			Name:                   "execute",
			Objective:              task,
			Persona:                name,
			PersonaSelectionMethod: method,
			ModelTier:              string(router.TierWork),
			Status:                 core.StatusPending,
		})
	}

	mode := "sequential"
	if hasParallelPhases(phases) {
		mode = "parallel"
	}

	plan := &core.Plan{
		ID:            fmt.Sprintf("plan_%d", time.Now().UnixNano()),
		Task:          task,
		Phases:        phases,
		ExecutionMode: mode,
		CreatedAt:     time.Now(),
	}
	ensureCodeReviewPhase(plan, tc)
	annotateRoles(plan)
	applyRuntimePolicy(plan)
	return plan
}

func ensureCodeReviewPhase(plan *core.Plan, tc *TargetContext) {
	if plan == nil {
		return
	}
	if tc != nil && tc.SkipReviewInjection {
		return
	}
	// Inject when the plan modifies code: either the task/target signals
	// implementation work (text + code-target check), or the decomposer
	// assigned implementer personas across 2+ phases. The persona-based path
	// catches missions where the target context is absent or the task
	// description doesn't match implementation keywords, but the resulting
	// multi-phase plan has code-producing personas. Single-phase plans are
	// excluded from this path (they are too simple to warrant an injected
	// review; the first path still applies when text+target signals exist).
	needsReview := taskNeedsCodeReview(plan.Task, tc) ||
		(len(plan.Phases) >= 2 && hasCodeProducingPhase(plan.Phases))
	if !needsReview || hasCodeReviewPhase(plan.Phases) {
		return
	}

	// Primary: find reviewer with code review capability.
	reviewerName := pickByCapability("code review", "reviewer")
	if reviewerName == "" {
		reviewerName = "staff-code-reviewer" // fallback
	}

	review := &core.Phase{
		ID:                     fmt.Sprintf("phase-%d", len(plan.Phases)+1),
		Name:                   "review",
		Objective:              "Review implementation for correctness, test coverage, and adherence to project conventions",
		Persona:                reviewerName,
		PersonaSelectionMethod: core.SelectionRequiredReview,
		ModelTier:              string(router.TierThink),
		Role:                   core.RoleReviewer,
		Skills:                 []string{"requesting-code-review"},
		MaxReviewLoops:         1, // allow one fix cycle; increase for stricter review loops
		Status:                 core.StatusPending,
	}

	if qaIdx := firstValidationPhaseIndex(plan.Phases); qaIdx >= 0 {
		review.Dependencies = append([]string(nil), plan.Phases[qaIdx].Dependencies...)
		plan.Phases[qaIdx].Dependencies = []string{review.ID}
		phases := append([]*core.Phase{}, plan.Phases[:qaIdx]...)
		phases = append(phases, review)
		plan.Phases = append(phases, plan.Phases[qaIdx:]...)
	} else {
		review.Dependencies = terminalPhaseIDs(plan.Phases)
		plan.Phases = append(plan.Phases, review)
	}

	if hasParallelPhases(plan.Phases) {
		plan.ExecutionMode = "parallel"
	} else {
		plan.ExecutionMode = "sequential"
	}
}

func firstValidationPhaseIndex(phases []*core.Phase) int {
	for i, p := range phases {
		if isValidationPhase(p) {
			return i
		}
	}
	return -1
}

func isValidationPhase(p *core.Phase) bool {
	if p == nil {
		return false
	}
	// Primary: check frontmatter capabilities for test-related work.
	if persona.HasCapability(p.Persona, "test design") ||
		persona.HasCapability(p.Persona, "integration testing") {
		return true
	}
	// Fallback: hardcoded slug for personas without frontmatter.
	if p.Persona == "qa-engineer" {
		return true
	}

	text := strings.ToLower(p.Name + " " + p.Objective)
	return containsAny(text, "validate", "validation", "verify", "verification", "test", "testing", "qa")
}

func terminalPhaseIDs(phases []*core.Phase) []string {
	if len(phases) == 0 {
		return nil
	}

	dependedOn := make(map[string]bool, len(phases))
	for _, p := range phases {
		for _, dep := range p.Dependencies {
			dependedOn[dep] = true
		}
	}

	var terminal []string
	for _, p := range phases {
		if !dependedOn[p.ID] {
			terminal = append(terminal, p.ID)
		}
	}
	if len(terminal) == 0 {
		return []string{phases[len(phases)-1].ID}
	}
	return terminal
}

// annotateRoles sets the Role field on every phase in a plan using
// core.ClassifyRole. Called after all phase creation/injection is complete
// so that auto-injected review phases get their roles too.
func annotateRoles(plan *core.Plan) {
	if plan == nil {
		return
	}
	for _, p := range plan.Phases {
		if p.Role == "" {
			p.Role = core.ClassifyRole(p.Persona, p.Name, p.Objective)
		}
	}
}

// applyRuntimePolicy fills in Phase.Runtime for every phase that has no
// explicit RUNTIME: field, using core.SelectRuntime keyed on each phase's
// role, persona, and objective.
//
// Must be called after annotateRoles so that Phase.Role is already populated;
// SelectRuntime uses the role as its primary signal.
//
// Phases with an explicit (non-empty) runtime are never touched — authored
// runtimes always win over policy-derived defaults.
func applyRuntimePolicy(plan *core.Plan) {
	if plan == nil {
		return
	}
	for _, p := range plan.Phases {
		if p.Runtime == "" {
			p.Runtime = core.SelectRuntime(p.Role, p.Persona, p.Objective)
			p.RuntimePolicyApplied = true
		}
	}
}

// pickPersona selects a persona for a task, preferring target-context bias signals
// (target_profile preferred personas, then routing_pattern hints) before falling
// back to LLM and keyword matching. Logs the selection source to stderr.
func pickPersona(task string, tc *TargetContext) (string, string) {
	return selectPersona(task, tc, false)
}

// resolvePersona infers a persona for a phase objective, preferring target-context
// bias signals before falling back to LLM and keyword matching. Logs when fallback fires.
func resolvePersona(objective string, tc *TargetContext) (string, string) {
	return selectPersona(objective, tc, true)
}

// selectPersona is the shared implementation for pickPersona and resolvePersona.
// isPhase controls the log message suffix (" (phase)" / " for phase").
func selectPersona(input string, tc *TargetContext, isPhase bool) (string, string) {
	limit := min(len(input), 80)
	desiredRole := core.Role("")
	if isPhase {
		desiredRole = core.ClassifyRole("", "", input)
	}
	if name, method, ok := pickFromTargetContext(input, tc, desiredRole); ok {
		suffix := ""
		if isPhase {
			suffix = " (phase)"
		}
		fmt.Fprintf(os.Stderr, "[decompose] %s%s: %q → %s\n", method, suffix, input[:limit], name)
		return name, string(method)
	}
	name, method := persona.MatchWithMethod(input)
	switch method {
	case persona.SelectionKeyword:
		suffix := ""
		if isPhase {
			suffix = " for phase"
		}
		fmt.Fprintf(os.Stderr, "[decompose] keyword fallback%s: %q → %s\n", suffix, input[:limit], name)
	case persona.SelectionFallback:
		// Alphabet-based default: override with intent-aware selection so phases
		// with obvious intent signals (review, research, write, implement) never
		// land on an arbitrary first-alphabetically persona.
		name, method = intentAwareFallback(input)
		suffix := ""
		if isPhase {
			suffix = " for phase"
		}
		fmt.Fprintf(os.Stderr, "[decompose] intent fallback%s: %q → %s\n", suffix, input[:limit], name)
	}
	return name, string(method)
}

// intentAwareFallback selects a persona based on detected task intent when
// keyword scoring produces no positive match (SelectionFallback). This prevents
// the alphabetical default ("architect") from landing on phases that carry
// obvious intent signals like review, research, write, or implement.
//
// Returns SelectionKeyword when pickImplementationPersona finds a language
// signal; SelectionFallback otherwise.
func intentAwareFallback(input string) (string, persona.SelectionMethod) {
	switch detectIntent(input) {
	case "implement":
		// Language-aware: returns SelectionKeyword when a language signal exists.
		name, method := pickImplementationPersona(input)
		return name, persona.SelectionMethod(method)
	case "review":
		// Primary: find a reviewer with "code review" capability.
		if name := pickByCapability("code review", "reviewer"); name != "" {
			return name, persona.SelectionFallback
		}
		return "staff-code-reviewer", persona.SelectionFallback
	case "research":
		return pickResearchPersona(input), persona.SelectionFallback
	case "write":
		// Primary: find a persona with documentation capabilities.
		if name := pickByCapability("API documentation", "planner"); name != "" {
			return name, persona.SelectionFallback
		}
		return "technical-writer", persona.SelectionFallback
	}
	return defaultImplementationPersona(), persona.SelectionFallback
}

// pickByCapability finds the first persona with the given capability and role.
// Returns "" when no match is found in the catalog.
func pickByCapability(capability, role string) string {
	for _, name := range persona.Names() {
		if persona.HasCapability(name, capability) &&
			(role == "" || persona.HasRole(name, role)) {
			return name
		}
	}
	return ""
}

// pickResearchPersona narrows generic "research" fallback so academic-researcher
// is only used for scholarly or evidence-heavy work. Technical comparisons and
// system-choice research route to architect; operational/metric analysis routes
// to data-analyst.
func pickResearchPersona(task string) string {
	lower := strings.ToLower(task)

	academicSignals := []string{
		"literature", "paper", "papers", "study", "studies", "scholarly",
		"peer review", "peer-reviewed", "citation", "citations", "thesis",
		"research question", "systematic review", "meta-analysis", "methodology",
		"evidence", "experiment", "experimental",
	}
	for _, signal := range academicSignals {
		if strings.Contains(lower, signal) {
			// Primary: find persona with literature review capability.
			if name := pickByCapability("literature review", ""); name != "" {
				return name
			}
			return "academic-researcher"
		}
	}

	dataSignals := []string{
		"log", "logs", "metric", "metrics", "usage", "trend", "trending",
		"error rate", "latency", "performance data", "dataset", "coverage",
		"how many", "how often",
	}
	for _, signal := range dataSignals {
		if strings.Contains(lower, signal) {
			// Primary: find persona with SQL/data capabilities.
			if name := pickByCapability("SQL queries", ""); name != "" {
				return name
			}
			return "data-analyst"
		}
	}

	// Default: find a planner with trade-off analysis capability.
	if name := pickByCapability("trade-off analysis", "planner"); name != "" {
		return name
	}
	return "architect"
}

// detectIntent infers the primary intent of a task from keyword signals.
// Returns one of: "implement", "review", "research", "write", or "" when unknown.
// The returned string is used to filter routing_pattern records by TaskHint.
// "write code" is treated as implement intent, not write intent.
func detectIntent(task string) string {
	lower := strings.ToLower(task)
	switch {
	case containsAny(lower, "literature review", "systematic review", "evidence review", "review the literature"):
		return "research"
	case containsAny(lower, "review", "audit", "check", "verify"):
		return "review"
	case containsAny(lower, "research", "analyze", "investigate", "explore", "find", "compare"):
		return "research"
	// "write code" signals implementation; check before the generic "write" case.
	// "develop" is intentionally omitted: it is a prefix of "developer" and would
	// produce false positives (e.g. "write a developer guide" → implement).
	case containsAny(lower, "implement", "build", "create", "add", "write code"):
		return "implement"
	case containsAny(lower, "write", "document", "blog", "post", "article"):
		return "write"
	default:
		return ""
	}
}

// routingPatternMatchesIntent reports whether a routing pattern's TaskHint is
// compatible with the task's detected intent.
// A pattern with an empty TaskHint matches any intent (no restriction).
// A pattern with a set TaskHint matches when the detected intent is empty (unknown)
// or when the TaskHint contains the detected intent as a substring (e.g. "implementation" ⊃ "implement").
func routingPatternMatchesIntent(taskIntent, taskHint string) bool {
	if taskHint == "" {
		return true
	}
	if taskIntent == "" {
		return true
	}
	return strings.Contains(strings.ToLower(taskHint), taskIntent)
}

// correctionMatchesIntent reports whether a routing correction should apply to a task.
// Corrections use the same intent gating as routing patterns: empty TaskHint means
// unrestricted, otherwise the correction only applies when the task intent matches.
func correctionMatchesIntent(taskIntent, taskHint string) bool {
	return routingPatternMatchesIntent(taskIntent, taskHint)
}

// pickFromTargetContext tries to find a persona from target-context bias signals.
//
// Corrections are the strongest learned signal: when an explicit correction says
// a prior persona was wrong and names the ideal replacement for a compatible task,
// use the corrected persona deterministically.
//
// When PreferredPersonas are set, they narrow the candidate set: keyword scoring
// runs over only the preferred personas in the catalog. If a preferred persona
// scores above 0 for the task, it is returned with SelectionTargetProfile.
// If no preferred persona matches the task intent, the full catalog is tried next
// (routing patterns, then unrestricted matching) so that the correct task-intent
// persona is not suppressed by the target profile.
//
// Priority when preferred personas exist:
//  1. matching routing correction → SelectionCorrection
//  2. matching role persona hint (phase-only) → SelectionRoutingPattern
//  3. Keyword match within preferred set → SelectionTargetProfile
//  4. routing_pattern hints (intent-filtered) → SelectionRoutingPattern
//  5. Return false → caller falls through to full-catalog MatchWithMethod
//
// When no preferred personas are set, corrections are tried first, then role
// persona hints (phase-only), then routing patterns. Corrections keep highest
// priority so explicit "persona X was wrong here" signals are never overridden
// by historical role frequency.
//
// Both corrections and routing patterns are filtered by intent: a record whose
// TaskHint does not match the task's detected intent is skipped entirely.
func pickFromTargetContext(input string, tc *TargetContext, desiredRole core.Role) (string, persona.SelectionMethod, bool) {
	if tc == nil {
		return "", "", false
	}
	taskIntent := detectIntent(input)

	// Explicit corrections are the highest-signal learned input. If a correction
	// matches this task shape and names a valid ideal persona, use it directly.
	for _, c := range tc.RoutingCorrections {
		if c.IdealPersona == "" || persona.Get(c.IdealPersona) == nil {
			continue
		}
		if correctionMatchesIntent(taskIntent, c.TaskHint) {
			return c.IdealPersona, persona.SelectionCorrection, true
		}
	}

	// Role-aware patterns are only meaningful for phase-level selection where the
	// desired role is known. They are stronger than preferred personas because
	// they describe which persona has historically performed this role well for
	// this target.
	if desiredRole != "" && len(tc.RolePersonaHints) > 0 {
		for _, h := range tc.RolePersonaHints {
			if h.Role != string(desiredRole) {
				continue
			}
			if persona.Get(h.Persona) == nil {
				continue
			}
			return h.Persona, persona.SelectionRoutingPattern, true
		}
	}

	// Narrow to preferred personas and run keyword scoring within that set.
	// MatchWithMethodCandidates skips unknown names, so no pre-filtering needed.
	// If a preferred persona scores above 0, task intent matched within the target's
	// preferred set — use it. If nothing scores, fall through so that the correct
	// reviewer/security/etc. persona is not suppressed by the target profile.
	if len(tc.PreferredPersonas) > 0 {
		name, method := persona.MatchWithMethodCandidates(input, tc.PreferredPersonas)
		if method == persona.SelectionKeyword && preferredPersonaMatchesIntent(name, taskIntent) {
			return name, persona.SelectionTargetProfile, true
		}
		// No preferred persona matched task intent — fall through to routing
		// patterns and then the full catalog.
	}
	// routing_pattern: TopPatterns ordered by confidence desc; take the first valid one
	// whose TaskHint is compatible with the task's detected intent.
	// A pattern whose hint conflicts with the task intent is skipped so that a
	// high-confidence review pattern never overrides an implementation task.
	if len(tc.TopPatterns) > 0 {
		for _, h := range tc.TopPatterns {
			if persona.Get(h.Persona) == nil {
				continue
			}
			if routingPatternMatchesIntent(taskIntent, h.TaskHint) {
				return h.Persona, persona.SelectionRoutingPattern, true
			}
		}
	}
	return "", "", false
}

func preferredPersonaMatchesIntent(name, taskIntent string) bool {
	if taskIntent == "" {
		return true
	}

	switch taskIntent {
	case "review":
		return core.ClassifyRole(name, "", "") == core.RoleReviewer
	case "write":
		// Primary: check for documentation capabilities.
		if persona.HasCapability(name, "API documentation") ||
			persona.HasCapability(name, "developer guides") {
			return true
		}
		// Fallback.
		return name == "technical-writer"
	case "research":
		// Primary: check for research-related capabilities.
		if persona.HasCapability(name, "literature review") ||
			persona.HasCapability(name, "trade-off analysis") ||
			persona.HasCapability(name, "SQL queries") {
			return true
		}
		// Fallback.
		return name == "academic-researcher" || name == "architect" || name == "data-analyst"
	case "implement":
		// Primary: check frontmatter role=implementer.
		if persona.HasRole(name, "implementer") {
			return true
		}
		// Fallback.
		switch name {
		case "senior-backend-engineer", "senior-frontend-engineer", "devops-engineer":
			return true
		default:
			return false
		}
	default:
		return true
	}
}

// pickImplementationPersona selects the best implementation persona for a task.
// It detects language signals and prefers the most appropriate current catalog
// implementer. Falls back to the first implementer in the catalog (or
// "senior-backend-engineer") with SelectionFallback when no signal is found.
func pickImplementationPersona(task string) (string, string) {
	lower := strings.ToLower(task)
	for _, candidate := range languagePersonaCandidates(lower) {
		return candidate, string(persona.SelectionKeyword)
	}
	return defaultImplementationPersona(), string(persona.SelectionFallback)
}

func defaultImplementationPersona() string {
	// Preserve the established general-purpose implementation fallback when it is
	// present in the catalog; role metadata should not silently redirect generic
	// coding work to QA or infrastructure specialists.
	if persona.Get("senior-backend-engineer") != nil {
		return "senior-backend-engineer"
	}
	if names := persona.NamesWithRole("implementer", nil); len(names) > 0 {
		return names[0]
	}
	return "senior-backend-engineer"
}

// languagePersonaCandidates returns candidate persona names for language-specific
// implementation work inferred from signals in the lowercased task text.
// Returns an empty slice when no language signal is detected.
func languagePersonaCandidates(lower string) []string {
	type signal struct {
		patterns []string
		persona  string
	}
	signals := []signal{
		// Go: "golang" is unambiguous; " go " / " in go" catch "implement X in Go"
		// and "build a go cli tool"; "goroutine" and "go mod" are Go-specific.
		{
			patterns: []string{"golang", " go ", " in go", "goroutine", "go mod"},
			persona:  "senior-backend-engineer",
		},
		// Rust: use word-boundary matching for bare "rust" so "trust"/"Zero Trust"
		// do not false-positive; "cargo" is Rust's build tool; "tokio" is the
		// dominant async runtime.
		{
			patterns: []string{"cargo", "crate", "tokio"},
			persona:  "senior-backend-engineer",
		},
		// TypeScript/React/Next.js: "typescript" and ".tsx" are unambiguous;
		// "nextjs" and "react" cover the dominant UI frameworks; "npm " (with
		// trailing space) avoids matching "example" or "environment".
		{
			patterns: []string{"typescript", ".tsx", "nextjs", "next.js", "react/", " react ", "npm "},
			persona:  "senior-frontend-engineer",
		},
		// Python: "python" is unambiguous; "fastapi" and "django" name the two
		// dominant Python web frameworks; "pytest" is Python-specific.
		{
			patterns: []string{"python", "fastapi", "django", "pytest", " pip "},
			persona:  "senior-backend-engineer",
		},
	}

	var candidates []string
	for _, s := range signals {
		if rustWordRE.MatchString(lower) && containsAny(strings.Join(s.patterns, " "), "cargo", "crate", "tokio") {
			candidates = append(candidates, s.persona)
			continue
		}
		for _, pat := range s.patterns {
			if strings.Contains(lower, pat) {
				candidates = append(candidates, s.persona)
				break
			}
		}
	}
	return candidates
}

func containsAny(s string, substrs ...string) bool {
	for _, sub := range substrs {
		if strings.Contains(s, sub) {
			return true
		}
	}
	return false
}
