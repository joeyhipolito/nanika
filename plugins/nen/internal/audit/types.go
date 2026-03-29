// Package audit provides standalone audit evaluation, reporting, and apply
// logic for nen binaries. Types are wire-compatible with the orchestrator's
// audit reports (serialized to ~/.alluka/audits.jsonl).
package audit

import "time"

// AuditReport is the top-level output of a mission audit.
type AuditReport struct {
	WorkspaceID   string            `json:"workspace_id"`
	Task          string            `json:"task"`
	Domain        string            `json:"domain"`
	Status        string            `json:"status"`
	AuditedAt     time.Time         `json:"audited_at"`
	LinearIssueID string            `json:"linear_issue_id,omitempty"`
	MissionPath   string            `json:"mission_path,omitempty"`
	Scorecard     Scorecard         `json:"scorecard"`
	Evaluation    MissionEvaluation `json:"evaluation"`
	Phases        []PhaseEvaluation `json:"phases"`
	Convergence          ConvergenceStatus     `json:"convergence"`
	DecomposerConvergence DecomposerConvergence `json:"decomposer_convergence"`
	Changes              []ChangeRecord        `json:"changes"`
}

// Scorecard is the quantitative summary — 5 axes, each 1-5.
type Scorecard struct {
	DecompositionQuality int `json:"decomposition_quality"`
	PersonaFit           int `json:"persona_fit"`
	SkillUtilization     int `json:"skill_utilization"`
	OutputQuality        int `json:"output_quality"`
	RuleCompliance       int `json:"rule_compliance"`
	Overall              int `json:"overall"`
}

// MissionEvaluation is the LLM's qualitative assessment of the mission.
type MissionEvaluation struct {
	Summary         string           `json:"summary"`
	Strengths       []string         `json:"strengths"`
	Weaknesses      []string         `json:"weaknesses"`
	Recommendations []Recommendation `json:"recommendations"`
}

// PhaseEvaluation is the LLM's assessment of a single phase.
type PhaseEvaluation struct {
	PhaseID         string   `json:"phase_id"`
	PhaseName       string   `json:"phase_name"`
	PersonaAssigned string   `json:"persona_assigned"`
	PersonaIdeal    string   `json:"persona_ideal"`
	PersonaCorrect  bool     `json:"persona_correct"`
	ObjectiveMet    bool     `json:"objective_met"`
	Issues          []string `json:"issues"`
	Score           int      `json:"score"`
}

// Recommendation is a specific, actionable improvement.
type Recommendation struct {
	Category string `json:"category"`
	Priority string `json:"priority"`
	Summary  string `json:"summary"`
	Detail   string `json:"detail"`
}

// ChangeRecord captures a concrete change made during the mission.
type ChangeRecord struct {
	PhaseID   string `json:"phase_id"`
	PhaseName string `json:"phase_name"`
	Type      string `json:"type"`
	Target    string `json:"target"`
	Summary   string `json:"summary"`
}

// ConvergenceStatus evaluates whether execution matched the decomposer's plan.
type ConvergenceStatus struct {
	Converged     bool     `json:"converged"`
	DriftPhases   []string `json:"drift_phases"`
	MissingPhases []string `json:"missing_phases"`
	RedundantWork []string `json:"redundant_work"`
	Assessment    string   `json:"assessment"`
}

// DecomposerConvergence tracks whether the decomposer used SKILL.md rules
// or fell back to hardcoded rules.
type DecomposerConvergence struct {
	SKILLMDHash    string `json:"skill_md_hash"`
	PromptSource   string `json:"prompt_source"`
	SKILLMDPath    string `json:"skill_md_path"`
	RulesExtracted bool   `json:"rules_extracted"`
}

// llmReport is the shape we ask the LLM to produce. Parsed from JSON in fences.
type llmReport struct {
	Scorecard   Scorecard         `json:"scorecard"`
	Evaluation  MissionEvaluation `json:"evaluation"`
	Phases      []PhaseEvaluation `json:"phases"`
	Convergence ConvergenceStatus `json:"convergence"`
}

// ── Checkpoint types for standalone loading ──────────────────────────────────

// Checkpoint is the minimal subset of orchestrator's checkpoint.json we need.
type Checkpoint struct {
	WorkspaceID   string `json:"workspace_id"`
	Domain        string `json:"domain"`
	Plan          *Plan  `json:"plan"`
	Status        string `json:"status"`
	LinearIssueID string `json:"linear_issue_id,omitempty"`
	MissionPath   string `json:"mission_path,omitempty"`
}

// Plan is the minimal subset of orchestrator's plan type.
type Plan struct {
	Task          string   `json:"task"`
	Phases        []*Phase `json:"phases"`
	ExecutionMode string   `json:"execution_mode"`
}

// Phase is the minimal subset of orchestrator's phase type.
type Phase struct {
	ID           string   `json:"id"`
	Name         string   `json:"name"`
	Objective    string   `json:"objective"`
	Persona      string   `json:"persona"`
	ModelTier    string   `json:"model_tier"`
	Skills       []string `json:"skills"`
	Dependencies []string `json:"dependencies"`
	Status       string   `json:"status"`
	Output       string   `json:"output,omitempty"`
}
