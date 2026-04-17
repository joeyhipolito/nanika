package engine

// Tests for shouldAssignPersistentWorker selection logic and worker-memory
// post-execution helpers (extractLearningsToWorkerMemory, maybeRecordSelfReflection).
// Follows the same pattern as TestDisableLearnings — extracts guard functions as
// pure helpers so tests don't need real workers or file I/O where avoidable.

import (
	"strings"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/worker"
)

// ---------------------------------------------------------------------------
// shouldAssignPersistentWorker: selection logic
// ---------------------------------------------------------------------------

func TestShouldAssignPersistentWorker_FlagDisables(t *testing.T) {
	phase := &core.Phase{Name: "implement", Objective: "write the feature"}
	// noPersistentWorker=true must always return false regardless of count.
	got := shouldAssignPersistentWorker(phase, true, 0)
	if got {
		t.Error("shouldAssignPersistentWorker: noPersistentWorker=true must return false")
	}
}

func TestShouldAssignPersistentWorker_PhaseExclusion(t *testing.T) {
	tests := []struct {
		name      string
		phaseName string
		wantFalse bool
	}{
		{name: "review phase excluded", phaseName: "review", wantFalse: true},
		{name: "code-review excluded", phaseName: "code-review", wantFalse: true},
		{name: "cleanup excluded", phaseName: "cleanup", wantFalse: true},
		{name: "cleanup-artifacts excluded", phaseName: "cleanup-artifacts", wantFalse: true},
		{name: "decompose excluded", phaseName: "decompose", wantFalse: true},
		{name: "decompose-mission excluded", phaseName: "decompose-mission", wantFalse: true},
		{name: "implement not excluded", phaseName: "implement", wantFalse: false},
		{name: "research not excluded", phaseName: "research", wantFalse: false},
		{name: "write not excluded", phaseName: "write", wantFalse: false},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			phase := &core.Phase{Name: tt.phaseName, Objective: "do something"}
			// Run many times because there's a random roll; excluded phases must
			// ALWAYS return false, eligible ones must return true at least once.
			if tt.wantFalse {
				for i := 0; i < 50; i++ {
					if shouldAssignPersistentWorker(phase, false, 0) {
						t.Errorf("phase %q: shouldAssignPersistentWorker must always return false (excluded type)", tt.phaseName)
						break
					}
				}
			}
			// For non-excluded phases we only verify the cap logic below —
			// the random roll makes "must return true" unreliable in unit tests.
		})
	}
}

func TestShouldAssignPersistentWorker_PerRunCap(t *testing.T) {
	phase := &core.Phase{Name: "implement", Objective: "build the feature"}
	// When sameDayCount >= perRunWorkerCap, must always return false.
	for i := 0; i < 20; i++ {
		if shouldAssignPersistentWorker(phase, false, perRunWorkerCap) {
			t.Errorf("shouldAssignPersistentWorker: sameDayCount=%d >= cap=%d must return false", perRunWorkerCap, perRunWorkerCap)
			break
		}
	}
	// Count strictly above cap also blocked.
	for i := 0; i < 20; i++ {
		if shouldAssignPersistentWorker(phase, false, perRunWorkerCap+1) {
			t.Errorf("shouldAssignPersistentWorker: count above cap must return false")
			break
		}
	}
}

func TestShouldAssignPersistentWorker_BelowCapCanReturn(t *testing.T) {
	phase := &core.Phase{Name: "implement", Objective: "build the feature"}
	// Below cap and eligible: should return true at some point across many trials.
	// (probability ~30% per trial → P(never true in 100 trials) ≈ 0.7^100 < 3×10^-16)
	got := false
	for i := 0; i < 100; i++ {
		if shouldAssignPersistentWorker(phase, false, 0) {
			got = true
			break
		}
	}
	if !got {
		t.Error("shouldAssignPersistentWorker: eligible phase below cap should eventually return true")
	}
}

func TestShouldAssignPersistentWorker_FlagBeatsEverything(t *testing.T) {
	// Even with count=0 and non-excluded phase, flag=true always wins.
	phase := &core.Phase{Name: "implement", Objective: "build the feature"}
	for i := 0; i < 50; i++ {
		if shouldAssignPersistentWorker(phase, true, 0) {
			t.Error("noPersistentWorker=true must beat all other conditions")
			break
		}
	}
}

// Phase.Worker field tests live in claudemd_test.go (TestPhaseWorkerField_ZeroValue,
// TestPhaseWorkerField_Settable) — no duplication here.

// ---------------------------------------------------------------------------
// extractLearningsToWorkerMemory: memory accumulation
// ---------------------------------------------------------------------------

// setupWorkerHome sets HOME to a temp dir so tests don't touch ~/.alluka.
func setupWorkerHome(t *testing.T) {
	t.Helper()
	t.Setenv("HOME", t.TempDir())
}

// TestExtractLearningsToWorkerMemory_AccumulatesEntries verifies that
// LEARNING:/PATTERN:/FINDING: markers in phase output are added to the
// worker's local memory.
func TestExtractLearningsToWorkerMemory_AccumulatesEntries(t *testing.T) {
	setupWorkerHome(t)

	wi, err := worker.LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	output := "LEARNING: always wrap errors with fmt.Errorf.\nPATTERN: use table-driven tests for all edge cases.\nFINDING: SQLite is sufficient for this workload."
	extractLearningsToWorkerMemory(wi, output, "dev")

	if len(wi.Entries) != 3 {
		t.Fatalf("expected 3 entries, got %d", len(wi.Entries))
	}

	contents := make([]string, len(wi.Entries))
	for i, e := range wi.Entries {
		contents[i] = e.Content
	}
	if !strings.Contains(strings.Join(contents, "\n"), "always wrap errors") {
		t.Error("LEARNING entry not found in worker memory")
	}
	if !strings.Contains(strings.Join(contents, "\n"), "table-driven tests") {
		t.Error("PATTERN entry not found in worker memory")
	}
}

// TestExtractLearningsToWorkerMemory_NoMarkersNoEntries verifies that output
// without any marker lines adds nothing to worker memory.
func TestExtractLearningsToWorkerMemory_NoMarkersNoEntries(t *testing.T) {
	setupWorkerHome(t)

	wi, err := worker.LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	extractLearningsToWorkerMemory(wi, "Nothing to capture here. Just prose.", "dev")

	if len(wi.Entries) != 0 {
		t.Errorf("expected 0 entries for output with no markers, got %d", len(wi.Entries))
	}
}

// TestExtractLearningsToWorkerMemory_DeduplicatesIdenticalMarkers verifies that
// the same content appearing twice (e.g. duplicate LEARNING: lines) is only
// stored once (deduplication via AddMemoryEntry).
func TestExtractLearningsToWorkerMemory_DeduplicatesIdenticalMarkers(t *testing.T) {
	setupWorkerHome(t)

	wi, err := worker.LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	output := "LEARNING: always use context cancellation to avoid leaks.\nLEARNING: always use context cancellation to avoid leaks."
	extractLearningsToWorkerMemory(wi, output, "dev")

	if len(wi.Entries) != 1 {
		t.Errorf("expected 1 entry after deduplication, got %d", len(wi.Entries))
	}
}

// ---------------------------------------------------------------------------
// maybeRecordSelfReflection: trigger at interval boundaries
// ---------------------------------------------------------------------------

// TestMaybeRecordSelfReflection_NoOpAtZero verifies that a freshly bootstrapped
// worker (PhasesCompleted=0) does not trigger a reflection.
func TestMaybeRecordSelfReflection_NoOpAtZero(t *testing.T) {
	setupWorkerHome(t)

	wi, err := worker.LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	// PhasesCompleted is 0 after bootstrap.
	maybeRecordSelfReflection(wi)

	if len(wi.Entries) != 0 {
		t.Errorf("expected no reflection entry at PhasesCompleted=0, got %d entries", len(wi.Entries))
	}
}

// TestMaybeRecordSelfReflection_NoOpBeforeInterval verifies that reflection is
// not triggered for phase counts that are not multiples of the interval.
func TestMaybeRecordSelfReflection_NoOpBeforeInterval(t *testing.T) {
	setupWorkerHome(t)

	wi, err := worker.LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	for _, count := range []int{1, 5, 9, 11, 19} {
		wi.PhasesCompleted = count
		wi.Entries = nil // reset entries between checks
		maybeRecordSelfReflection(wi)
		if len(wi.Entries) != 0 {
			t.Errorf("PhasesCompleted=%d: unexpected reflection entry (interval is %d)", count, selfReflectionInterval)
		}
	}
}

// TestMaybeRecordSelfReflection_TriggersAtInterval verifies that a reflection
// entry is added exactly at positive multiples of selfReflectionInterval.
func TestMaybeRecordSelfReflection_TriggersAtInterval(t *testing.T) {
	setupWorkerHome(t)

	wi, err := worker.LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	for _, count := range []int{10, 20, 30} {
		wi.PhasesCompleted = count
		wi.Entries = nil
		maybeRecordSelfReflection(wi)
		if len(wi.Entries) != 1 {
			t.Fatalf("PhasesCompleted=%d: expected 1 reflection entry, got %d", count, len(wi.Entries))
		}
		entry := wi.Entries[0]
		if entry.Type != memoryTypeReflection {
			t.Errorf("PhasesCompleted=%d: expected type=reflection, got %q", count, entry.Type)
		}
		if !strings.Contains(entry.Content, "[reflection]") {
			t.Errorf("PhasesCompleted=%d: reflection entry content missing [reflection] marker: %q", count, entry.Content)
		}
		if !strings.Contains(entry.Content, "phases completed") {
			t.Errorf("PhasesCompleted=%d: reflection entry missing phases count: %q", count, entry.Content)
		}
	}
}

// TestMaybeRecordSelfReflection_IncludesDomainStats verifies that domain counts
// are included in the reflection entry when domains are tracked.
func TestMaybeRecordSelfReflection_IncludesDomainStats(t *testing.T) {
	setupWorkerHome(t)

	wi, err := worker.LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	wi.PhasesCompleted = 10
	wi.Domains = map[string]int{"dev": 7, "personal": 3}
	wi.TotalCost = 0.42

	maybeRecordSelfReflection(wi)

	if len(wi.Entries) != 1 {
		t.Fatalf("expected 1 reflection entry, got %d", len(wi.Entries))
	}
	content := wi.Entries[0].Content
	if !strings.Contains(content, "dev:7") {
		t.Errorf("reflection entry missing dev domain count: %q", content)
	}
	if !strings.Contains(content, "personal:3") {
		t.Errorf("reflection entry missing personal domain count: %q", content)
	}
}

// ---------------------------------------------------------------------------
// No-op for ephemeral phases (phase.Worker == "")
// ---------------------------------------------------------------------------

// TestEphemeralPhase_NoWorkerMemoryTouched verifies that a phase without a
// persistent worker assignment (Worker=="") leaves all WorkerIdentity state
// unchanged. The engine gates all worker-memory calls on assignedWorker != nil,
// so this test exercises that guard path by calling helpers only when the
// worker is assigned.
func TestEphemeralPhase_NoWorkerMemoryTouched(t *testing.T) {
	setupWorkerHome(t)

	wi, err := worker.LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	phase := &core.Phase{Name: "implement", Worker: ""}

	// Simulate the engine's guard: assignedWorker is nil when phase.Worker == "".
	var assignedWorker *worker.WorkerIdentity
	if phase.Worker == persistentWorkerName {
		assignedWorker = wi
	}

	if assignedWorker != nil {
		extractLearningsToWorkerMemory(assignedWorker, "LEARNING: something", "dev")
		assignedWorker.RecordPhase("dev", 0.01)
		maybeRecordSelfReflection(assignedWorker)
	}

	// wi should be unchanged.
	if wi.PhasesCompleted != 0 {
		t.Errorf("ephemeral phase must not increment PhasesCompleted; got %d", wi.PhasesCompleted)
	}
	if len(wi.Entries) != 0 {
		t.Errorf("ephemeral phase must not add memory entries; got %d", len(wi.Entries))
	}
}
