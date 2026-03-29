package core

import (
	"strings"

	"github.com/joeyhipolito/orchestrator-cli/internal/persona"
)

// Role represents the orchestrator-level function a phase serves in the
// plan/implement/review lifecycle. Unlike personas (which describe *who* does
// the work), roles describe *what kind* of work is being done — planning vs.
// implementing vs. reviewing. A single persona can serve different roles in
// different phases: an architect plans, a backend-engineer implements, a
// staff-code-reviewer reviews.
type Role string

const (
	// RolePlanner is assigned to phases that design, architect, research, or
	// plan before implementation work begins. Planner output typically becomes
	// the specification that implementers consume.
	RolePlanner Role = "planner"

	// RoleImplementer is assigned to phases that produce artifacts: code,
	// content, configuration, tests. This is the default role when no
	// stronger signal exists.
	RoleImplementer Role = "implementer"

	// RoleReviewer is assigned to phases that evaluate implementer output:
	// code review, security audit, QA validation. Reviewer output may trigger
	// a fix loop back to the implementer.
	RoleReviewer Role = "reviewer"
)

// ClassifyRole determines the orchestrator role for a phase based on its
// persona name, phase name, and objective. The classification is heuristic
// and deliberately conservative: ambiguous cases default to RoleImplementer.
//
// The priority order is:
//  1. Persona name (strongest signal — reviewer/auditor personas are definitive)
//  2. Phase name (explicit naming like "review", "plan", "design")
//  3. Objective keywords (weakest signal, only used when 1 and 2 are ambiguous)
func ClassifyRole(persona, phaseName, objective string) Role {
	// 1. Persona-based classification (strongest signal)
	if role, ok := classifyByPersona(persona); ok {
		return role
	}

	// 2. Phase name-based classification
	if role, ok := classifyByPhaseName(phaseName); ok {
		return role
	}

	// 3. Objective keyword classification (weakest signal)
	if role, ok := classifyByObjective(objective); ok {
		return role
	}

	return RoleImplementer
}

// reviewerPersonasFallback is used when no persona in the catalog has frontmatter.
var reviewerPersonasFallback = map[string]bool{
	"staff-code-reviewer": true,
	"security-auditor":    true,
	"qa-engineer":         true,
}

// plannerPersonasFallback is used when no persona in the catalog has frontmatter.
var plannerPersonasFallback = map[string]bool{
	"architect":           true,
	"system-architect":    true,
	"solutions-architect": true,
}

func classifyByPersona(name string) (Role, bool) {
	p := strings.ToLower(name)

	// Primary path: read role from persona frontmatter metadata.
	if persona.HasRole(p, "reviewer") {
		return RoleReviewer, true
	}
	if persona.HasRole(p, "planner") {
		return RolePlanner, true
	}
	if persona.HasRole(p, "implementer") {
		return RoleImplementer, true
	}

	// Fallback: hardcoded maps for personas without frontmatter.
	if reviewerPersonasFallback[p] {
		return RoleReviewer, true
	}
	if plannerPersonasFallback[p] {
		return RolePlanner, true
	}
	return "", false
}

// reviewPhaseNames are phase name substrings that indicate review work.
var reviewPhaseNames = []string{"review", "audit", "validate", "verification", "qa"}

// plannerPhaseNames are phase name substrings that indicate planning work.
var plannerPhaseNames = []string{"plan", "design", "architect", "research", "investigate", "analyze", "discovery"}

func classifyByPhaseName(name string) (Role, bool) {
	lower := strings.ToLower(name)
	for _, kw := range reviewPhaseNames {
		if strings.Contains(lower, kw) {
			return RoleReviewer, true
		}
	}
	for _, kw := range plannerPhaseNames {
		if strings.Contains(lower, kw) {
			return RolePlanner, true
		}
	}
	return "", false
}

// reviewObjectiveKeywords are objective substrings that indicate review work.
var reviewObjectiveKeywords = []string{
	"review implementation", "review the", "audit the", "validate the",
	"check for regressions", "evaluate the", "assess the quality",
}

// plannerObjectiveKeywords are objective substrings that indicate planning work.
var plannerObjectiveKeywords = []string{
	"design the", "plan the", "architect the", "research how",
	"investigate the", "analyze the", "create a plan", "draft a specification",
}

func classifyByObjective(objective string) (Role, bool) {
	lower := strings.ToLower(objective)
	for _, kw := range reviewObjectiveKeywords {
		if strings.Contains(lower, kw) {
			return RoleReviewer, true
		}
	}
	for _, kw := range plannerObjectiveKeywords {
		if strings.Contains(lower, kw) {
			return RolePlanner, true
		}
	}
	return "", false
}

// HandoffRecord captures the structured context passed between phases with
// different roles. It records what was produced by the source phase, what
// constraints/expectations the receiving phase should honor, and the role
// transition (e.g., planner→implementer, implementer→reviewer).
type HandoffRecord struct {
	// FromPhaseID is the phase that produced the handoff.
	FromPhaseID string `json:"from_phase_id"`
	// ToPhaseID is the phase that receives the handoff.
	ToPhaseID string `json:"to_phase_id"`
	// FromRole is the role of the source phase.
	FromRole Role `json:"from_role"`
	// ToRole is the role of the receiving phase.
	ToRole Role `json:"to_role"`
	// FromPersona is the persona of the source phase.
	FromPersona string `json:"from_persona"`
	// ToPersona is the persona of the receiving phase.
	ToPersona string `json:"to_persona"`
	// Summary is a concise description of what was handed off.
	Summary string `json:"summary"`
	// Expectations lists what the receiving phase should honor or deliver.
	Expectations []string `json:"expectations,omitempty"`
}

// FormatForWorker renders the handoff record as a markdown section suitable
// for injection into CLAUDE.md. It gives the worker explicit awareness of
// its role in the plan/implement/review lifecycle and what prior work it
// should build on or evaluate.
func (h HandoffRecord) FormatForWorker() string {
	var b strings.Builder
	b.WriteString("### Role Handoff\n\n")
	b.WriteString("You are the **")
	b.WriteString(string(h.ToRole))
	b.WriteString("** in this workflow. The preceding **")
	b.WriteString(string(h.FromRole))
	b.WriteString("** (")
	b.WriteString(h.FromPersona)
	b.WriteString(") has completed their work.\n\n")

	if h.Summary != "" {
		b.WriteString("**What was handed off:** ")
		b.WriteString(h.Summary)
		b.WriteString("\n\n")
	}

	if len(h.Expectations) > 0 {
		b.WriteString("**Your responsibilities:**\n")
		for _, exp := range h.Expectations {
			b.WriteString("- ")
			b.WriteString(exp)
			b.WriteString("\n")
		}
		b.WriteString("\n")
	}

	return b.String()
}

// RoleTransitionExpectations returns default expectations for a given role
// transition. These are injected into handoff records when no phase-specific
// expectations are available.
func RoleTransitionExpectations(from, to Role) []string {
	key := string(from) + "→" + string(to)
	switch key {
	case "planner→implementer":
		return []string{
			"Follow the design/plan from the prior phase — do not redesign",
			"Implement all specified requirements; flag any ambiguities as DECISION: markers",
			"Produce working code/artifacts, not just a plan",
		}
	case "implementer→reviewer":
		return []string{
			"Review the implementation for correctness, security, and maintainability",
			"Produce structured findings with ### Blockers and ### Warnings sections",
			"Do not implement fixes yourself — report findings for the implementer to address",
		}
	case "reviewer→implementer":
		return []string{
			"Address all blockers identified in the review",
			"Do not introduce new features — only fix the reported issues",
			"Preserve the existing architecture and patterns",
		}
	case "planner→reviewer":
		return []string{
			"Evaluate whether the plan is complete, feasible, and addresses the mission objective",
			"Check for missing considerations, security gaps, or architectural risks",
		}
	default:
		return nil
	}
}
