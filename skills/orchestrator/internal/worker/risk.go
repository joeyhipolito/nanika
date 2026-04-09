package worker

// RiskLevel classifies the potential impact of a Claude Code tool call.
// It drives the allow-list in settings.local.json: LOW-risk tools are
// explicitly permitted so workers can use them without incurring approval
// prompts. MEDIUM and HIGH tools are controlled via deny rules.
type RiskLevel string

const (
	// RiskLow marks read-only tools with no filesystem or network side effects.
	// These are explicitly allowed in settings.local.json for all worker roles.
	RiskLow RiskLevel = "LOW"

	// RiskMedium marks tools that modify local state in a recoverable way
	// (file edits, writes, shell commands). They are neither explicitly allowed
	// nor blocked — Claude Code's default approval flow applies.
	RiskMedium RiskLevel = "MEDIUM"

	// RiskHigh marks tools with irreversible or external-system effects
	// (spawning autonomous sub-agents, etc.). These are blocked by deny rules
	// or require explicit operator approval.
	RiskHigh RiskLevel = "HIGH"
)

// lowRiskToolSet is the canonical set of Claude Code tool names that are
// read-only with no side effects. Kept as a map for O(1) lookup.
var lowRiskToolSet = map[string]struct{}{
	"Glob":       {},
	"Grep":       {},
	"Read":       {},
	"TaskOutput": {},
	"WebFetch":   {},
	"WebSearch":  {},
}

// ClassifyToolRisk returns the RiskLevel for the given Claude Code tool name.
//
// LOW  — read-only tools (Read, Glob, Grep, WebSearch, WebFetch, TaskOutput)
// HIGH — Agent (autonomous sub-agent spawning with uncontrolled access)
// MEDIUM — all other tools (Bash, Edit, Write, TodoWrite, NotebookEdit, unknown)
//
// Unknown tool names default to RiskMedium: assume state-modifying until proven otherwise.
func ClassifyToolRisk(toolName string) RiskLevel {
	if _, ok := lowRiskToolSet[toolName]; ok {
		return RiskLow
	}
	if toolName == "Agent" {
		return RiskHigh
	}
	return RiskMedium
}

// LowRiskTools returns the sorted list of tool names classified as RiskLow.
// These names are written to permissions.allow in settings.local.json so that
// workers can invoke read-only tools without manual approval.
func LowRiskTools() []string {
	return []string{
		"Glob",
		"Grep",
		"Read",
		"TaskOutput",
		"WebFetch",
		"WebSearch",
	}
}
