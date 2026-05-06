package router

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
)

// writeConfig writes a config.yaml to dir and returns the path.
func writeConfig(t *testing.T, dir, yaml string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(dir, "config.yaml"), []byte(yaml), 0600); err != nil {
		t.Fatalf("write config.yaml: %v", err)
	}
}

func TestRoutingConfig_LoadFromYAML(t *testing.T) {
	dir := t.TempDir()
	writeConfig(t, dir, `
model_tiers:
  think:
    provider: anthropic
    model: claude-opus-4-7
    runtime: anthropic-api
  work:
    provider: openai
    model: gpt-4o
    runtime: openai-api
  quick:
    provider: anthropic
    model: claude-haiku-4-5
    runtime: claude
`)

	rc, err := LoadRoutingConfig(dir)
	if err != nil {
		t.Fatalf("LoadRoutingConfig: %v", err)
	}

	cases := []struct {
		tier     ModelTier
		wantRT   core.Runtime
		wantMod  string
		wantProv string
	}{
		{TierThink, core.RuntimeAnthropicAPI, "claude-opus-4-7", "anthropic"},
		{TierWork, core.RuntimeOpenAIAPI, "gpt-4o", "openai"},
		{TierQuick, core.RuntimeClaude, "claude-haiku-4-5", "anthropic"},
	}
	for _, c := range cases {
		if got := rc.RuntimeForTier(c.tier); got != c.wantRT {
			t.Errorf("RuntimeForTier(%q) = %q, want %q", c.tier, got, c.wantRT)
		}
		if got := rc.ModelForTier(c.tier, c.wantRT); got != c.wantMod {
			t.Errorf("ModelForTier(%q, %q) = %q, want %q", c.tier, c.wantRT, got, c.wantMod)
		}
		if got := rc.ProviderForTier(c.tier); got != c.wantProv {
			t.Errorf("ProviderForTier(%q) = %q, want %q", c.tier, got, c.wantProv)
		}
	}
}

func TestRoutingConfig_MissingFileReturnsDefaults(t *testing.T) {
	dir := t.TempDir() // no config.yaml written

	rc, err := LoadRoutingConfig(dir)
	if err != nil {
		t.Fatalf("LoadRoutingConfig on missing file: %v", err)
	}
	if rc == nil {
		t.Fatal("expected non-nil RoutingConfig for missing file")
	}
	// Default has no tier entries — RuntimeForTier returns "" so the caller
	// falls through to policy (SelectRuntime), preserving existing behaviour.
	if rt := rc.RuntimeForTier(TierWork); rt != "" {
		t.Errorf("default work tier runtime = %q, want \"\" (no-op)", rt)
	}
}

// TestEffectiveRuntime_Precedence verifies the full precedence chain.
//
//  1. ForcedRuntime (--runtime flag) beats env var and config.
//  2. NANIKA_DEFAULT_RUNTIME env var beats config.
//  3. Config tier entry beats policy default.
//  4. policyRuntime is the fallback when nothing else matches.
func TestEffectiveRuntime_Precedence(t *testing.T) {
	dir := t.TempDir()
	writeConfig(t, dir, `
model_tiers:
  work:
    provider: anthropic
    model: claude-sonnet-4-6
    runtime: anthropic-api
`)
	rc, err := LoadRoutingConfig(dir)
	if err != nil {
		t.Fatalf("LoadRoutingConfig: %v", err)
	}

	// Clean up env between sub-tests.
	unsetEnv := func() { t.Helper(); os.Unsetenv(EnvDefaultRuntime) }

	t.Run("flag wins over env", func(t *testing.T) {
		t.Setenv(EnvDefaultRuntime, string(core.RuntimeOpenAIAPI))
		got := rc.EffectiveRuntime(core.RuntimeOpenRouter, core.RuntimeClaude, TierWork)
		if got != core.RuntimeOpenRouter {
			t.Errorf("got %q, want openrouter (flag wins)", got)
		}
		unsetEnv()
	})

	t.Run("env wins over config", func(t *testing.T) {
		t.Setenv(EnvDefaultRuntime, string(core.RuntimeOpenRouter))
		got := rc.EffectiveRuntime("", core.RuntimeClaude, TierWork)
		if got != core.RuntimeOpenRouter {
			t.Errorf("got %q, want openrouter (env wins over config anthropic-api)", got)
		}
		unsetEnv()
	})

	t.Run("config wins over policy", func(t *testing.T) {
		got := rc.EffectiveRuntime("", core.RuntimeClaude, TierWork)
		if got != core.RuntimeAnthropicAPI {
			t.Errorf("got %q, want anthropic-api (config wins over policy claude)", got)
		}
	})

	t.Run("policy fallback when tier missing", func(t *testing.T) {
		// TierThink is not in the config above.
		got := rc.EffectiveRuntime("", core.RuntimeClaude, TierThink)
		if got != core.RuntimeClaude {
			t.Errorf("got %q, want claude (policy fallback)", got)
		}
	})

	t.Run("per-phase authored runtime wins over all", func(t *testing.T) {
		// The caller (engine.effectiveRuntime) guards this: it returns
		// phase.Runtime directly when RuntimePolicyApplied==false. Here we
		// verify that EffectiveRuntime with a forcedRuntime still works —
		// the authored guard is in the engine, not RoutingConfig.
		t.Setenv(EnvDefaultRuntime, string(core.RuntimeOpenAIAPI))
		got := rc.EffectiveRuntime(core.RuntimeAnthropicAPI, core.RuntimeClaude, TierWork)
		if got != core.RuntimeAnthropicAPI {
			t.Errorf("got %q, want anthropic-api (forced wins)", got)
		}
		unsetEnv()
	})
}

func TestRoutingConfig_AllKnownRuntimes(t *testing.T) {
	// Verify all runtime constants round-trip through config.yaml.
	dir := t.TempDir()
	writeConfig(t, dir, `
model_tiers:
  think:
    provider: google
    model: gemini-2.0-flash
    runtime: gemini-api
  work:
    provider: openrouter
    model: anthropic/claude-sonnet-4-6
    runtime: openrouter
  quick:
    provider: openai
    model: gpt-4o-mini
    runtime: openai-api
`)
	rc, err := LoadRoutingConfig(dir)
	if err != nil {
		t.Fatalf("LoadRoutingConfig: %v", err)
	}
	if rt := rc.RuntimeForTier(TierThink); rt != core.RuntimeGeminiAPI {
		t.Errorf("think tier: got %q, want gemini-api", rt)
	}
	if rt := rc.RuntimeForTier(TierWork); rt != core.RuntimeOpenRouter {
		t.Errorf("work tier: got %q, want openrouter", rt)
	}
	if rt := rc.RuntimeForTier(TierQuick); rt != core.RuntimeOpenAIAPI {
		t.Errorf("quick tier: got %q, want openai-api", rt)
	}
}
