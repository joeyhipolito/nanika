package router

import (
	"strings"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
)

// ModelTier represents the complexity tier for model selection.
type ModelTier string

const (
	TierThink ModelTier = "think" // Complex reasoning, architecture, planning, security
	TierWork  ModelTier = "work"  // Implementation, research, writing
	TierQuick ModelTier = "quick" // Simple edits, formatting, small fixes
)

// ClaudeModelID maps tiers to Claude model identifiers.
var ClaudeModelID = map[ModelTier]string{
	TierThink: "opus",
	TierWork:  "sonnet",
	TierQuick: "haiku",
}

// CodexModelID maps tiers to OpenAI Codex model identifiers.
var CodexModelID = map[ModelTier]string{
	TierThink: "gpt-5.4",
	TierWork:  "gpt-5.4",
	TierQuick: "gpt-5.4",
}

// ModelID is the default (Claude) model map. Kept for backward compatibility.
var ModelID = ClaudeModelID

// Resolve returns the model ID for a tier (Claude runtime).
func Resolve(tier ModelTier) string {
	if id, ok := ClaudeModelID[tier]; ok {
		return id
	}
	return ClaudeModelID[TierWork] // default to sonnet
}

// ResolveForRuntime returns the model ID appropriate for the given runtime.
func ResolveForRuntime(tier ModelTier, runtime core.Runtime) string {
	switch runtime.Effective() {
	case core.RuntimeCodex:
		if id, ok := CodexModelID[tier]; ok {
			return id
		}
		return CodexModelID[TierWork]
	default:
		return Resolve(tier)
	}
}

// ResolveEffort returns the Claude Code effort level for the given tier/persona.
// Reviewer/security/architecture work should bias toward higher reasoning effort,
// while quick edits stay low-cost.
func ResolveEffort(tier ModelTier, persona string) string {
	switch persona {
	case "architect", "security-auditor", "staff-code-reviewer":
		return "high"
	}

	switch tier {
	case TierThink:
		return "high"
	case TierQuick:
		return "low"
	default:
		return "medium"
	}
}

// ResolveEffortForRuntime returns the effort level appropriate for the given runtime.
// Codex uses its own effort scale: xhigh for think, high for work, medium for quick.
func ResolveEffortForRuntime(tier ModelTier, persona string, runtime core.Runtime) string {
	switch runtime.Effective() {
	case core.RuntimeCodex:
		switch {
		case persona == "architect" || persona == "security-auditor" || persona == "staff-code-reviewer":
			return "xhigh"
		case tier == TierThink:
			return "xhigh"
		case tier == TierQuick:
			return "medium"
		default:
			return "high"
		}
	default:
		return ResolveEffort(tier, persona)
	}
}

// ClassifyTier determines the appropriate model tier based on task and persona.
func ClassifyTier(task string, persona string) ModelTier {
	lower := strings.ToLower(task)

	// Think tier: architecture, planning, security, complex analysis
	thinkSignals := []string{
		"architect", "design", "plan", "security", "audit",
		"threat model", "vulnerability", "review architecture",
		"system design", "data model", "api design",
	}
	for _, signal := range thinkSignals {
		if strings.Contains(lower, signal) {
			return TierThink
		}
	}

	// Persona-based escalation
	switch persona {
	case "architect", "security-auditor", "staff-code-reviewer":
		return TierThink
	case "reviewer":
		// Reviews that mention security get Think tier
		if strings.Contains(lower, "security") || strings.Contains(lower, "vulnerab") {
			return TierThink
		}
		return TierWork
	}

	// Quick tier: simple edits, formatting
	quickSignals := []string{
		"format", "rename", "fix typo", "update comment",
		"add docstring", "simple fix", "minor change",
	}
	for _, signal := range quickSignals {
		if strings.Contains(lower, signal) {
			return TierQuick
		}
	}

	// Default: Work tier
	return TierWork
}

// ClassifyComplexity determines if a task needs multi-agent orchestration.
func ClassifyComplexity(task string) bool {
	lower := strings.ToLower(task)
	wordCount := len(strings.Fields(task))

	// Long tasks are likely complex
	if wordCount > 20 {
		return true
	}

	// Explicit complexity indicators
	complexSignals := []string{
		"and then", "then ", "after that", "first ", "finally ",
		"research and", "write and", "implement and", "build and",
		"multiple", "several", "phases", "steps",
		"plan and execute", "design and implement",
	}
	for _, signal := range complexSignals {
		if strings.Contains(lower, signal) {
			return true
		}
	}

	return false
}
