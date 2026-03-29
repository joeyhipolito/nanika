package engine

// Tests for configurable GateMode (warn/block) behavior.
//
// These integration tests run Execute with a stub executor that returns
// output intentionally short enough to trigger the format gate (< 100 chars
// containing an error phrase), then assert the phase outcome matches the mode.

import (
	"context"
	"strings"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/sdk"
)

// shortErrorExecutor returns output that is short and contains an error phrase,
// which triggers CheckGate to fail with "output appears to be an error".
type shortErrorExecutor struct{}

func (s shortErrorExecutor) Execute(_ context.Context, _ *core.WorkerConfig, _ event.Emitter, _ bool) (string, string, *sdk.CostInfo, error) {
	return "error: something went wrong", "sess-gate-test", nil, nil
}

func newGateModeEngine(t *testing.T, mode core.GateMode) *Engine {
	t.Helper()
	wsPath := t.TempDir()
	ws := &core.Workspace{ID: "ws-gate-mode-test", Path: wsPath, Domain: "dev"}
	cfg := &core.OrchestratorConfig{GateMode: mode}
	e := New(ws, cfg, nil, nil, "").WithEmitter(event.NoOpEmitter{})
	e.RegisterExecutor(core.Runtime("stub-gate"), shortErrorExecutor{})
	return e
}

func singlePhaseGatePlan() *core.Plan {
	return &core.Plan{
		ID:            "plan-gate-mode-test",
		Task:          "test gate mode",
		ExecutionMode: "sequential",
		Phases: []*core.Phase{
			{
				ID:        "phase-1",
				Name:      "impl",
				Objective: "do work",
				Persona:   "senior-backend-engineer",
				ModelTier: "work",
				Runtime:   core.Runtime("stub-gate"),
				Status:    core.StatusPending,
			},
		},
	}
}

// TestGateModeBlock verifies that when GateMode is block (the default),
// a phase whose output fails the quality gate is marked failed.
func TestGateModeBlock(t *testing.T) {
	e := newGateModeEngine(t, core.GateModeBlock)
	plan := singlePhaseGatePlan()

	result, _ := e.Execute(context.Background(), plan)

	phase := plan.Phases[0]
	if phase.Status != core.StatusFailed {
		t.Errorf("phase status = %q; want %q (gate should block)", phase.Status, core.StatusFailed)
	}
	if phase.GatePassed {
		t.Error("phase.GatePassed = true; want false")
	}
	if !strings.Contains(phase.Error, "gate:") {
		t.Errorf("phase.Error = %q; want it to contain \"gate:\"", phase.Error)
	}
	if result.Success {
		t.Error("result.Success = true; want false when gate blocks")
	}
}

// TestGateModeWarn verifies that when GateMode is warn, a phase whose output
// fails the quality gate is still marked completed (fail-forward).
func TestGateModeWarn(t *testing.T) {
	e := newGateModeEngine(t, core.GateModeWarn)
	plan := singlePhaseGatePlan()

	result, err := e.Execute(context.Background(), plan)

	if err != nil {
		t.Fatalf("Execute returned error %v; want nil in warn mode", err)
	}
	phase := plan.Phases[0]
	if phase.Status != core.StatusCompleted {
		t.Errorf("phase status = %q; want %q (gate should warn, not block)", phase.Status, core.StatusCompleted)
	}
	if phase.GatePassed {
		t.Error("phase.GatePassed = true; want false (gate did fail, just not blocking)")
	}
	if !result.Success {
		t.Error("result.Success = false; want true in warn mode")
	}
}

// TestGateModeDefaultIsBlock verifies that the zero value of OrchestratorConfig
// (GateMode == "") behaves as block, so existing callers that don't set GateMode
// get fail-fast behavior.
func TestGateModeDefaultIsBlock(t *testing.T) {
	wsPath := t.TempDir()
	ws := &core.Workspace{ID: "ws-gate-default-test", Path: wsPath, Domain: "dev"}
	// GateMode intentionally left as zero value.
	cfg := &core.OrchestratorConfig{}
	e := New(ws, cfg, nil, nil, "").WithEmitter(event.NoOpEmitter{})
	e.RegisterExecutor(core.Runtime("stub-gate"), shortErrorExecutor{})

	plan := singlePhaseGatePlan()
	result, _ := e.Execute(context.Background(), plan)

	phase := plan.Phases[0]
	if phase.Status != core.StatusFailed {
		t.Errorf("phase status = %q; want %q (default GateMode should block)", phase.Status, core.StatusFailed)
	}
	if result.Success {
		t.Error("result.Success = true; want false with default GateMode")
	}
}
