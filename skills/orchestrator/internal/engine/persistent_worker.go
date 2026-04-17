package engine

import (
	"fmt"
	"math/rand"
	"sort"
	"strings"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
	"github.com/joeyhipolito/orchestrator-cli/internal/worker"
)

// persistentWorkerName is the name of the single persistent worker used across missions.
const persistentWorkerName = "alpha"

// perRunWorkerCap is the maximum number of phases the persistent worker handles per run.
// Acts as a per-run safety valve to prevent a single mission from exhausting
// the worker's context budget.
const perRunWorkerCap = 5

// persistentWorkerRoll is the probability that an eligible phase is assigned to
// the persistent worker (~30%).
const persistentWorkerRoll = 0.30

// excludedPhaseTypes are phase name substrings that make a phase ineligible for
// the persistent worker. These phases are meta-level (reviewing, decomposing,
// or cleaning up) and don't benefit from accumulated domain memory.
var excludedPhaseTypes = []string{"review", "cleanup", "decompose"}

// shouldAssignPersistentWorker returns true if phase should be executed by the
// persistent alpha worker. Checks in order:
//
//  1. noPersistentWorker flag (global opt-out)
//  2. Phase name exclusion (review/cleanup/decompose phases are skipped)
//  3. Per-run cap: sameDayCount >= perRunWorkerCap disables further assignment
//  4. Random roll: ~30% probability
//
// Extracted as a package-level function so engine_test.go can test the logic
// without spawning workers, following the same pattern as shouldInjectLearnings.
func shouldAssignPersistentWorker(phase *core.Phase, noPersistentWorker bool, sameDayCount int) bool {
	if noPersistentWorker {
		return false
	}

	// Phase type exclusion: meta-phases don't benefit from accumulated memory.
	lower := strings.ToLower(phase.Name)
	for _, excluded := range excludedPhaseTypes {
		if strings.Contains(lower, excluded) {
			return false
		}
	}

	// Per-run cap: limit persistent worker load per execution run.
	if sameDayCount >= perRunWorkerCap {
		return false
	}

	// Random roll: stochastic assignment so not every eligible phase uses the
	// persistent worker — this keeps the worker's memory focused on high-signal
	// phases rather than growing unboundedly.
	return rand.Float64() < persistentWorkerRoll //nolint:gosec
}

// selfReflectionInterval is how often (in phases completed) the worker records
// a self-reflection entry. Extracted as a constant so tests can reference it.
const selfReflectionInterval = 10

// memoryTypeReflection is the MemoryEntry.Type value for self-reflection entries.
const memoryTypeReflection = "reflection"

// writeLearningsToWorkerMemory receives already-captured learnings (from the
// global extraction goroutine) and appends each as a MemoryEntry to the worker
// identity's local memory. This is the worker-local write — no extraction
// happens here; the global goroutine handles both marker-based and persona-aware
// LLM extraction so there is exactly one extraction, two destinations (global DB
// + worker memory).
//
// Called synchronously (under e.mu) so the subsequent SaveIdentity persists all
// new entries.
func writeLearningsToWorkerMemory(wi *worker.WorkerIdentity, captured []learning.Learning) {
	now := time.Now()
	for _, l := range captured {
		wi.AddMemoryEntry(worker.MemoryEntry{
			Content: l.Content,
			Filed:   now,
			By:      wi.Name,
			Type:    string(l.Type),
		})
	}
}

// extractLearningsToWorkerMemory is the legacy entry point that extracts
// marker-based learnings from raw output and writes them to worker memory.
// Retained for backward compatibility with tests; new engine code uses the
// split pipeline (global extraction goroutine + writeLearningsToWorkerMemory).
func extractLearningsToWorkerMemory(wi *worker.WorkerIdentity, output, domain string) {
	captured := learning.CaptureFromText(output, wi.Name, domain, "")
	writeLearningsToWorkerMemory(wi, captured)
}

// maybeRecordSelfReflection appends a synthetic reflection MemoryEntry when
// wi.PhasesCompleted is a positive multiple of selfReflectionInterval.
// The entry summarises current stats so downstream phases get a high-level
// snapshot of the worker's accumulated experience.
// No-op when PhasesCompleted is 0 (just bootstrapped) or not at an interval boundary.
func maybeRecordSelfReflection(wi *worker.WorkerIdentity) {
	if wi.PhasesCompleted == 0 || wi.PhasesCompleted%selfReflectionInterval != 0 {
		return
	}
	content := fmt.Sprintf("[reflection] %d phases completed; domains: %s; total_cost: %.4f; memory_entries: %d",
		wi.PhasesCompleted, formatDomains(wi.Domains), wi.TotalCost, len(wi.Entries))
	wi.AddMemoryEntry(worker.MemoryEntry{
		Content: content,
		Filed:   time.Now(),
		By:      wi.Name,
		Type:    memoryTypeReflection,
	})
}

// formatDomains produces a compact sorted "key:count,..." representation.
func formatDomains(domains map[string]int) string {
	if len(domains) == 0 {
		return "(none)"
	}
	keys := make([]string, 0, len(domains))
	for k := range domains {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	parts := make([]string, 0, len(keys))
	for _, k := range keys {
		parts = append(parts, fmt.Sprintf("%s:%d", k, domains[k]))
	}
	return strings.Join(parts, ",")
}
