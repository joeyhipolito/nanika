package engine

// extract_test.go verifies the four behavioral guarantees of the background
// learning extraction system introduced in engine.go:
//
//  1. Extraction runs after phase completion without blocking the next phase.
//  2. Graceful shutdown: Execute waits for all pending extractions before
//     emitting the terminal mission event.
//  3. Rate limiter: at most one extraction goroutine runs at a time per mission.
//  4. Extraction failures do not fail the mission.

import (
	"context"
	"fmt"
	"path/filepath"
	"sync"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
	"github.com/joeyhipolito/orchestrator-cli/internal/sdk"
	"github.com/joeyhipolito/orchestrator-cli/internal/worker"
)

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const extractRuntime = core.Runtime("extract-test")

// learningOutputExecutor returns fixed output containing n unique LEARNING:
// markers that satisfy isValidLearning (≥20 chars, terminal punctuation).
type learningOutputExecutor struct {
	output string
}

func (x learningOutputExecutor) Execute(_ context.Context, _ *core.WorkerConfig, _ event.Emitter, _ bool) (string, string, *sdk.CostInfo, error) {
	return x.output, "", nil, nil
}

// makeLearningOutput returns output with n unique LEARNING: lines.
// The prefix disambiguates learnings across phases; index disambiguates within.
func makeLearningOutput(prefix string, n int) string {
	out := ""
	for i := 0; i < n; i++ {
		out += fmt.Sprintf("LEARNING: %s extraction insight index %d verified correct.\n", prefix, i)
	}
	return out
}

// openTempDB opens a real SQLite learning database backed by a per-test temp dir.
func openTempDB(t *testing.T) *learning.DB {
	t.Helper()
	db, err := learning.OpenDB(filepath.Join(t.TempDir(), "learnings.db"))
	if err != nil {
		t.Fatalf("openTempDB: %v", err)
	}
	t.Cleanup(func() { db.Close() })
	return db
}

// openClosedDB returns a *learning.DB whose underlying connection is already
// closed, causing every Insert call to return an error.
func openClosedDB(t *testing.T) *learning.DB {
	t.Helper()
	db := openTempDB(t)
	if err := db.Close(); err != nil {
		t.Fatalf("openClosedDB: close: %v", err)
	}
	return db
}

// extractPhase builds a phase that uses extractRuntime.
func extractPhase(id string, deps ...string) *core.Phase {
	return &core.Phase{
		ID:           id,
		Name:         id,
		Objective:    "phase " + id,
		Persona:      "senior-backend-engineer",
		ModelTier:    "work",
		Runtime:      extractRuntime,
		Status:       core.StatusPending,
		Dependencies: deps,
	}
}

// newExtractEngine creates an Engine with the given learning DB and executor.
// Sandboxes HOME to a temp dir so the 30% persistent-worker roll can't touch
// the real ~/.alluka/workers/alpha/ during tests.
func newExtractEngine(t *testing.T, db *learning.DB, exec PhaseExecutor) (*Engine, *captureEmitter) {
	t.Helper()
	t.Setenv("HOME", t.TempDir())
	ws := &core.Workspace{ID: "xws-" + t.Name(), Path: t.TempDir(), Domain: "test"}
	em := &captureEmitter{}
	e := New(ws, &core.OrchestratorConfig{}, nil, db).WithEmitter(em)
	e.RegisterExecutor(extractRuntime, exec)
	return e, em
}

// firstIndexOf returns the index of the first event matching typ, or -1.
func firstIndexOf(evts []event.Event, typ event.EventType) int {
	for i, ev := range evts {
		if ev.Type == typ {
			return i
		}
	}
	return -1
}

// countEvents counts events of the given type.
func countEvents(evts []event.Event, typ event.EventType) int {
	n := 0
	for _, ev := range evts {
		if ev.Type == typ {
			n++
		}
	}
	return n
}

// ---------------------------------------------------------------------------
// 1. Extraction does not block the next phase
// ---------------------------------------------------------------------------

// TestExtraction_NonBlocking verifies that sequential phases complete
// successfully when learning output is present: the extraction goroutine runs
// asynchronously, and learning.stored arrives before mission.completed.
func TestExtraction_NonBlocking(t *testing.T) {
	tests := []struct {
		name        string
		phaseOutput string
		wantStored  bool
	}{
		{
			name:        "learning marker in output — extraction fires asynchronously",
			phaseOutput: makeLearningOutput("nonblocking", 1),
			wantStored:  true,
		},
		{
			name:        "no markers in output — no extraction goroutine spawned",
			phaseOutput: "plain output with no markers.",
			wantStored:  false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			db := openTempDB(t)
			// Both phases use the same executor. Phase B's output has no markers
			// when tt.wantStored is false (tt.phaseOutput has none).
			e, em := newExtractEngine(t, db, learningOutputExecutor{output: tt.phaseOutput})

			plan := &core.Plan{
				ID:            "plan-nonblocking",
				Task:          "non-blocking extraction test",
				ExecutionMode: "sequential",
				Phases: []*core.Phase{
					extractPhase("a"),
					extractPhase("b", "a"),
				},
			}

			result, err := e.Execute(context.Background(), plan)
			if err != nil {
				t.Fatalf("Execute returned error: %v", err)
			}
			if result == nil || !result.Success {
				t.Errorf("result.Success = false; extraction must not block or fail phases")
			}

			evts := em.collected()

			// Both phases must reach phase.completed.
			for _, id := range []string{"a", "b"} {
				var saw bool
				for _, ev := range evts {
					if ev.Type == event.PhaseCompleted && ev.PhaseID == id {
						saw = true
						break
					}
				}
				if !saw {
					t.Errorf("phase.completed not emitted for phase %q", id)
				}
			}

			storedIdx := firstIndexOf(evts, event.LearningStored)
			completedIdx := firstIndexOf(evts, event.MissionCompleted)

			if tt.wantStored {
				if storedIdx < 0 {
					t.Error("expected at least one learning.stored event; got none")
				} else if completedIdx >= 0 && storedIdx > completedIdx {
					// extractWG.Wait() must drain goroutines before terminal event.
					t.Errorf("learning.stored (index %d) appeared after mission.completed (index %d); "+
						"extractWG.Wait() did not enforce ordering", storedIdx, completedIdx)
				}
			} else {
				if storedIdx >= 0 {
					t.Errorf("unexpected learning.stored event when no markers present")
				}
			}

			if completedIdx < 0 {
				t.Error("mission.completed event not emitted")
			}
		})
	}
}

// ---------------------------------------------------------------------------
// 2. Graceful shutdown waits for pending extractions
// ---------------------------------------------------------------------------

// TestExtraction_GracefulShutdown verifies that Execute does not return until
// all background extraction goroutines finish: learning.stored must arrive
// before mission.completed even when extraction is artificially delayed.
func TestExtraction_GracefulShutdown(t *testing.T) {
	db := openTempDB(t)
	e, em := newExtractEngine(t, db, learningOutputExecutor{output: makeLearningOutput("shutdown", 1)})

	// Pre-lock extractMu so every extraction goroutine blocks immediately after
	// spawning. This simulates a slow extraction (e.g. DB contention).
	e.extractMu.Lock()

	plan := &core.Plan{
		ID:            "plan-graceful-shutdown",
		Task:          "graceful shutdown test",
		ExecutionMode: "sequential",
		Phases:        []*core.Phase{extractPhase("s1")},
	}

	execDone := make(chan struct{})
	go func() {
		defer close(execDone)
		e.Execute(context.Background(), plan) //nolint:errcheck
	}()

	// Poll until phase.completed fires — at that point the extraction goroutine
	// has been spawned and is blocked on the mutex we hold.
	deadline := time.After(5 * time.Second)
	phaseCompleted := false
	for !phaseCompleted {
		select {
		case <-deadline:
			e.extractMu.Unlock()
			t.Fatal("timed out waiting for phase to complete")
		case <-time.After(5 * time.Millisecond):
			for _, ev := range em.collected() {
				if ev.Type == event.PhaseCompleted && ev.PhaseID == "s1" {
					phaseCompleted = true
					break
				}
			}
		}
	}

	// Execute must still be running: it is blocked on extractWG.Wait() because
	// the extraction goroutine is waiting for the mutex we hold.
	select {
	case <-execDone:
		e.extractMu.Unlock()
		t.Fatal("Execute returned before extractMu was released; extractWG.Wait() did not block")
	default:
		// Correct: Execute is still running.
	}

	// Release the mutex. The extraction goroutine proceeds, stores the learning,
	// emits learning.stored, decrements extractWG, unblocking Execute.
	e.extractMu.Unlock()

	select {
	case <-execDone:
	case <-time.After(5 * time.Second):
		t.Fatal("Execute did not return after extractMu was released")
	}

	evts := em.collected()

	storedIdx := firstIndexOf(evts, event.LearningStored)
	if storedIdx < 0 {
		t.Fatal("expected learning.stored event after extractMu released; got none")
	}

	completedIdx := firstIndexOf(evts, event.MissionCompleted)
	if completedIdx < 0 {
		t.Fatal("mission.completed event not emitted")
	}
	if storedIdx > completedIdx {
		t.Errorf("learning.stored (index %d) appeared after mission.completed (index %d); "+
			"extractWG.Wait() ordering guarantee violated", storedIdx, completedIdx)
	}
}

// ---------------------------------------------------------------------------
// 3. Rate limiter enforces max-1 concurrency
// ---------------------------------------------------------------------------

// TestExtraction_RateLimiter verifies that extractMu serialises goroutines:
// at most one extraction runs at a time. We spawn several parallel phases,
// pre-lock the mutex to hold all goroutines at the gate, then verify:
//   - All goroutines are queued (Execute is still blocked on extractWG.Wait()).
//   - TryLock fails while we hold the mutex.
//   - After releasing, all learnings are stored (no data loss from serialisation).
func TestExtraction_RateLimiter(t *testing.T) {
	const numPhases = 4

	db := openTempDB(t)

	// Each phase produces a unique learning prefix so all N are distinct entries.
	phases := make([]*core.Phase, numPhases)
	for i := 0; i < numPhases; i++ {
		phases[i] = extractPhase(fmt.Sprintf("p%d", i)) // no deps → dispatched in parallel
	}

	// Use a counter-based executor so each invocation returns unique content.
	exec := &countingOutputExecutor{prefix: "ratelimit"}
	e, em := newExtractEngine(t, db, exec)

	// Pre-lock extractMu: all N goroutines will spawn then immediately block.
	e.extractMu.Lock()

	plan := &core.Plan{
		ID:            "plan-ratelimit",
		Task:          "rate limiter test",
		ExecutionMode: "parallel",
		Phases:        phases,
	}

	execDone := make(chan struct{})
	go func() {
		defer close(execDone)
		e.Execute(context.Background(), plan) //nolint:errcheck
	}()

	// Wait until all N phases have completed (all N goroutines spawned and queued).
	deadline := time.After(10 * time.Second)
	for {
		select {
		case <-deadline:
			e.extractMu.Unlock()
			t.Fatal("timed out waiting for all phases to complete")
		case <-time.After(5 * time.Millisecond):
		}
		n := countEvents(em.collected(), event.PhaseCompleted)
		if n >= numPhases {
			break
		}
	}

	// Execute must still be blocked: goroutines are queued on extractMu.
	select {
	case <-execDone:
		e.extractMu.Unlock()
		t.Fatal("Execute returned while extractMu is held; goroutines did not queue up")
	default:
	}

	// TryLock must fail — we hold the mutex.
	if e.extractMu.TryLock() {
		e.extractMu.Unlock() // clean up acquired lock
		e.extractMu.Unlock() // release the original held lock
		t.Fatal("TryLock succeeded while extractMu should be held; mutex is not protecting the critical section")
	}

	// Release: N goroutines execute ONE AT A TIME (serialised by extractMu).
	e.extractMu.Unlock()

	select {
	case <-execDone:
	case <-time.After(10 * time.Second):
		t.Fatal("Execute did not return after extractMu was released")
	}

	evts := em.collected()

	// All N learnings must be stored (no data loss from serialisation).
	if got := countEvents(evts, event.LearningStored); got != numPhases {
		t.Errorf("learning.stored count = %d; want %d (one per phase, no data loss under serialisation)",
			got, numPhases)
	}

	if firstIndexOf(evts, event.MissionCompleted) < 0 {
		t.Error("mission.completed not emitted")
	}
}

// countingOutputExecutor produces a unique learning output on every call using
// an atomic counter to ensure each phase's content is distinct.
type countingOutputExecutor struct {
	mu     sync.Mutex
	prefix string
	count  int
}

func (c *countingOutputExecutor) Execute(_ context.Context, _ *core.WorkerConfig, _ event.Emitter, _ bool) (string, string, *sdk.CostInfo, error) {
	c.mu.Lock()
	n := c.count
	c.count++
	c.mu.Unlock()
	return makeLearningOutput(fmt.Sprintf("%s-%d", c.prefix, n), 1), "", nil, nil
}

// ---------------------------------------------------------------------------
// 4. Extraction failures do not fail the mission
// ---------------------------------------------------------------------------

// TestExtraction_FailureDoesNotFailMission verifies that a broken learning DB
// (every Insert returns an error) does not cause mission failure. Phases must
// complete and mission.completed must be emitted.
func TestExtraction_FailureDoesNotFailMission(t *testing.T) {
	tests := []struct {
		name      string
		numPhases int
		mode      string
	}{
		{
			name:      "single phase with broken DB — sequential",
			numPhases: 1,
			mode:      "sequential",
		},
		{
			name:      "multiple phases with broken DB — parallel",
			numPhases: 3,
			mode:      "parallel",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Closed DB: all Insert calls return an error.
			db := openClosedDB(t)

			phases := make([]*core.Phase, tt.numPhases)
			phaseIDs := make([]string, tt.numPhases)
			for i := 0; i < tt.numPhases; i++ {
				id := fmt.Sprintf("fail%d", i)
				phaseIDs[i] = id
				phases[i] = extractPhase(id)
			}

			e, em := newExtractEngine(t, db, learningOutputExecutor{output: makeLearningOutput("broken", 1)})

			plan := &core.Plan{
				ID:            "plan-failure-nonfatal",
				Task:          "extraction failure non-fatal test",
				ExecutionMode: tt.mode,
				Phases:        phases,
			}

			result, err := e.Execute(context.Background(), plan)

			// Execute must not return an error due to extraction failure.
			if err != nil {
				t.Fatalf("Execute returned error = %v; extraction failures must not propagate", err)
			}
			if result == nil || !result.Success {
				t.Error("result.Success = false; extraction failures must not fail the mission")
			}

			evts := em.collected()

			// Every phase must still complete.
			for _, id := range phaseIDs {
				var saw bool
				for _, ev := range evts {
					if ev.Type == event.PhaseCompleted && ev.PhaseID == id {
						saw = true
						break
					}
				}
				if !saw {
					t.Errorf("phase.completed not emitted for phase %q despite broken DB", id)
				}
			}

			// mission.completed (not mission.failed) must be emitted.
			if firstIndexOf(evts, event.MissionCompleted) < 0 {
				t.Error("mission.completed not emitted despite broken extraction DB")
			}
			if firstIndexOf(evts, event.MissionFailed) >= 0 {
				t.Error("mission.failed emitted; extraction failures must not fail the mission")
			}

			// No learning.stored events — all inserts failed.
			if n := countEvents(evts, event.LearningStored); n > 0 {
				t.Errorf("got %d learning.stored events; expected 0 when DB is broken", n)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

// TestExtraction_NilDB_NoGoroutineSpawned verifies that when learningDB is nil
// no extraction goroutine is spawned: extractWG stays at zero and Execute
// returns cleanly with no learning events.
func TestExtraction_NilDB_NoGoroutineSpawned(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	ws := &core.Workspace{ID: "extract-nildb-ws", Path: t.TempDir(), Domain: "test"}
	em := &captureEmitter{}
	// nil db: extraction disabled entirely
	e := New(ws, &core.OrchestratorConfig{}, nil, nil).WithEmitter(em)
	e.RegisterExecutor(extractRuntime, learningOutputExecutor{output: makeLearningOutput("nildb", 2)})

	plan := &core.Plan{
		ID:            "plan-nildb",
		Task:          "nil db test",
		ExecutionMode: "sequential",
		Phases:        []*core.Phase{extractPhase("n1"), extractPhase("n2", "n1")},
	}

	result, err := e.Execute(context.Background(), plan)
	if err != nil {
		t.Fatalf("Execute returned error: %v", err)
	}
	if !result.Success {
		t.Error("result.Success = false; nil DB must not affect mission success")
	}

	for _, ev := range em.collected() {
		if ev.Type == event.LearningStored {
			t.Errorf("unexpected learning.stored event when DB is nil")
		}
	}

	// extractMu must be free — no goroutine leaks.
	if !e.extractMu.TryLock() {
		t.Error("extractMu is held after Execute with nil DB; goroutine leaked")
	} else {
		e.extractMu.Unlock()
	}
}

// ---------------------------------------------------------------------------
// 5. Persistent worker phases still emit learning.stored events
// ---------------------------------------------------------------------------

// TestExtraction_PersistentWorkerPhase_EmitsLearningStored verifies that phases
// assigned to the persistent worker still go through the global learning pipeline
// (CaptureWithFocus + learningDB.Insert + learning.stored events). Before the
// fix, the `assignedWorker == nil` guard skipped the entire global path for
// persistent worker phases — this test ensures they're no longer dropped.
//
// The test pre-loads the engine's persistentWorker so that when the random roll
// succeeds, the persistent worker is used. It runs enough iterations to ensure
// at least one persistent worker assignment occurs.
func TestExtraction_PersistentWorkerPhase_EmitsLearningStored(t *testing.T) {
	t.Setenv("HOME", t.TempDir())

	db := openTempDB(t)

	// Use a phase whose output has learning markers.
	exec := learningOutputExecutor{output: makeLearningOutput("pw", 1)}

	ws := &core.Workspace{ID: "xws-pw-test", Path: t.TempDir(), Domain: "test"}
	em := &captureEmitter{}
	e := New(ws, &core.OrchestratorConfig{}, nil, db).WithEmitter(em)
	e.RegisterExecutor(extractRuntime, exec)

	// Pre-load the persistent worker so LoadIdentity is already done.
	wi, err := worker.LoadIdentity(persistentWorkerName)
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}
	e.persistentWorker = wi

	plan := &core.Plan{
		ID:            "plan-pw",
		Task:          "persistent worker learning.stored test",
		ExecutionMode: "sequential",
		Phases:        []*core.Phase{extractPhase("pw1")},
	}

	// Run multiple times until we get a persistent worker assignment.
	// With 30% probability per trial, P(none in 50 trials) < 0.7^50 ≈ 1.8e-8.
	var gotWorkerAssignment bool
	for i := 0; i < 50; i++ {
		// Reset engine state for each iteration.
		em.mu.Lock()
		em.events = nil
		em.mu.Unlock()
		e.persistentWorkerCount = 0

		// Reset phase status for re-execution.
		plan.Phases[0].Status = core.StatusPending
		plan.Phases[0].Worker = ""

		result, execErr := e.Execute(context.Background(), plan)
		if execErr != nil {
			t.Fatalf("Execute returned error: %v", execErr)
		}
		if result == nil || !result.Success {
			t.Fatal("result.Success = false")
		}

		evts := em.collected()

		// Check if the persistent worker was assigned this iteration.
		if plan.Phases[0].Worker == persistentWorkerName {
			gotWorkerAssignment = true

			// The key assertion: learning.stored must still be emitted even when
			// the phase was assigned to a persistent worker.
			if firstIndexOf(evts, event.LearningStored) < 0 {
				t.Fatal("persistent worker phase did not emit learning.stored; " +
					"the global extraction pipeline was skipped")
			}

			break // success — found what we needed
		}

		// Even without persistent worker assignment, learning.stored should fire.
		if firstIndexOf(evts, event.LearningStored) < 0 {
			t.Error("non-worker phase did not emit learning.stored")
		}
	}

	if !gotWorkerAssignment {
		t.Fatal("persistent worker was never assigned in 50 trials (expected ~30% rate)")
	}
}

// Compile-time interface checks.
var _ PhaseExecutor = learningOutputExecutor{}
var _ PhaseExecutor = &countingOutputExecutor{}
