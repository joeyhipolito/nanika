package audit

import "time"

// AuditReport is the top-level output of a mission audit.
type AuditReport struct {
	WorkspaceID string            `json:"workspace_id"`
	Task        string            `json:"task"`
	Domain      string            `json:"domain"`
	Status      string            `json:"status"` // from checkpoint: completed, failed
	AuditedAt   time.Time         `json:"audited_at"`
	// Linking metadata — present when the source mission carried frontmatter with
	// linear_issue_id, or when run from a named .md file.
	LinearIssueID string `json:"linear_issue_id,omitempty"` // e.g. "V-5"
	MissionPath   string `json:"mission_path,omitempty"`    // absolute path to source mission file
	Scorecard   Scorecard         `json:"scorecard"`
	Evaluation  MissionEvaluation `json:"evaluation"`
	Phases      []PhaseEvaluation `json:"phases"`
	Convergence          ConvergenceStatus     `json:"convergence"`
	DecomposerConvergence DecomposerConvergence `json:"decomposer_convergence"`
	Changes              []ChangeRecord        `json:"changes"`
}

// Scorecard is the quantitative summary — 5 axes, each 1-5.
type Scorecard struct {
	DecompositionQuality int `json:"decomposition_quality"` // Was the task broken down well?
	PersonaFit           int `json:"persona_fit"`            // Were the right personas assigned?
	SkillUtilization     int `json:"skill_utilization"`      // Were available skills used effectively?
	OutputQuality        int `json:"output_quality"`         // Did outputs meet objectives?
	RuleCompliance       int `json:"rule_compliance"`        // Did decomposition follow SKILL.md rules?
	Overall              int `json:"overall"`                // Computed: (sum of 5) / 5
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
	Score           int      `json:"score"` // 1-5
}

// Recommendation is a specific, actionable improvement.
type Recommendation struct {
	Category string `json:"category"` // decomposition, persona, skill, process
	Priority string `json:"priority"` // high, medium, low
	Summary  string `json:"summary"`
	Detail   string `json:"detail"`
}

// ChangeRecord captures a concrete change made during the mission.
type ChangeRecord struct {
	PhaseID   string `json:"phase_id"`
	PhaseName string `json:"phase_name"`
	Type      string `json:"type"`    // file_created, file_modified, command_run
	Target    string `json:"target"`  // file path, command
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
	SKILLMDHash    string `json:"skill_md_hash"`    // SHA-256 of SKILL.md content, empty if not found
	PromptSource   string `json:"prompt_source"`     // "skill_md" or "hardcoded_fallback"
	SKILLMDPath    string `json:"skill_md_path"`     // path that was found, empty if fallback
	RulesExtracted bool   `json:"rules_extracted"`   // whether extraction succeeded
}

// DecompositionFinding is a single structural observation about a mission's
// decomposition, extracted from audit data (ConvergenceStatus + PhaseEvaluation).
// These findings feed the decomposition learning loop.
type DecompositionFinding struct {
	FindingType  string `json:"finding_type"`  // missing_phase, redundant_phase, phase_drift, wrong_persona, low_phase_score
	PhaseName    string `json:"phase_name"`    // phase this finding relates to, or "" for plan-level findings
	Detail       string `json:"detail"`        // human-readable description
	DecompSource string `json:"decomp_source"` // from Plan.DecompSource at audit time
	AuditScore   int    `json:"audit_score"`   // overall scorecard score
}

// Finding type constants for DecompositionFinding.FindingType.
const (
	FindingMissingPhase   = "missing_phase"
	FindingRedundantPhase = "redundant_phase"
	FindingPhaseDrift     = "phase_drift"
	FindingWrongPersona   = "wrong_persona"
	FindingLowPhaseScore  = "low_phase_score"
)

// llmReport is the shape we ask the LLM to produce. Parsed from JSON in fences.
// Matches the prompt schema exactly.
type llmReport struct {
	Scorecard   Scorecard         `json:"scorecard"`
	Evaluation  MissionEvaluation `json:"evaluation"`
	Phases      []PhaseEvaluation `json:"phases"`
	Convergence ConvergenceStatus `json:"convergence"`
}
