package engine

import (
	"bytes"
	"context"
	"strings"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/nanika/shared/sdk"
)

type stubExecutor struct {
	output string
}

func (s stubExecutor) Execute(ctx context.Context, config *core.WorkerConfig, emitter event.Emitter, verbose bool) (string, string, *sdk.CostInfo, error) {
	return s.output, "sess-test", nil, nil
}

type recordingExecutor struct {
	output string
	called bool
	config *core.WorkerConfig
}

func (r *recordingExecutor) Execute(ctx context.Context, config *core.WorkerConfig, emitter event.Emitter, verbose bool) (string, string, *sdk.CostInfo, error) {
	r.called = true
	r.config = config
	return r.output, "sess-recorded", nil, nil
}

func TestExecutorRegistryResolve(t *testing.T) {
	reg := defaultRegistry()

	if got := reg.resolve(""); got == nil {
		t.Fatal("resolve(\"\") returned nil")
	}
	if got := reg.resolve(core.RuntimeClaude); got == nil {
		t.Fatal("resolve(RuntimeClaude) returned nil")
	}
	if got := reg.resolve(core.Runtime("unknown-runtime")); got == nil {
		t.Fatal("resolve(unknown) returned nil")
	}

	custom := stubExecutor{output: "ok"}
	reg[core.Runtime("codex")] = custom
	got := reg.resolve(core.Runtime("codex"))
	if _, ok := got.(stubExecutor); !ok {
		t.Fatalf("resolve(codex) returned %T, want stubExecutor", got)
	}
}

func TestEngineRegisterExecutorAndResolve(t *testing.T) {
	e := &Engine{}
	custom := stubExecutor{output: "ok"}
	e.RegisterExecutor(core.Runtime("codex"), custom)

	got := e.resolveExecutor(core.Runtime("codex"))
	if _, ok := got.(stubExecutor); !ok {
		t.Fatalf("resolveExecutor(codex) returned %T, want stubExecutor", got)
	}

	// Unknown runtimes still degrade to Claude for backward compatibility.
	if got := e.resolveExecutor(core.Runtime("typo-runtime")); got == nil {
		t.Fatal("resolveExecutor(unknown) returned nil")
	}
}

func TestRuntimeEffective(t *testing.T) {
	if got := core.Runtime("").Effective(); got != core.RuntimeClaude {
		t.Fatalf("Runtime(\"\").Effective() = %q, want %q", got, core.RuntimeClaude)
	}
	if got := core.Runtime("codex").Effective(); got != core.Runtime("codex") {
		t.Fatalf("Runtime(\"codex\").Effective() = %q, want codex", got)
	}
}

func TestExecutePhase_UsesRegisteredRuntimeExecutor(t *testing.T) {
	wsPath := t.TempDir()
	ws := &core.Workspace{
		ID:     "ws-runtime",
		Path:   wsPath,
		Domain: "dev",
	}
	e := New(ws, &core.OrchestratorConfig{}, nil, nil)
	rec := &recordingExecutor{output: "runtime-output"}
	e.RegisterExecutor(core.Runtime("codex"), rec)

	phase := &core.Phase{
		ID:        "phase-1",
		Name:      "implement",
		Objective: "Implement runtime dispatch",
		Persona:   "senior-backend-engineer",
		ModelTier: "work",
		Runtime:   core.Runtime("codex"),
		Status:    core.StatusPending,
	}
	e.plan = &core.Plan{Task: "Implement runtime dispatch", Phases: []*core.Phase{phase}}
	e.phases[phase.ID] = phase

	output, err := e.executePhase(context.Background(), phase, "")
	if err != nil {
		t.Fatalf("executePhase() error = %v", err)
	}
	if !rec.called {
		t.Fatal("registered runtime executor was not called")
	}
	if rec.config == nil {
		t.Fatal("executor received nil config")
	}
	if rec.config.TargetDir != "" {
		t.Fatalf("config.TargetDir = %q, want empty", rec.config.TargetDir)
	}
	if output != "runtime-output" {
		t.Fatalf("executePhase() output = %q, want %q", output, "runtime-output")
	}
	if phase.SessionID != "sess-recorded" {
		t.Fatalf("phase.SessionID = %q, want %q", phase.SessionID, "sess-recorded")
	}
	if phase.Status != core.StatusCompleted {
		t.Fatalf("phase.Status = %q, want %q", phase.Status, core.StatusCompleted)
	}
}

func TestExecutePhase_EmitsRoleHandoffAndRuntimePolicyVisibility(t *testing.T) {
	wsPath := t.TempDir()
	ws := &core.Workspace{ID: "ws-handoff", Path: wsPath, Domain: "dev"}
	em := &captureEmitter{}
	e := New(ws, &core.OrchestratorConfig{}, nil, nil).WithEmitter(em)
	rec := &recordingExecutor{output: "ok"}
	e.RegisterExecutor(core.Runtime("codex"), rec)

	dep := &core.Phase{
		ID:      "plan",
		Name:    "plan",
		Persona: "architect",
		Role:    core.RolePlanner,
		Status:  core.StatusCompleted,
		Output:  "Created the implementation plan.",
	}
	phase := &core.Phase{
		ID:                   "review",
		Name:                 "review",
		Objective:            "Review the implementation",
		Persona:              "staff-code-reviewer",
		ModelTier:            "think",
		Role:                 core.RoleReviewer,
		Runtime:              core.Runtime("codex"),
		RuntimePolicyApplied: true,
		Dependencies:         []string{"plan"},
		Status:               core.StatusPending,
	}
	e.plan = &core.Plan{Task: "Review the implementation", Phases: []*core.Phase{dep, phase}}
	e.phases[dep.ID] = dep
	e.phases[phase.ID] = phase

	if _, err := e.executePhase(context.Background(), phase, ""); err != nil {
		t.Fatalf("executePhase() error = %v", err)
	}

	var sawHandoff, sawPhaseStarted, sawContractValidated bool
	for _, ev := range em.collected() {
		switch ev.Type {
		case event.RoleHandoff:
			sawHandoff = true
		case event.ContractValidated:
			sawContractValidated = true
		case event.PhaseStarted:
			sawPhaseStarted = true
			if got, _ := ev.Data["runtime"].(string); got != "codex" {
				t.Fatalf("phase.started runtime = %q, want %q", got, "codex")
			}
			if got, _ := ev.Data["runtime_policy_applied"].(bool); !got {
				t.Fatal("phase.started missing runtime_policy_applied=true")
			}
		}
	}
	if !sawHandoff {
		t.Fatal("expected role.handoff event, got none")
	}
	if !sawContractValidated {
		t.Fatal("expected contract.validated event, got none")
	}
	if !sawPhaseStarted {
		t.Fatal("expected phase.started event, got none")
	}
}

// ---------------------------------------------------------------------------
// RuntimeDescriber interface
// ---------------------------------------------------------------------------

func TestClaudeExecutor_ImplementsRuntimeDescriber(t *testing.T) {
	var ex PhaseExecutor = ClaudeExecutor{}
	describer, ok := ex.(RuntimeDescriber)
	if !ok {
		t.Fatal("ClaudeExecutor does not implement RuntimeDescriber")
	}
	desc := describer.Describe()
	if desc.Name != core.RuntimeClaude {
		t.Fatalf("Describe().Name = %q, want %q", desc.Name, core.RuntimeClaude)
	}
	for _, cap := range []core.RuntimeCap{core.CapToolUse, core.CapSessionResume, core.CapStreaming, core.CapCostReport, core.CapArtifacts} {
		if !desc.Caps.Has(cap) {
			t.Errorf("ClaudeExecutor descriptor missing capability %q", cap)
		}
	}
}

func TestStubExecutor_DoesNotImplementRuntimeDescriber(t *testing.T) {
	var ex PhaseExecutor = stubExecutor{output: "ok"}
	if _, ok := ex.(RuntimeDescriber); ok {
		t.Fatal("stubExecutor should not implement RuntimeDescriber")
	}
}

// ---------------------------------------------------------------------------
// executorRegistry.describe
// ---------------------------------------------------------------------------

func TestExecutorRegistryDescribe_KnownRuntime(t *testing.T) {
	reg := defaultRegistry()
	desc, known := reg.describe(core.RuntimeClaude)
	if !known {
		t.Fatal("describe(RuntimeClaude) returned known=false")
	}
	if desc.Name != core.RuntimeClaude {
		t.Fatalf("desc.Name = %q, want %q", desc.Name, core.RuntimeClaude)
	}
}

func TestExecutorRegistryDescribe_UnknownRuntime(t *testing.T) {
	reg := defaultRegistry()
	desc, known := reg.describe(core.Runtime("unknown-runtime"))
	if known {
		t.Fatal("describe(unknown-runtime) should return known=false when not registered")
	}
	if desc.Name != core.Runtime("unknown-runtime") {
		t.Fatalf("desc.Name = %q, want %q", desc.Name, "unknown-runtime")
	}
}

func TestExecutorRegistryDescribe_NonDescriber(t *testing.T) {
	reg := defaultRegistry()
	reg[core.Runtime("stub")] = stubExecutor{output: "ok"}
	desc, known := reg.describe(core.Runtime("stub"))
	if known {
		t.Fatal("describe(stub) should return known=false for non-describer executor")
	}
	if desc.Name != core.Runtime("stub") {
		t.Fatalf("desc.Name = %q, want %q", desc.Name, "stub")
	}
}

// ---------------------------------------------------------------------------
// Contract validation through engine
// ---------------------------------------------------------------------------

func TestCheckContract_ClaudeSatisfiesAllRoles(t *testing.T) {
	wsPath := t.TempDir()
	ws := &core.Workspace{ID: "ws-contract", Path: wsPath, Domain: "dev"}
	em := &captureEmitter{}
	e := New(ws, &core.OrchestratorConfig{}, nil, nil).WithEmitter(em)

	for _, role := range []core.Role{core.RolePlanner, core.RoleImplementer, core.RoleReviewer} {
		phase := &core.Phase{
			ID:      "phase-1",
			Name:    "test",
			Role:    role,
			Runtime: core.RuntimeClaude,
		}
		if err := e.checkContract(context.Background(), phase); err != nil {
			t.Errorf("checkContract(%s) with Claude returned error: %v", role, err)
		}
	}

	validated := 0
	for _, ev := range em.collected() {
		if ev.Type == event.ContractValidated {
			validated++
		}
	}
	if validated != 3 {
		t.Fatalf("ContractValidated events = %d, want 3", validated)
	}
}

func TestCheckContract_UnknownRuntimeSkipsValidation(t *testing.T) {
	wsPath := t.TempDir()
	ws := &core.Workspace{ID: "ws-contract", Path: wsPath, Domain: "dev"}
	em := &captureEmitter{}
	e := New(ws, &core.OrchestratorConfig{Verbose: true}, nil, nil).WithEmitter(em)

	phase := &core.Phase{
		ID:      "phase-1",
		Name:    "test",
		Role:    core.RoleImplementer,
		Runtime: core.Runtime("unknown-custom-runtime"),
	}
	// Should not error — unknown runtimes get a warning, not a failure
	if err := e.checkContract(context.Background(), phase); err != nil {
		t.Fatalf("checkContract with unknown runtime should not error, got: %v", err)
	}

	found := false
	for _, ev := range em.collected() {
		if ev.Type == event.ContractValidated {
			if got, _ := ev.Data["outcome"].(string); got == "skipped" {
				found = true
			}
		}
	}
	if !found {
		t.Fatal("expected contract.validated outcome=skipped event, got none")
	}
}

func TestCheckContract_ViolatedContract(t *testing.T) {
	wsPath := t.TempDir()
	ws := &core.Workspace{ID: "ws-violated", Path: wsPath, Domain: "dev"}
	em := &captureEmitter{}
	e := New(ws, &core.OrchestratorConfig{}, nil, nil).WithEmitter(em)

	// Register a limited executor that only has tool_use (missing artifacts)
	limited := &limitedExecutor{}
	e.RegisterExecutor(core.Runtime("limited"), limited)

	phase := &core.Phase{
		ID:      "phase-1",
		Name:    "implement",
		Role:    core.RoleImplementer, // requires tool_use + artifacts
		Runtime: core.Runtime("limited"),
	}
	err := e.checkContract(context.Background(), phase)
	if err == nil {
		t.Fatal("checkContract should fail when runtime lacks required capabilities")
	}
	if !strings.Contains(err.Error(), "artifacts") {
		t.Errorf("error should mention missing 'artifacts' capability, got: %v", err)
	}

	found := false
	for _, ev := range em.collected() {
		if ev.Type == event.ContractViolated {
			found = true
			break
		}
	}
	if !found {
		t.Fatal("expected contract.violated event, got none")
	}
}

// limitedExecutor implements both PhaseExecutor and RuntimeDescriber but declares
// only CapToolUse — missing CapArtifacts that implementer/reviewer roles require.
type limitedExecutor struct{}

func (limitedExecutor) Execute(ctx context.Context, config *core.WorkerConfig, emitter event.Emitter, verbose bool) (string, string, *sdk.CostInfo, error) {
	return "ok", "", nil, nil
}

func (limitedExecutor) Describe() core.RuntimeDescriptor {
	return core.RuntimeDescriptor{
		Name: core.Runtime("limited"),
		Caps: core.RuntimeCaps{core.CapToolUse: true},
	}
}

func TestResolveExecutor_UnknownRuntimeWarnsAndFallsBack(t *testing.T) {
	var buf bytes.Buffer
	oldWriter := runtimeWarningWriter
	runtimeWarningWriter = &buf
	defer func() {
		runtimeWarningWriter = oldWriter
	}()

	e := &Engine{executors: defaultRegistry()}
	ex := e.resolveExecutor(core.Runtime("codex-typo"))
	if ex == nil {
		t.Fatal("resolveExecutor() returned nil")
	}
	if got := buf.String(); got == "" {
		t.Fatal("expected warning output for unknown runtime, got empty output")
	}
}
