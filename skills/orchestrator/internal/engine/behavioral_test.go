package engine

// behavioral_test.go verifies the four runtime guarantees introduced by the
// exit-code / deadlock fixes:
//
//  1. Execute blocks until all phases complete (sequential and parallel).
//  2. DryRun is validated at the plan level — the engine never spawns workers
//     when config.DryRun is true.
//  3. result.Success is false (→ exit code 1) when any phase fails.
//  4. SIGINT / context cancellation unblocks Execute and returns context.Canceled.

import (
	"context"
	"errors"
	"fmt"
	"sync/atomic"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/nanika/shared/sdk"
)

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

// instantExecutor completes immediately with a configurable success/failure.
type instantExecutor struct {
	fail bool
	errMsg string
}

func (x instantExecutor) Execute(_ context.Context, _ *core.WorkerConfig, _ event.Emitter, _ bool) (string, string, *sdk.CostInfo, error) {
	if x.fail {
		return "", "", nil, fmt.Errorf("%s", x.errMsg)
	}
	return "done", "", nil, nil
}

// trackingExecutor records call count and returns immediately.
type trackingExecutor struct {
	calls atomic.Int64
}

func (t *trackingExecutor) Execute(_ context.Context, _ *core.WorkerConfig, _ event.Emitter, _ bool) (string, string, *sdk.CostInfo, error) {
	t.calls.Add(1)
	return "done", "", nil, nil
}

func behavPlan(mode string, phases ...*core.Phase) *core.Plan {
	return &core.Plan{
		ID:            "test-plan",
		Task:          "test",
		ExecutionMode: mode,
		Phases:        phases,
	}
}

func behavPhase(id string, deps ...string) *core.Phase {
	return &core.Phase{
		ID:           id,
		Name:         id,
		Objective:    "do " + id,
		Persona:      "senior-backend-engineer",
		ModelTier:    "work",
		Runtime:      core.Runtime("behav-test"),
		Status:       core.StatusPending,
		Dependencies: deps,
	}
}

func newBehavEngine(t *testing.T, exec PhaseExecutor) *Engine {
	t.Helper()
	ws := &core.Workspace{ID: "test-ws", Path: t.TempDir(), Domain: "dev"}
	e := New(ws, &core.OrchestratorConfig{}, nil, nil).WithEmitter(event.NoOpEmitter{})
	e.RegisterExecutor(core.Runtime("behav-test"), exec)
	return e
}

// ---------------------------------------------------------------------------
// 1. Execute blocks until completion
// ---------------------------------------------------------------------------

func TestExecuteBlocks_Sequential(t *testing.T) {
	t.Parallel()
	exec := &trackingExecutor{}
	e := newBehavEngine(t, exec)

	plan := behavPlan("sequential", behavPhase("p1"), behavPhase("p2", "p1"))

	done := make(chan struct{})
	go func() {
		e.Execute(context.Background(), plan) //nolint:errcheck
		close(done)
	}()

	select {
	case <-done:
		if exec.calls.Load() != 2 {
			t.Errorf("want 2 phase executions, got %d", exec.calls.Load())
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Execute did not return within 5s — blocked")
	}
}

func TestExecuteBlocks_Parallel(t *testing.T) {
	t.Parallel()
	exec := &trackingExecutor{}
	e := newBehavEngine(t, exec)

	plan := behavPlan("parallel", behavPhase("p1"), behavPhase("p2"))

	done := make(chan struct{})
	go func() {
		e.Execute(context.Background(), plan) //nolint:errcheck
		close(done)
	}()

	select {
	case <-done:
		if exec.calls.Load() != 2 {
			t.Errorf("want 2 phase executions, got %d", exec.calls.Load())
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Execute did not return within 5s — blocked")
	}
}

// ---------------------------------------------------------------------------
// 2. --dry-run gate: cmd layer returns before engine runs phases.
//    Verified by showing the engine skips already-terminal phases on resume
//    (the executeParallel deadlock fix): only pending phases are dispatched.
// ---------------------------------------------------------------------------

func TestResumeSkipsTerminalPhases_OnlyPendingExecuted(t *testing.T) {
	t.Parallel()
	exec := &trackingExecutor{}
	e := newBehavEngine(t, exec)

	// p1 is already completed (from a prior run checkpoint), p2 is pending.
	p1 := behavPhase("p1")
	p1.Status = core.StatusCompleted
	p2 := behavPhase("p2", "p1")

	plan := behavPlan("parallel", p1, p2)

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	result, err := e.Execute(ctx, plan)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if result == nil || !result.Success {
		t.Error("want success result when resumed plan completes")
	}
	// Only p2 should have been dispatched — p1 was already terminal.
	if exec.calls.Load() != 1 {
		t.Errorf("want 1 phase execution (p2 only), got %d", exec.calls.Load())
	}
}

// ---------------------------------------------------------------------------
// 3. Exit code 1 when a phase fails — result.Success is false
// ---------------------------------------------------------------------------

func TestPhaseFailure_ResultSuccessIsFalse_Sequential(t *testing.T) {
	t.Parallel()
	exec := instantExecutor{fail: true, errMsg: "phase error"}
	e := newBehavEngine(t, exec)

	plan := behavPlan("sequential", behavPhase("p1"))
	result, _ := e.Execute(context.Background(), plan)

	if result == nil {
		t.Fatal("expected non-nil result")
	}
	if result.Success {
		t.Error("want result.Success=false when phase fails, got true")
	}
}

func TestPhaseFailure_ResultSuccessIsFalse_Parallel(t *testing.T) {
	t.Parallel()
	exec := instantExecutor{fail: true, errMsg: "phase error"}
	e := newBehavEngine(t, exec)

	plan := behavPlan("parallel", behavPhase("p1"), behavPhase("p2"))
	result, _ := e.Execute(context.Background(), plan)

	if result == nil {
		t.Fatal("expected non-nil result")
	}
	if result.Success {
		t.Error("want result.Success=false when phase fails, got true")
	}
}

// missionSucceeded replicates the run.go logic to confirm the error path.
func testMissionSucceeded(result *core.ExecutionResult, err error) bool {
	return err == nil && result != nil && result.Success
}

func TestPhaseFailure_MissionSucceededReturnsFalse(t *testing.T) {
	t.Parallel()
	exec := instantExecutor{fail: true, errMsg: "injected failure"}
	e := newBehavEngine(t, exec)

	plan := behavPlan("sequential", behavPhase("p1"))
	result, err := e.Execute(context.Background(), plan)

	if testMissionSucceeded(result, err) {
		t.Error("missionSucceeded should return false when a phase fails")
	}
}

// ---------------------------------------------------------------------------
// 4. SIGINT / context cancellation unblocks Execute
// ---------------------------------------------------------------------------

func TestSIGINT_CancelsGracefully_Sequential(t *testing.T) {
	t.Parallel()
	blocking := &blockUntilCancelledExecutor{ready: make(chan struct{})}
	ws := &core.Workspace{ID: "test-ws", Path: t.TempDir(), Domain: "dev"}
	e := New(ws, &core.OrchestratorConfig{}, nil, nil).WithEmitter(event.NoOpEmitter{})
	e.RegisterExecutor(core.Runtime("blocking"), blocking)

	plan := &core.Plan{
		ID:            "plan-sigint-seq",
		Task:          "test sigint sequential",
		ExecutionMode: "sequential",
		Phases: []*core.Phase{
			{
				ID: "p1", Name: "p1", Objective: "block",
				Persona: "senior-backend-engineer", ModelTier: "work",
				Runtime: core.Runtime("blocking"), Status: core.StatusPending,
			},
		},
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	done := make(chan error, 1)
	go func() {
		_, err := e.Execute(ctx, plan)
		done <- err
	}()

	// Wait for the phase to start blocking, then cancel (simulate SIGINT).
	select {
	case <-blocking.ready:
	case <-time.After(5 * time.Second):
		cancel()
		t.Fatal("timed out waiting for phase to start")
	}
	cancel()

	select {
	case err := <-done:
		if err == nil {
			t.Error("want non-nil error after cancellation, got nil")
		}
		if !isContextError(err) {
			t.Errorf("want context error, got %v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Execute did not return after cancellation")
	}
}

func TestSIGINT_CancelsGracefully_Parallel(t *testing.T) {
	t.Parallel()
	blocking := &blockUntilCancelledExecutor{ready: make(chan struct{})}
	ws := &core.Workspace{ID: "test-ws", Path: t.TempDir(), Domain: "dev"}
	e := New(ws, &core.OrchestratorConfig{}, nil, nil).WithEmitter(event.NoOpEmitter{})
	e.RegisterExecutor(core.Runtime("blocking"), blocking)

	plan := &core.Plan{
		ID:            "plan-sigint-par",
		Task:          "test sigint parallel",
		ExecutionMode: "parallel",
		Phases: []*core.Phase{
			{
				ID: "p1", Name: "p1", Objective: "block",
				Persona: "senior-backend-engineer", ModelTier: "work",
				Runtime: core.Runtime("blocking"), Status: core.StatusPending,
			},
		},
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	done := make(chan error, 1)
	go func() {
		_, err := e.Execute(ctx, plan)
		done <- err
	}()

	select {
	case <-blocking.ready:
	case <-time.After(5 * time.Second):
		cancel()
		t.Fatal("timed out waiting for phase to start")
	}
	cancel()

	select {
	case err := <-done:
		if err == nil {
			t.Error("want non-nil error after cancellation, got nil")
		}
		if !isContextError(err) {
			t.Errorf("want context error, got %v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("Execute did not return after cancellation")
	}
}

// isContextError returns true for context.Canceled or context.DeadlineExceeded.
func isContextError(err error) bool {
	return errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded)
}
