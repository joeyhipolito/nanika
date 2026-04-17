package engine

// Regression coverage for terminal state and event consistency during
// context cancellation. Specifically verifies that phases which were never
// dispatched (dependencies not yet satisfied when cancellation fires) receive
// a phase.skipped event and reach StatusSkipped rather than staying "pending"
// in LiveState forever.

import (
	"context"
	"sync"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/sdk"
)

// blockUntilCancelledExecutor signals readiness the first time Execute is
// called, then blocks until its context is cancelled. This lets tests
// precisely control when cancellation fires relative to phase dispatch.
type blockUntilCancelledExecutor struct {
	once  sync.Once
	ready chan struct{} // closed when Execute is first called
}

func (b *blockUntilCancelledExecutor) Execute(ctx context.Context, _ *core.WorkerConfig, _ event.Emitter, _ bool) (string, string, *sdk.CostInfo, error) {
	b.once.Do(func() { close(b.ready) })
	<-ctx.Done()
	return "", "", nil, ctx.Err()
}

// TestExecuteParallel_CancelledPendingPhasesGetSkippedEvent verifies that when
// a mission is cancelled while phase A is executing, phase B (which depends on
// A and was never dispatched) receives a phase.skipped terminal event. Without
// this fix, B would stay in "pending" — causing LiveState to show it as
// permanently in-progress and leaving the checkpoint inconsistent.
func TestExecuteParallel_CancelledPendingPhasesGetSkippedEvent(t *testing.T) {
	wsPath := t.TempDir()
	ws := &core.Workspace{ID: "ws-cancel-test", Path: wsPath, Domain: "dev"}
	em := &captureEmitter{}

	blocking := &blockUntilCancelledExecutor{ready: make(chan struct{})}

	e := New(ws, &core.OrchestratorConfig{}, nil, nil).WithEmitter(em)
	e.RegisterExecutor(core.Runtime("blocking"), blocking)

	// Phase A: no deps, dispatched immediately, will block in the executor.
	// Phase B: depends on A, so it is never dispatched before we cancel.
	plan := &core.Plan{
		ID:            "plan-cancel-test",
		Task:          "test cancellation pending phase events",
		ExecutionMode: "parallel",
		Phases: []*core.Phase{
			{
				ID:        "a",
				Name:      "A",
				Objective: "do a",
				Persona:   "senior-backend-engineer",
				ModelTier: "work",
				Runtime:   core.Runtime("blocking"),
				Status:    core.StatusPending,
			},
			{
				ID:           "b",
				Name:         "B",
				Objective:    "do b",
				Persona:      "senior-backend-engineer",
				ModelTier:    "work",
				Runtime:      core.Runtime("blocking"),
				Status:       core.StatusPending,
				Dependencies: []string{"a"},
			},
		},
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	execDone := make(chan error, 1)
	go func() {
		_, err := e.Execute(ctx, plan)
		execDone <- err
	}()

	// Wait for phase A's executor to start blocking before we cancel.
	select {
	case <-blocking.ready:
	case <-time.After(5 * time.Second):
		cancel()
		t.Fatal("timed out waiting for phase A to start executing")
	}

	// Cancel while A is running and B has never been dispatched.
	cancel()

	select {
	case err := <-execDone:
		if err == nil {
			t.Fatal("expected non-nil error from cancelled Execute")
		}
	case <-time.After(10 * time.Second):
		t.Fatal("Execute did not return after cancellation")
	}

	// Phase B must have a phase.skipped terminal event with the expected reason.
	var sawBSkipped bool
	for _, ev := range em.collected() {
		if ev.Type == event.PhaseSkipped && ev.PhaseID == "b" {
			sawBSkipped = true
			if reason, _ := ev.Data["reason"].(string); reason != "mission cancelled" {
				t.Errorf("phase.skipped reason = %q; want %q", reason, "mission cancelled")
			}
		}
	}
	if !sawBSkipped {
		t.Error("expected phase.skipped event for never-dispatched phase B; got none")
	}

	// Phase B's in-memory state must reflect the skip.
	phaseB := plan.Phases[1]
	if phaseB.Status != core.StatusSkipped {
		t.Errorf("phase B status = %q; want %q", phaseB.Status, core.StatusSkipped)
	}
	if phaseB.Error != "skipped: mission cancelled" {
		t.Errorf("phase B error = %q; want %q", phaseB.Error, "skipped: mission cancelled")
	}
}

// TestExecuteParallel_CancelEmitsSingleMissionTerminalEvent verifies that
// cancellation produces exactly ONE mission-terminal event and that it is
// mission.cancelled — not mission.failed — so LiveState and event consumers
// see a coherent terminal state. Prior to the fix, executeParallel emitted
// mission.cancelled internally and Execute() then ALSO emitted mission.failed
// because err != nil, producing two conflicting terminal events.
func TestExecuteParallel_CancelEmitsSingleMissionTerminalEvent(t *testing.T) {
	wsPath := t.TempDir()
	ws := &core.Workspace{ID: "ws-single-terminal", Path: wsPath, Domain: "dev"}
	em := &captureEmitter{}

	blocking := &blockUntilCancelledExecutor{ready: make(chan struct{})}

	e := New(ws, &core.OrchestratorConfig{}, nil, nil).WithEmitter(em)
	e.RegisterExecutor(core.Runtime("blocking"), blocking)

	plan := &core.Plan{
		ID:            "plan-single-terminal",
		Task:          "test single terminal event",
		ExecutionMode: "parallel",
		Phases: []*core.Phase{
			{
				ID:        "a",
				Name:      "A",
				Objective: "do a",
				Persona:   "senior-backend-engineer",
				ModelTier: "work",
				Runtime:   core.Runtime("blocking"),
				Status:    core.StatusPending,
			},
		},
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	execDone := make(chan error, 1)
	go func() {
		_, err := e.Execute(ctx, plan)
		execDone <- err
	}()

	select {
	case <-blocking.ready:
	case <-time.After(5 * time.Second):
		cancel()
		t.Fatal("timed out waiting for phase A to start")
	}
	cancel()

	select {
	case <-execDone:
	case <-time.After(10 * time.Second):
		t.Fatal("Execute did not return after cancellation")
	}

	evts := em.collected()

	// Count mission-terminal events (completed, failed, cancelled).
	var terminals []event.Event
	for _, ev := range evts {
		switch ev.Type {
		case event.MissionCompleted, event.MissionFailed, event.MissionCancelled:
			terminals = append(terminals, ev)
		}
	}

	if len(terminals) != 1 {
		types := make([]string, len(terminals))
		for i, ev := range terminals {
			types[i] = string(ev.Type)
		}
		t.Fatalf("expected exactly 1 mission-terminal event; got %d: %v", len(terminals), types)
	}
	if terminals[0].Type != event.MissionCancelled {
		t.Errorf("mission terminal event type = %q; want %q", terminals[0].Type, event.MissionCancelled)
	}
}

// TestExecuteSequential_CancelledPendingPhasesGetSkippedEvent verifies that
// when a sequential mission is cancelled while phase A is executing, phase B
// (which had not yet started) receives a phase.skipped terminal event and
// reaches StatusSkipped. Without the fix, B would stay permanently pending.
func TestExecuteSequential_CancelledPendingPhasesGetSkippedEvent(t *testing.T) {
	wsPath := t.TempDir()
	ws := &core.Workspace{ID: "ws-seq-cancel", Path: wsPath, Domain: "dev"}
	em := &captureEmitter{}

	blocking := &blockUntilCancelledExecutor{ready: make(chan struct{})}

	e := New(ws, &core.OrchestratorConfig{ForceSequential: true}, nil, nil).WithEmitter(em)
	e.RegisterExecutor(core.Runtime("blocking"), blocking)

	plan := &core.Plan{
		ID:            "plan-seq-cancel",
		Task:          "test sequential cancellation",
		ExecutionMode: "sequential",
		Phases: []*core.Phase{
			{
				ID:        "a",
				Name:      "A",
				Objective: "do a",
				Persona:   "senior-backend-engineer",
				ModelTier: "work",
				Runtime:   core.Runtime("blocking"),
				Status:    core.StatusPending,
			},
			{
				ID:        "b",
				Name:      "B",
				Objective: "do b",
				Persona:   "senior-backend-engineer",
				ModelTier: "work",
				Runtime:   core.Runtime("blocking"),
				Status:    core.StatusPending,
			},
		},
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	execDone := make(chan error, 1)
	go func() {
		_, err := e.Execute(ctx, plan)
		execDone <- err
	}()

	select {
	case <-blocking.ready:
	case <-time.After(5 * time.Second):
		cancel()
		t.Fatal("timed out waiting for phase A to start")
	}
	cancel()

	select {
	case err := <-execDone:
		if err == nil {
			t.Fatal("expected non-nil error from cancelled Execute")
		}
	case <-time.After(10 * time.Second):
		t.Fatal("Execute did not return after cancellation")
	}

	var sawBSkipped bool
	for _, ev := range em.collected() {
		if ev.Type == event.PhaseSkipped && ev.PhaseID == "b" {
			sawBSkipped = true
			if reason, _ := ev.Data["reason"].(string); reason != "mission cancelled" {
				t.Errorf("phase.skipped reason = %q; want %q", reason, "mission cancelled")
			}
		}
	}
	if !sawBSkipped {
		t.Error("expected phase.skipped event for unstarted phase B; got none")
	}

	phaseB := plan.Phases[1]
	if phaseB.Status != core.StatusSkipped {
		t.Errorf("phase B status = %q; want %q", phaseB.Status, core.StatusSkipped)
	}
}

// TestExecuteSequential_CancelEmitsMissionCancelled verifies that the
// sequential execution path emits mission.cancelled (not mission.failed) and
// exactly one mission-terminal event when the context is cancelled.
func TestExecuteSequential_CancelEmitsMissionCancelled(t *testing.T) {
	wsPath := t.TempDir()
	ws := &core.Workspace{ID: "ws-seq-mission-cancel", Path: wsPath, Domain: "dev"}
	em := &captureEmitter{}

	blocking := &blockUntilCancelledExecutor{ready: make(chan struct{})}

	e := New(ws, &core.OrchestratorConfig{ForceSequential: true}, nil, nil).WithEmitter(em)
	e.RegisterExecutor(core.Runtime("blocking"), blocking)

	plan := &core.Plan{
		ID:            "plan-seq-mission-cancel",
		Task:          "test sequential mission.cancelled emission",
		ExecutionMode: "sequential",
		Phases: []*core.Phase{
			{
				ID:        "x",
				Name:      "X",
				Objective: "do x",
				Persona:   "senior-backend-engineer",
				ModelTier: "work",
				Runtime:   core.Runtime("blocking"),
				Status:    core.StatusPending,
			},
		},
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	execDone := make(chan error, 1)
	go func() {
		_, err := e.Execute(ctx, plan)
		execDone <- err
	}()

	select {
	case <-blocking.ready:
	case <-time.After(5 * time.Second):
		cancel()
		t.Fatal("timed out waiting for phase X to start")
	}
	cancel()

	select {
	case <-execDone:
	case <-time.After(10 * time.Second):
		t.Fatal("Execute did not return after cancellation")
	}

	evts := em.collected()
	var terminals []event.Event
	for _, ev := range evts {
		switch ev.Type {
		case event.MissionCompleted, event.MissionFailed, event.MissionCancelled:
			terminals = append(terminals, ev)
		}
	}

	if len(terminals) != 1 {
		types := make([]string, len(terminals))
		for i, ev := range terminals {
			types[i] = string(ev.Type)
		}
		t.Fatalf("expected exactly 1 mission-terminal event; got %d: %v", len(terminals), types)
	}
	if terminals[0].Type != event.MissionCancelled {
		t.Errorf("mission terminal event = %q; want %q", terminals[0].Type, event.MissionCancelled)
	}
}

// TestExecuteParallel_AllPendingOnPreCancelledContext verifies that when
// Execute is called with an already-cancelled context, every phase that was
// not yet dispatched still reaches a terminal state (skipped). This covers the
// edge case where the context is cancelled before the event loop even starts.
func TestExecuteParallel_AllPendingOnPreCancelledContext(t *testing.T) {
	wsPath := t.TempDir()
	ws := &core.Workspace{ID: "ws-precancel-test", Path: wsPath, Domain: "dev"}
	em := &captureEmitter{}

	// Use a blocking executor for A so it does not complete synchronously.
	blocking := &blockUntilCancelledExecutor{ready: make(chan struct{})}

	e := New(ws, &core.OrchestratorConfig{}, nil, nil).WithEmitter(em)
	e.RegisterExecutor(core.Runtime("blocking"), blocking)

	plan := &core.Plan{
		ID:            "plan-precancel",
		Task:          "test pre-cancelled context",
		ExecutionMode: "parallel",
		Phases: []*core.Phase{
			{
				ID:        "x",
				Name:      "X",
				Objective: "do x",
				Persona:   "senior-backend-engineer",
				ModelTier: "work",
				Runtime:   core.Runtime("blocking"),
				Status:    core.StatusPending,
			},
			{
				ID:           "y",
				Name:         "Y",
				Objective:    "do y",
				Persona:      "senior-backend-engineer",
				ModelTier:    "work",
				Runtime:      core.Runtime("blocking"),
				Status:       core.StatusPending,
				Dependencies: []string{"x"},
			},
			{
				ID:           "z",
				Name:         "Z",
				Objective:    "do z",
				Persona:      "senior-backend-engineer",
				ModelTier:    "work",
				Runtime:      core.Runtime("blocking"),
				Status:       core.StatusPending,
				Dependencies: []string{"y"},
			},
		},
	}

	// Cancel AFTER phase X is dispatched (so Y and Z remain Pending).
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	execDone := make(chan error, 1)
	go func() {
		_, err := e.Execute(ctx, plan)
		execDone <- err
	}()

	select {
	case <-blocking.ready:
	case <-time.After(5 * time.Second):
		cancel()
		t.Fatal("timed out waiting for phase X to start executing")
	}
	cancel()

	select {
	case <-execDone:
	case <-time.After(10 * time.Second):
		t.Fatal("Execute did not return after cancellation")
	}

	// Y and Z were never dispatched; both must reach a terminal state.
	for _, phaseID := range []string{"y", "z"} {
		var sawSkipped bool
		for _, ev := range em.collected() {
			if ev.Type == event.PhaseSkipped && ev.PhaseID == phaseID {
				sawSkipped = true
				break
			}
		}
		if !sawSkipped {
			t.Errorf("expected phase.skipped event for never-dispatched phase %q; got none", phaseID)
		}
	}

	// Statuses must be terminal for Y and Z.
	for i, phaseID := range []string{"y", "z"} {
		p := plan.Phases[i+1]
		if p.ID != phaseID {
			t.Fatalf("unexpected phase ordering; phases[%d].ID = %q, want %q", i+1, p.ID, phaseID)
		}
		if !p.Status.IsTerminal() {
			t.Errorf("phase %q status = %q; want terminal", phaseID, p.Status)
		}
	}
}
