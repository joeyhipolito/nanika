package core

import (
	"fmt"
	"time"
)

// Plan represents a decomposed mission with phases.
type Plan struct {
	ID            string    `json:"id"`
	Task          string    `json:"task"`
	Phases        []*Phase  `json:"phases"`
	ExecutionMode string    `json:"execution_mode"` // "parallel" or "sequential"
	DecompSource  string    `json:"decomp_source"`  // "predecomposed", "llm", "keyword", "template"
	CreatedAt     time.Time `json:"created_at"`
}

// Decomposition source constants for Plan.DecompSource.
const (
	DecompPredecomposed = "predecomposed"  // human-written PHASE lines in mission file
	DecompLLM           = "decomp.llm"     // LLM-based decomposition
	DecompKeyword       = "decomp.keyword" // keyword-matching fallback
	DecompTemplate      = "template"       // loaded from a saved template
)

// SelectionRequiredReview is the PersonaSelectionMethod value that marks a
// phase as a mandatory code-review gate. The engine uses this to distinguish
// auto-injected review gates (which trigger the fix loop) from user-added
// phases that happen to use the staff-code-reviewer persona.
const SelectionRequiredReview = "required_review"

// Phase represents a single unit of work in a plan.
type Phase struct {
	ID           string   `json:"id"`
	Name         string   `json:"name"`
	Objective    string   `json:"objective"`
	Persona      string   `json:"persona"`      // persona key (e.g., "architect", "implementer")
	ModelTier    string   `json:"model_tier"`   // "think", "work", "quick"
	Skills       []string `json:"skills"`       // skill names from via catalog
	Constraints  []string `json:"constraints"`  // what NOT to do
	Dependencies []string `json:"dependencies"` // phase IDs that must complete first
	Expected     string   `json:"expected"`     // expected output description
	// Role is the orchestrator-level function this phase serves: planner,
	// implementer, or reviewer. Populated during decomposition by ClassifyRole.
	// Used by the engine to generate structured handoff context between phases.
	Role Role `json:"role,omitempty"`
	// TargetDir is the working directory where this phase's worker executes.
	// When set, the worker process runs with this as its CWD (e.g. the target git repo)
	// while still writing artifacts and mission state to WorkerDir.
	// Populated from WORKDIR: in pre-decomposed PHASE lines, or inherited from Workspace.TargetDir.
	// Empty means the worker executes in its own WorkerDir (legacy behaviour).
	TargetDir string `json:"target_dir,omitempty"`
	// Runtime identifies which execution backend runs this phase.
	// The zero value ("") is treated as RuntimeClaude for backward compatibility,
	// so existing plans that omit this field continue to use the Claude Code CLI.
	Runtime Runtime `json:"runtime,omitempty"`
	// RuntimePolicyApplied is true when Runtime was filled by orchestrator policy
	// rather than authored explicitly in the plan or mission.
	RuntimePolicyApplied bool `json:"runtime_policy_applied,omitempty"`
	// StallTimeout is the per-phase watchdog stall timeout. When non-zero it
	// overrides the global ORCHESTRATOR_STALL_TIMEOUT env var and the
	// --stall-timeout flag for this specific phase.
	// Populated from TIMEOUT: in pre-decomposed PHASE lines (e.g. TIMEOUT: 30m).
	StallTimeout time.Duration `json:"stall_timeout,omitempty"`

	// Runtime state
	Status    PhaseStatus `json:"status"`
	Output    string      `json:"output,omitempty"`
	Error     string      `json:"error,omitempty"`
	StartTime *time.Time  `json:"start_time,omitempty"`
	EndTime   *time.Time  `json:"end_time,omitempty"`

	// Completion signal (populated by engine after ReadSignalFile)
	SignalRemainder string `json:"signal_remainder,omitempty"` // partial signal: unfinished work for dependents

	// Execution tracking (populated during executePhase)
	Retries                int      `json:"retries,omitempty"`
	GatePassed             bool     `json:"gate_passed,omitempty"`
	OutputLen              int      `json:"output_len,omitempty"`
	ParsedSkills           []string `json:"parsed_skills,omitempty"`
	LearningsRetrieved     int      `json:"learnings_retrieved,omitempty"`
	SessionID              string   `json:"session_id,omitempty"`               // Claude session ID from last worker run
	PersonaSelectionMethod string   `json:"persona_selection_method,omitempty"` // "llm" or "keyword"
	// Cost attribution (accumulated across retries; populated from Claude CLI ResultMessage)
	Model     string  `json:"model,omitempty"`      // resolved model ID (e.g. "claude-sonnet-4-6")
	TokensIn  int     `json:"tokens_in,omitempty"`  // total input tokens across all attempts
	TokensOut int     `json:"tokens_out,omitempty"` // total output tokens across all attempts
	CostUSD   float64 `json:"cost_usd,omitempty"`   // total cost in USD across all attempts

	// Review loop tracking (populated by engine/review_loop.go)
	ReviewIteration int      `json:"review_iteration,omitempty"` // 0 = first review pass, 1 = after first fix
	OriginPhaseID   string   `json:"origin_phase_id,omitempty"`  // for fix phases: the impl phase being fixed
	MaxReviewLoops  int      `json:"max_review_loops,omitempty"` // 0 = use engine default (1)
	ReviewBlockers  []string `json:"review_blockers,omitempty"`  // latest parsed blocker findings for this review phase
	ReviewWarnings  []string `json:"review_warnings,omitempty"`  // latest parsed non-blocking findings for this review phase

	// ChangedFiles holds the list of files modified by this phase relative to
	// the base branch. Populated by the engine after successful completion when
	// the phase ran in a git worktree. Used for cross-phase overlap detection.
	ChangedFiles []string `json:"changed_files,omitempty"`
}

// FileOverlap records a file that was modified by more than one parallel phase.
type FileOverlap struct {
	File     string   // repository-relative path of the conflicting file
	Phases   []string // IDs of phases that modified this file
	Severity string   // "high" if both modified an existing file, "medium" if one created it
}

// PhaseStatus tracks phase execution state.
type PhaseStatus string

const (
	StatusPending   PhaseStatus = "pending"
	StatusRunning   PhaseStatus = "running"
	StatusCompleted PhaseStatus = "completed"
	StatusFailed    PhaseStatus = "failed"
	StatusSkipped   PhaseStatus = "skipped"
)

// IsTerminal reports whether s is a terminal state — one that a phase can
// never legitimately leave. Terminal states are completed, failed, and skipped.
// Pending and running are non-terminal (the phase can still make progress).
//
// Use IsTerminal as the canonical guard anywhere the engine or tests need to
// prevent overwriting a phase that has already reached its final state.
func (s PhaseStatus) IsTerminal() bool {
	return s == StatusCompleted || s == StatusFailed || s == StatusSkipped
}

// ValidatePhaseIDs returns an error if any two phases in the plan share the
// same ID. Called by the engine before dispatch to enforce the uniqueness
// invariant that phase IDs serve as primary keys in the phase index map.
func ValidatePhaseIDs(plan *Plan) error {
	if plan == nil {
		return nil
	}
	seen := make(map[string]int, len(plan.Phases))
	for i, p := range plan.Phases {
		if prev, ok := seen[p.ID]; ok {
			return fmt.Errorf("duplicate phase ID %q: phases[%d] (%s) and phases[%d] (%s)",
				p.ID, prev, plan.Phases[prev].Name, i, p.Name)
		}
		seen[p.ID] = i
	}
	return nil
}

// ContextBundle is what each worker receives — everything it needs to do its job.
type ContextBundle struct {
	Objective      string   // what to accomplish
	Persona        string   // persona prompt text (the full constant)
	PersonaName    string   // persona key for logging
	MissionContext string   // key-value pairs extracted from the mission task header
	ModelTier      string   // think/work/quick
	Skills         []Skill  // inlined skill content (phase-specific)
	SkillIndex     string   // full skill routing index (all available tools)
	Constraints    []string // guardrails
	PriorContext   string   // output from dependency phases
	Learnings      string   // relevant learnings from DB
	Domain         string   // dev/personal/work/creative/academic
	WorkspaceID    string   // workspace identifier
	PhaseID        string   // phase identifier
	// TargetDir is the target repo path where the worker executes (may be empty).
	// When non-empty, WorkerDir holds the path to which the worker should write artifacts.
	TargetDir string // target working directory (e.g. /Users/joey/skills/orchestrator)
	WorkerDir string // artifact output directory (worker's own subdir under workspace)
	// Handoffs holds structured handoff records from dependency phases whose role
	// differs from this phase's role (e.g., planner→implementer). Populated by
	// the engine when building the context bundle, injected into CLAUDE.md.
	Handoffs []HandoffRecord
	// Role is the orchestrator-level role this phase serves (planner, implementer,
	// reviewer). Propagated into CLAUDE.md so the worker is aware of its contract.
	Role Role
	// Runtime is the execution backend for this phase. Propagated into CLAUDE.md
	// for worker awareness.
	Runtime Runtime
}

// Skill represents an inlined skill reference.
type Skill struct {
	Name             string   // e.g., "obsidian"
	CommandReference string   // extracted command section from SKILL.md
	EnvVars          []string // env var names this skill needs passed to subprocesses
}

// WorkerConfig holds configuration for spawning a worker.
type WorkerConfig struct {
	Name            string // e.g., "architect-01"
	WorkerDir       string // full path to worker's artifact directory (always under workspace)
	TargetDir       string // CWD for worker execution; empty → use WorkerDir
	Model           string // resolved model ID
	EffortLevel     string // Claude Code effort level: low, medium, high
	MaxTurns        int    // max agentic turns; 0 means use engine default (50)
	StallTimeout    time.Duration // watchdog stall timeout; 0 means use global default
	ResumeSessionID string // if set, passed to AgentOptions to resume a prior Claude session
	Bundle          ContextBundle
	HookScript      string // generated stop.sh content
}

// ExecutionResult is the outcome of running a mission.
type ExecutionResult struct {
	Plan      *Plan         `json:"plan"`
	Success   bool          `json:"success"`
	Output    string        `json:"output"`
	Artifacts []string      `json:"artifacts"`
	Duration  time.Duration `json:"duration"`
	Error     string        `json:"error,omitempty"`
}

// GateMode controls what happens when the quality gate fails after a phase completes.
type GateMode string

const (
	// GateModeWarn logs a warning when the gate fails but lets the phase succeed (fail-forward).
	GateModeWarn GateMode = "warn"
	// GateModeBlock returns an error when the gate fails, causing the phase to fail.
	GateModeBlock GateMode = "block"
)

// OrchestratorConfig holds runtime configuration.
type OrchestratorConfig struct {
	MaxConcurrent    int           // max parallel workers (default 3)
	Timeout          time.Duration // per-phase timeout (default 15min)
	Verbose          bool
	DryRun           bool
	ForcedModel      string        // override model for all phases
	ForceSequential  bool          // force sequential execution
	Domain           string        // dev/personal/work/creative/academic
	MaxTurns         int           // max agentic turns per worker (default 50)
	DisableLearnings bool          // skip learning retrieval and injection
	GateMode         GateMode      // warn (fail-forward) or block (fail phase); default block
	// StallTimeout is the global watchdog stall timeout applied to all phases
	// that do not specify their own TIMEOUT: field. Overrides ORCHESTRATOR_STALL_TIMEOUT.
	// Zero means fall back to ORCHESTRATOR_STALL_TIMEOUT or the 5-minute default.
	StallTimeout     time.Duration
}
