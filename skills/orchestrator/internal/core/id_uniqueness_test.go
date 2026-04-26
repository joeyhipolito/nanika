package core_test

// Build-time validation: every typed string constant that acts as an identifier
// must have a globally unique value. This test collects constants from all
// packages that define ID-like enums and fails if any two constants in different
// semantic domains share the same string value.
//
// Intentional overlaps (SDK wire-format constants that must match the Claude API)
// are tracked in an explicit allowlist so we know exactly which duplicates are
// accepted and why.

import (
	"fmt"
	"sort"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/nanika/shared/sdk"
)

// idEntry pairs a constant's string value with its source (package + name).
type idEntry struct {
	value  string
	source string // e.g. "core.DecompLLM"
}

// sdkWireFormatAllowlist enumerates string values that are intentionally
// duplicated across SDK constant groups because they must match the Claude
// API wire format. Each entry maps a value to the set of constant names
// that legitimately share it.
//
// V-23 scope: read-model unification with two on-disk authoring trees retained.
// See shared/sdk/DESIGN.md for the full rationale.
var sdkWireFormatAllowlist = map[string][]string{
	"text":     {"sdk.BlockTypeText", "sdk.KindText"},
	"tool_use": {"sdk.BlockTypeToolUse", "sdk.KindToolUse"},
}

func TestIDConstantUniqueness(t *testing.T) {
	// Collect all typed string constants that serve as identifiers.
	// Each entry records the constant's string value and its qualified name.
	all := []idEntry{
		// core: decomposition source
		{core.DecompPredecomposed, "core.DecompPredecomposed"},
		{core.DecompLLM, "core.DecompLLM"},
		{core.DecompKeyword, "core.DecompKeyword"},
		{core.DecompTemplate, "core.DecompTemplate"},

		// core: phase status
		{string(core.StatusPending), "core.StatusPending"},
		{string(core.StatusRunning), "core.StatusRunning"},
		{string(core.StatusCompleted), "core.StatusCompleted"},
		{string(core.StatusFailed), "core.StatusFailed"},
		{string(core.StatusSkipped), "core.StatusSkipped"},

		// core: runtime
		{string(core.RuntimeClaude), "core.RuntimeClaude"},
		{string(core.RuntimeCodex), "core.RuntimeCodex"},

		// core: runtime capabilities
		{string(core.CapToolUse), "core.CapToolUse"},
		{string(core.CapSessionResume), "core.CapSessionResume"},
		{string(core.CapStreaming), "core.CapStreaming"},
		{string(core.CapCostReport), "core.CapCostReport"},
		{string(core.CapArtifacts), "core.CapArtifacts"},

		// core: roles
		{string(core.RolePlanner), "core.RolePlanner"},
		{string(core.RoleImplementer), "core.RoleImplementer"},
		{string(core.RoleReviewer), "core.RoleReviewer"},

		// event: event types (25 constants)
		{string(event.MissionStarted), "event.MissionStarted"},
		{string(event.MissionCompleted), "event.MissionCompleted"},
		{string(event.MissionFailed), "event.MissionFailed"},
		{string(event.MissionCancelled), "event.MissionCancelled"},
		{string(event.PhaseStarted), "event.PhaseStarted"},
		{string(event.PhaseCompleted), "event.PhaseCompleted"},
		{string(event.PhaseFailed), "event.PhaseFailed"},
		{string(event.PhaseSkipped), "event.PhaseSkipped"},
		{string(event.PhaseRetrying), "event.PhaseRetrying"},
		{string(event.WorkerSpawned), "event.WorkerSpawned"},
		{string(event.WorkerOutput), "event.WorkerOutput"},
		{string(event.WorkerCompleted), "event.WorkerCompleted"},
		{string(event.WorkerFailed), "event.WorkerFailed"},
		{string(event.DecomposeStarted), "event.DecomposeStarted"},
		{string(event.DecomposeCompleted), "event.DecomposeCompleted"},
		{string(event.DecomposeFallback), "event.DecomposeFallback"},
		{string(event.LearningExtracted), "event.LearningExtracted"},
		{string(event.LearningStored), "event.LearningStored"},
		{string(event.DAGDependencyResolved), "event.DAGDependencyResolved"},
		{string(event.DAGPhaseDispatched), "event.DAGPhaseDispatched"},
		{string(event.RoleHandoff), "event.RoleHandoff"},
		{string(event.ContractValidated), "event.ContractValidated"},
		{string(event.ContractViolated), "event.ContractViolated"},
		{string(event.PersonaContractViolation), "event.PersonaContractViolation"},
		{string(event.SystemError), "event.SystemError"},
		{string(event.SystemCheckpointSaved), "event.SystemCheckpointSaved"},
		{string(event.ZettelWritten), "event.ZettelWritten"},
		{string(event.ZettelSkipped), "event.ZettelSkipped"},
		{string(event.ZettelWriteFailed), "event.ZettelWriteFailed"},

		// sdk: content block types (wire-format — allowlisted overlaps)
		{sdk.BlockTypeText, "sdk.BlockTypeText"},
		{sdk.BlockTypeToolUse, "sdk.BlockTypeToolUse"},
		{sdk.BlockTypeToolResult, "sdk.BlockTypeToolResult"},

		// sdk: streamed event kinds (wire-format — allowlisted overlaps)
		{string(sdk.KindText), "sdk.KindText"},
		{string(sdk.KindToolUse), "sdk.KindToolUse"},
		{string(sdk.KindTurnEnd), "sdk.KindTurnEnd"},
	}

	// Build map: value → list of sources.
	seen := make(map[string][]string)
	for _, e := range all {
		seen[e.value] = append(seen[e.value], e.source)
	}

	// Check for duplicates, excluding allowlisted wire-format overlaps.
	for value, sources := range seen {
		if len(sources) <= 1 {
			continue
		}

		// Check allowlist.
		if allowed, ok := sdkWireFormatAllowlist[value]; ok {
			sort.Strings(allowed)
			sorted := make([]string, len(sources))
			copy(sorted, sources)
			sort.Strings(sorted)
			if fmt.Sprint(sorted) == fmt.Sprint(allowed) {
				continue // exact match with allowlist — accepted
			}
		}

		t.Errorf("duplicate constant value %q used by: %v — rename one to be unique or add to sdkWireFormatAllowlist if intentional",
			value, sources)
	}
}
