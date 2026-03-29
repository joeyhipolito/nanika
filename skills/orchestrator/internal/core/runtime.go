package core

import (
	"fmt"
	"slices"
	"strings"
	"unicode"
)

// Runtime identifies which execution backend handles a phase.
// The zero value ("") is treated as RuntimeClaude for backward compatibility,
// so existing plan.json files and pre-decomposed PHASE lines that omit the
// runtime field continue to execute through the Claude Code CLI unchanged.
type Runtime string

const (
	// RuntimeClaude routes the phase through the Claude Code CLI subprocess.
	// This is the default runtime for all phases and is used whenever Runtime
	// is empty or explicitly set to "claude".
	RuntimeClaude Runtime = "claude"

	// RuntimeCodex routes the phase through the OpenAI Codex CLI subprocess
	// (codex exec). Phases assigned this runtime bypass the Claude Code path
	// entirely; the CodexExecutor must be registered before Execute is called.
	RuntimeCodex Runtime = "codex"
)

// Effective returns RuntimeClaude when r is the zero value (""), preserving
// backward compatibility for plans that pre-date explicit runtime ownership.
func (r Runtime) Effective() Runtime {
	if r == "" {
		return RuntimeClaude
	}
	return r
}

// SelectRuntime derives the runtime for a phase whose RUNTIME: field was not
// explicitly authored. It applies a three-signal heuristic — role, persona,
// and task-text shape — so the plan.json always carries a resolved, non-zero
// runtime after decomposition, removing ambiguity between "author chose Claude"
// and "runtime was never set".
//
// Heuristic slice (signals applied in priority order):
//
//  1. Role gates the branch:
//     - RoleReviewer → RuntimeClaude always; review needs Claude's analytical
//     reasoning, not a code-generation backend.
//     - RolePlanner  → RuntimeClaude always; planning requires multi-turn
//     conversational synthesis that Codex does not provide.
//     - RoleImplementer → evaluated further by persona + task shape.
//     - unknown/empty → RuntimeClaude (safe default).
//
//  2. Task-text shape: within RoleImplementer, obvious non-code tasks
//     (research, docs, writing, deployment) always stay on Claude regardless
//     of persona, because Codex provides no advantage for those shapes.
//
//  3. Persona: Codex is auto-selected only when the persona
//     explicitly names a programming language (e.g. "golang", "typescript"),
//     making the language target unambiguous. Generic roles ("backend-engineer",
//     "fullstack-engineer") stay on Claude.
//
// Codex auto-selection is currently disabled. Explicit authored runtimes still
// win, but policy-derived routing always resolves to RuntimeClaude.
//
// Callers must only invoke SelectRuntime when Phase.Runtime == "".
// Explicit authored runtimes always win over this policy.
func SelectRuntime(role Role, persona, task string) Runtime {
	switch role {
	case RoleReviewer:
		// Review phases need Claude's extended analytical reasoning and rich
		// judgement. Codex is a code-generation runtime, not a review tool.
		return RuntimeClaude
	case RolePlanner:
		// Planning phases benefit from Claude's multi-turn conversational
		// reasoning and broad context synthesis across many documents.
		return RuntimeClaude
	case RoleImplementer:
		return selectForImplementer(persona, task)
	default:
		return RuntimeClaude
	}
}

// nonCodeTaskKeywords are task-text substrings that indicate the work is not
// primarily code production. When present, the task stays on Claude regardless
// of persona, because Codex provides no advantage for research, documentation,
// writing, or deployment coordination.
var nonCodeTaskKeywords = []string{
	// research / investigation
	"research", "investigate", "analyze", "analyse", "explore", "study",
	// documentation / specification
	"document", "documentation", "readme", "changelog", "design doc",
	// content / writing
	"blog post", "newsletter", "article", "write blog", "draft post",
	"content creation", "narration", "linkedin post", "reddit post",
	// deployment / operations
	"deploy", "release", "rollout",
}

// codeTaskKeywords are positive code-shape signals. We require at least one of
// these before auto-selecting Codex so language-specialist personas do not
// silently drift into Codex for ambiguous coordination or analysis tasks.
var codeTaskKeywords = []string{
	"implement", "build", "create", "fix", "refactor", "optimize",
	"write code", "add", "update", "parser", "handler", "endpoint",
	"api", "component", "service", "function", "integration", "test",
	"bug", "cache", "cli", "schema", "migration", "migrate",
}

// languageSpecialistTokens are persona-name substrings that unambiguously
// identify a language-specialist implementer. Generic roles ("backend",
// "fullstack") are intentionally absent to keep the heuristic narrow.
var languageSpecialistTokens = []string{
	"golang", "go", "python", "typescript", "javascript", "rust",
	"java", "ruby", "swift", "kotlin", "cpp", "csharp",
}

// selectForImplementer applies the implementer sub-heuristic.
//
// NOTE: Codex auto-selection is disabled while Codex API limits are being hit.
// When re-enabling, restore the persona + task-shape gating below.
// The heuristic data (nonCodeTaskKeywords, codeTaskKeywords,
// languageSpecialistTokens) is retained so re-enabling is a one-line change.
func selectForImplementer(persona, task string) Runtime {
	// Codex auto-selection disabled — always use Claude.
	return RuntimeClaude
}

// containsHeuristicPhrase matches keywords and short phrases on normalized
// word boundaries. This avoids substring false-positives like "cli" matching
// "client" while still supporting multi-word phrases such as "blog post".
func containsHeuristicPhrase(text, phrase string) bool {
	normalizedText := normalizeHeuristicText(text)
	normalizedPhrase := normalizeHeuristicText(phrase)
	if normalizedText == "" || normalizedPhrase == "" {
		return false
	}
	return strings.Contains(" "+normalizedText+" ", " "+normalizedPhrase+" ")
}

func normalizeHeuristicText(s string) string {
	var b strings.Builder
	b.Grow(len(s))
	lastSpace := true
	for _, r := range strings.ToLower(s) {
		if unicode.IsLetter(r) || unicode.IsDigit(r) {
			b.WriteRune(r)
			lastSpace = false
			continue
		}
		if !lastSpace {
			b.WriteByte(' ')
			lastSpace = true
		}
	}
	return strings.TrimSpace(b.String())
}

// ---------------------------------------------------------------------------
// Runtime capabilities & ownership contracts
// ---------------------------------------------------------------------------

// RuntimeCap identifies a discrete capability that a runtime may provide.
// Capabilities are declared by RuntimeDescriptor and required by PhaseContract.
type RuntimeCap string

const (
	// CapToolUse indicates the runtime can invoke tools (file ops, shell, etc.).
	CapToolUse RuntimeCap = "cap.tool_use"

	// CapSessionResume indicates the runtime can resume from a prior session ID.
	CapSessionResume RuntimeCap = "session_resume"

	// CapStreaming indicates the runtime emits streaming progress events.
	CapStreaming RuntimeCap = "streaming"

	// CapCostReport indicates the runtime reports token counts and cost.
	CapCostReport RuntimeCap = "cost_report"

	// CapArtifacts indicates the runtime produces file artifacts that the
	// orchestrator can merge into the workspace.
	CapArtifacts RuntimeCap = "artifacts"
)

// RuntimeCaps is a set of capabilities. It is intentionally a map rather than
// a slice so lookup is O(1) and duplicates are impossible.
type RuntimeCaps map[RuntimeCap]bool

// Has reports whether the set contains cap.
func (c RuntimeCaps) Has(cap RuntimeCap) bool { return c[cap] }

// Slice returns the capabilities as a sorted slice (for deterministic output).
func (c RuntimeCaps) Slice() []RuntimeCap {
	out := make([]RuntimeCap, 0, len(c))
	for cap := range c {
		out = append(out, cap)
	}
	slices.Sort(out)
	return out
}

// RuntimeDescriptor declares the identity and capabilities of a runtime
// executor. Executors that implement the optional RuntimeDescriber interface
// return one of these so the engine can validate phase contracts before dispatch.
type RuntimeDescriptor struct {
	// Name is the runtime identifier (e.g. "claude", "codex").
	Name Runtime
	// Caps is the set of capabilities this runtime provides.
	Caps RuntimeCaps
}

// ClaudeDescriptor returns the canonical descriptor for RuntimeClaude.
func ClaudeDescriptor() RuntimeDescriptor {
	return RuntimeDescriptor{
		Name: RuntimeClaude,
		Caps: RuntimeCaps{
			CapToolUse:       true,
			CapSessionResume: true,
			CapStreaming:     true,
			CapCostReport:    true,
			CapArtifacts:     true,
		},
	}
}

// CodexDescriptor returns the canonical descriptor for RuntimeCodex.
// Codex supports tool use, session resume (via thread IDs), and streaming
// output. It does not report per-call cost breakdowns.
// CapCostReport is intentionally absent: RuntimeCaps is a set, so only
// capabilities the runtime actually provides should appear as map keys.
func CodexDescriptor() RuntimeDescriptor {
	return RuntimeDescriptor{
		Name: RuntimeCodex,
		Caps: RuntimeCaps{
			CapToolUse:       true,
			CapSessionResume: true,
			CapStreaming:     true,
			CapArtifacts:     true,
		},
	}
}

// ---------------------------------------------------------------------------
// Phase contracts
// ---------------------------------------------------------------------------

// PhaseContract declares what a phase requires from its runtime executor.
// Contracts are derived from the phase's Role and validated before dispatch.
type PhaseContract struct {
	// Role is the orchestrator role this contract was derived from.
	Role Role
	// Required lists capabilities that MUST be present. A missing required
	// capability causes the phase to fail before dispatch.
	Required []RuntimeCap
	// Preferred lists capabilities that SHOULD be present. Missing preferred
	// capabilities emit a warning but do not block execution.
	Preferred []RuntimeCap
}

// ContractForRole returns the default PhaseContract for a given orchestrator
// role. Unknown roles return a minimal contract requiring only CapToolUse.
func ContractForRole(role Role) PhaseContract {
	switch role {
	case RolePlanner:
		return PhaseContract{
			Role:      RolePlanner,
			Required:  []RuntimeCap{CapToolUse},
			Preferred: []RuntimeCap{CapArtifacts, CapCostReport},
		}
	case RoleImplementer:
		return PhaseContract{
			Role:      RoleImplementer,
			Required:  []RuntimeCap{CapToolUse, CapArtifacts},
			Preferred: []RuntimeCap{CapSessionResume, CapCostReport},
		}
	case RoleReviewer:
		return PhaseContract{
			Role:      RoleReviewer,
			Required:  []RuntimeCap{CapToolUse, CapArtifacts},
			Preferred: []RuntimeCap{CapCostReport},
		}
	default:
		return PhaseContract{
			Role:     role,
			Required: []RuntimeCap{CapToolUse},
		}
	}
}

// ---------------------------------------------------------------------------
// Contract validation
// ---------------------------------------------------------------------------

// ContractResult captures the outcome of validating a PhaseContract against a
// RuntimeDescriptor. Used by the engine to decide whether to proceed, warn, or
// fail before executor dispatch.
type ContractResult struct {
	// Satisfied is true when all required capabilities are present.
	Satisfied bool
	// Missing lists required capabilities not provided by the runtime.
	Missing []RuntimeCap
	// Warnings lists preferred capabilities not provided by the runtime.
	Warnings []string
}

// Validate checks whether desc satisfies the contract. Returns a ContractResult
// indicating whether all required capabilities are met and whether any preferred
// capabilities are missing.
func (c PhaseContract) Validate(desc RuntimeDescriptor) ContractResult {
	var result ContractResult
	result.Satisfied = true

	for _, req := range c.Required {
		if !desc.Caps.Has(req) {
			result.Satisfied = false
			result.Missing = append(result.Missing, req)
		}
	}

	for _, pref := range c.Preferred {
		if !desc.Caps.Has(pref) {
			result.Warnings = append(result.Warnings,
				fmt.Sprintf("runtime %q lacks preferred capability %q for %s phase",
					desc.Name, pref, c.Role))
		}
	}

	return result
}

// ErrorMessage returns a human-readable message when Satisfied is false.
// Returns "" when the contract is satisfied.
func (r ContractResult) ErrorMessage() string {
	if r.Satisfied {
		return ""
	}
	caps := make([]string, len(r.Missing))
	for i, c := range r.Missing {
		caps[i] = string(c)
	}
	return fmt.Sprintf("runtime missing required capabilities: %s", strings.Join(caps, ", "))
}
