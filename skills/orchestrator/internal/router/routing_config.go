package router

import (
	"os"
	"path/filepath"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"gopkg.in/yaml.v3"
)

// EnvDefaultRuntime is the environment variable that overrides the per-tier
// routing config for any phase whose runtime was not explicitly authored.
// It takes precedence over config.yaml entries but loses to --runtime flag
// and per-phase RUNTIME: annotations.
const EnvDefaultRuntime = "NANIKA_DEFAULT_RUNTIME"

// TierConfig specifies the routing target for a single model tier.
type TierConfig struct {
	Provider string       `yaml:"provider"`
	Model    string       `yaml:"model"`
	Runtime  core.Runtime `yaml:"runtime"`
}

// RoutingConfig holds the default routing table parsed from config.yaml.
// The model_tiers map is keyed by tier name ("think", "work", "quick").
type RoutingConfig struct {
	ModelTiers map[string]TierConfig `yaml:"model_tiers"`
}

// configFile is the config filename inside the orchestrator config dir.
const configFile = "config.yaml"

// LoadRoutingConfig loads and parses config.yaml from configDir.
// Returns DefaultRoutingConfig() when the file is absent, and an error
// only when the file exists but is malformed.
func LoadRoutingConfig(configDir string) (*RoutingConfig, error) {
	path := filepath.Join(configDir, configFile)
	data, err := os.ReadFile(path)
	if os.IsNotExist(err) {
		return DefaultRoutingConfig(), nil
	}
	if err != nil {
		return nil, err
	}
	var rc RoutingConfig
	if err := yaml.Unmarshal(data, &rc); err != nil {
		return nil, err
	}
	if rc.ModelTiers == nil {
		rc.ModelTiers = make(map[string]TierConfig)
	}
	return &rc, nil
}

// DefaultRoutingConfig returns the built-in routing table used when no
// config.yaml is present. It has no tier entries so that policy-applied
// runtimes (SelectRuntime) and authored RUNTIME: values are preserved
// unchanged — existing behaviour is maintained without a config file.
// Users add a config.yaml with model_tiers entries to change this.
func DefaultRoutingConfig() *RoutingConfig {
	return &RoutingConfig{ModelTiers: map[string]TierConfig{}}
}

// RuntimeForTier returns the configured runtime for the given model tier.
// Returns "" when the tier is not in the routing table.
func (rc *RoutingConfig) RuntimeForTier(tier ModelTier) core.Runtime {
	if rc == nil {
		return ""
	}
	if entry, ok := rc.ModelTiers[string(tier)]; ok {
		return entry.Runtime
	}
	return ""
}

// ModelForTier returns the configured model ID for the given tier and runtime.
// Falls back to router.ResolveForRuntime when no entry is found in the table.
func (rc *RoutingConfig) ModelForTier(tier ModelTier, rt core.Runtime) string {
	if rc != nil {
		if entry, ok := rc.ModelTiers[string(tier)]; ok && entry.Runtime == rt && entry.Model != "" {
			return entry.Model
		}
	}
	return ResolveForRuntime(tier, rt)
}

// ProviderForTier returns the provider name configured for the given tier.
// Returns "" when not configured.
func (rc *RoutingConfig) ProviderForTier(tier ModelTier) string {
	if rc == nil {
		return ""
	}
	if entry, ok := rc.ModelTiers[string(tier)]; ok {
		return entry.Provider
	}
	return ""
}

// EffectiveRuntime resolves the runtime for a phase using the full precedence
// chain. It is called by the engine whenever phase.RuntimePolicyApplied is
// true (runtime was filled by policy, not authored).
//
// Precedence (highest to lowest):
//  1. authoredRuntime — per-phase RUNTIME: field (RuntimePolicyApplied==false)
//  2. forcedRuntime   — --runtime CLI flag (core.OrchestratorConfig.ForcedRuntime)
//  3. NANIKA_DEFAULT_RUNTIME env var
//  4. rc.ModelTiers[tier].Runtime — config.yaml per-tier default
//  5. policyRuntime   — SelectRuntime result (the policy-applied value)
//
// The caller must not pass an authoredRuntime when RuntimePolicyApplied==true;
// in that case pass "" and use the policyRuntime as the final fallback.
func (rc *RoutingConfig) EffectiveRuntime(forcedRuntime, policyRuntime core.Runtime, tier ModelTier) core.Runtime {
	if forcedRuntime != "" {
		return forcedRuntime
	}
	if env := os.Getenv(EnvDefaultRuntime); env != "" {
		return core.Runtime(env)
	}
	if rt := rc.RuntimeForTier(tier); rt != "" {
		return rt
	}
	return policyRuntime.Effective()
}
