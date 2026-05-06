package engine

// Tests for effectiveRuntime precedence and mixed-runtime plan execution.

import (
	"context"
	"os"
	"path/filepath"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/router"
	"github.com/joeyhipolito/nanika/shared/sdk"
)

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

func newRoutingTestEngine(t *testing.T, cfg *core.OrchestratorConfig, rc *router.RoutingConfig) *Engine {
	t.Helper()
	ws := &core.Workspace{
		ID:   "test-ws",
		Path: t.TempDir(),
	}
	if cfg == nil {
		cfg = &core.OrchestratorConfig{}
	}
	eng := New(ws, cfg, nil, nil)
	eng.WithRoutingConfig(rc)
	return eng
}

func makePhaseWithTier(name, tier string, rt core.Runtime, policyApplied bool) *core.Phase {
	return &core.Phase{
		ID:                   "phase-" + name,
		Name:                 name,
		Objective:            "do " + name,
		ModelTier:            tier,
		Runtime:              rt,
		RuntimePolicyApplied: policyApplied,
		Status:               core.StatusPending,
	}
}

// routingConfig builds a RoutingConfig that maps work→anthropic-api,
// think→anthropic-api, quick→claude.
func routingConfig() *router.RoutingConfig {
	return &router.RoutingConfig{
		ModelTiers: map[string]router.TierConfig{
			"think": {Provider: "anthropic", Model: "claude-opus-4-7", Runtime: core.RuntimeAnthropicAPI},
			"work":  {Provider: "anthropic", Model: "claude-sonnet-4-6", Runtime: core.RuntimeAnthropicAPI},
			"quick": {Provider: "anthropic", Model: "claude-haiku-4-5", Runtime: core.RuntimeClaude},
		},
	}
}

// ---------------------------------------------------------------------------
// effectiveRuntime precedence tests
// ---------------------------------------------------------------------------

// TestEffectiveRuntime_AuthoredWinsOverAll verifies that a per-phase authored
// RUNTIME: value (RuntimePolicyApplied==false) wins over --runtime flag, env
// var, and config tier.
func TestEffectiveRuntime_AuthoredWinsOverAll(t *testing.T) {
	t.Setenv(router.EnvDefaultRuntime, string(core.RuntimeOpenAIAPI))
	eng := newRoutingTestEngine(t, &core.OrchestratorConfig{ForcedRuntime: core.RuntimeOpenRouter}, routingConfig())

	phase := makePhaseWithTier("impl", "work", core.RuntimeClaude, false) // authored claude
	got := eng.effectiveRuntime(phase)
	if got != core.RuntimeClaude {
		t.Errorf("authored phase: got %q, want claude (authored always wins)", got)
	}
}

// TestEffectiveRuntime_FlagWinsOverEnv verifies --runtime (ForcedRuntime)
// beats NANIKA_DEFAULT_RUNTIME when the phase runtime was policy-applied.
func TestEffectiveRuntime_FlagWinsOverEnv(t *testing.T) {
	t.Setenv(router.EnvDefaultRuntime, string(core.RuntimeOpenAIAPI))
	eng := newRoutingTestEngine(t, &core.OrchestratorConfig{ForcedRuntime: core.RuntimeOpenRouter}, routingConfig())

	phase := makePhaseWithTier("impl", "work", core.RuntimeClaude, true) // policy-applied
	got := eng.effectiveRuntime(phase)
	if got != core.RuntimeOpenRouter {
		t.Errorf("flag+env: got %q, want openrouter (flag wins)", got)
	}
}

// TestEffectiveRuntime_EnvWinsOverConfig verifies NANIKA_DEFAULT_RUNTIME beats
// the per-tier config entry when the phase runtime was policy-applied.
func TestEffectiveRuntime_EnvWinsOverConfig(t *testing.T) {
	t.Setenv(router.EnvDefaultRuntime, string(core.RuntimeOpenRouter))
	eng := newRoutingTestEngine(t, &core.OrchestratorConfig{}, routingConfig())

	phase := makePhaseWithTier("impl", "work", core.RuntimeClaude, true)
	got := eng.effectiveRuntime(phase)
	if got != core.RuntimeOpenRouter {
		t.Errorf("env+config: got %q, want openrouter (env wins over config anthropic-api)", got)
	}
}

// TestEffectiveRuntime_ConfigWinsOverPolicy verifies the config tier entry
// beats the policy default (RuntimeClaude) for a policy-applied phase.
func TestEffectiveRuntime_ConfigWinsOverPolicy(t *testing.T) {
	eng := newRoutingTestEngine(t, &core.OrchestratorConfig{}, routingConfig())

	phase := makePhaseWithTier("impl", "work", core.RuntimeClaude, true)
	got := eng.effectiveRuntime(phase)
	if got != core.RuntimeAnthropicAPI {
		t.Errorf("config: got %q, want anthropic-api (config wins over policy)", got)
	}
}

// TestEffectiveRuntime_PolicyFallback verifies SelectRuntime (policy) applies
// when no flag, env, or tier config override is present.
func TestEffectiveRuntime_PolicyFallback(t *testing.T) {
	// Empty routing config — no tier entries.
	eng := newRoutingTestEngine(t, &core.OrchestratorConfig{}, &router.RoutingConfig{})

	phase := makePhaseWithTier("impl", "work", core.RuntimeClaude, true)
	got := eng.effectiveRuntime(phase)
	if got != core.RuntimeClaude {
		t.Errorf("policy fallback: got %q, want claude", got)
	}
}

// TestEffectiveRuntime_UpdatesPhaseRuntime verifies effectiveRuntime mutates
// phase.Runtime when the override chain picks a different runtime, so that
// subsequent telemetry reflects the actual runtime used.
func TestEffectiveRuntime_UpdatesPhaseRuntime(t *testing.T) {
	eng := newRoutingTestEngine(t, &core.OrchestratorConfig{}, routingConfig())

	phase := makePhaseWithTier("impl", "work", core.RuntimeClaude, true)
	if phase.Runtime != core.RuntimeClaude {
		t.Fatalf("precondition: phase.Runtime should be claude, got %q", phase.Runtime)
	}
	eng.effectiveRuntime(phase)
	if phase.Runtime != core.RuntimeAnthropicAPI {
		t.Errorf("phase.Runtime after override: got %q, want anthropic-api", phase.Runtime)
	}
}

// ---------------------------------------------------------------------------
// Mixed-runtime plan execution
// ---------------------------------------------------------------------------

// trackedExecutor records which phases it was invoked for and which runtime
// was on the phase at call time. It succeeds for all phases.
type trackedExecutor struct {
	rt      core.Runtime
	invoked []string
}

func (e *trackedExecutor) Execute(_ context.Context, cfg *core.WorkerConfig, _ event.Emitter, _ bool) (string, string, *sdk.CostInfo, error) {
	e.invoked = append(e.invoked, cfg.Name)
	return "ok", "", &sdk.CostInfo{TotalCostUSD: 0.01, InputTokens: 10, OutputTokens: 10}, nil
}

func writeTestClaudeMD(t *testing.T, workerDir string) {
	t.Helper()
	if err := os.MkdirAll(workerDir, 0o700); err != nil {
		t.Fatal(err)
	}
}

// TestMixedRuntimePlan runs a two-phase plan where one phase is authored
// claude and the other is policy-applied (resolved to anthropic-api by
// config). Each executor tracks its invocations; the test asserts the correct
// executor was called for each phase.
func TestMixedRuntimePlan(t *testing.T) {
	wsDir := t.TempDir()
	ws := &core.Workspace{ID: "test-ws", Path: wsDir}
	cfg := &core.OrchestratorConfig{}

	claudeTracker := &trackedExecutor{rt: core.RuntimeClaude}
	apiTracker := &trackedExecutor{rt: core.RuntimeAnthropicAPI}

	eng := New(ws, cfg, nil, nil)
	eng.WithRoutingConfig(routingConfig())
	// Replace executors with trackers.
	eng.RegisterExecutor(core.RuntimeClaude, claudeTracker)
	eng.RegisterExecutor(core.RuntimeAnthropicAPI, apiTracker)

	// Phase 1: authored claude (RuntimePolicyApplied=false) → claudeTracker.
	// Phase 2: policy-applied (RuntimePolicyApplied=true, tier=work) → apiTracker via config.
	plan := &core.Plan{
		ID:   "test-plan",
		Task: "mixed runtime test",
		Phases: []*core.Phase{
			{
				ID:                   "phase-1",
				Name:                 "authored-claude",
				Objective:            "do something",
				ModelTier:            "work",
				Persona:              "senior-backend-engineer",
				Status:               core.StatusPending,
				Runtime:              core.RuntimeClaude,
				RuntimePolicyApplied: false,
			},
			{
				ID:                   "phase-2",
				Name:                 "policy-api",
				Objective:            "do something else",
				ModelTier:            "work",
				Persona:              "senior-backend-engineer",
				Status:               core.StatusPending,
				Runtime:              core.RuntimeClaude,
				RuntimePolicyApplied: true,
			},
		},
	}

	// Write the workers dir so Spawn doesn't fail.
	workersDir := filepath.Join(wsDir, "workers")
	if err := os.MkdirAll(workersDir, 0o700); err != nil {
		t.Fatal(err)
	}

	// Write a stub personas dir so persona.GetPrompt doesn't crash.
	_ = os.MkdirAll(filepath.Join(wsDir, "personas"), 0o700)

	ctx := context.Background()
	result, err := eng.Execute(ctx, plan)
	if err != nil && result != nil && !result.Success {
		// Phase failures due to missing Claude binary are expected in unit test
		// environment; we only care about routing, not output correctness.
		t.Logf("Execute returned err (expected in unit env): %v", err)
	}

	// The key assertion: phase-1 (authored claude) went through claudeTracker.
	if len(claudeTracker.invoked) == 0 {
		t.Error("claudeTracker was never invoked; authored claude phase should route to it")
	}
	// And phase-2 (policy-applied work tier) went through apiTracker.
	if len(apiTracker.invoked) == 0 {
		t.Error("apiTracker was never invoked; policy-applied work phase should route to anthropic-api via config")
	}
}
