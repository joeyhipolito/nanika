package sdk

import "testing"

// TestEnvAllowlist_IncludesClaudeConfigDir pins the TRK-1118 pt1 addition: a
// user's custom CLAUDE_CONFIG_DIR must survive the strict allowlist so the SDK
// subprocess reads the same Claude Code config the user configured.
func TestEnvAllowlist_IncludesClaudeConfigDir(t *testing.T) {
	if !baseAllowedEnvVars["CLAUDE_CONFIG_DIR"] {
		t.Fatalf("CLAUDE_CONFIG_DIR must be in baseAllowedEnvVars (TRK-1118 pt1)")
	}
}

// TestEnvAllowlist_StillMinimal count-pins the allowlist so Wave 3 grows it by
// EXACTLY one key (CLAUDE_CONFIG_DIR) over the prior snapshot. A drop here that
// widens the set — the passthrough footgun the audit test guards — fails loudly
// rather than silently opening a credential-leak surface. PassthroughEnv stays
// unset (asserted by cmd/nanika/passthrough_env_audit_test.go and
// gateway_resume_test.go).
func TestEnvAllowlist_StillMinimal(t *testing.T) {
	// The exact set after the Wave-3 addition. Update this snapshot ONLY when a
	// key is deliberately added, and only alongside its own justification.
	want := map[string]bool{
		"HOME":                 true,
		"PATH":                 true,
		"LANG":                 true,
		"TERM":                 true,
		"USER":                 true,
		"SHELL":                true,
		"TMPDIR":               true,
		"ANTHROPIC_BASE_URL":   true,
		"ANTHROPIC_API_KEY":    true,
		"ANTHROPIC_AUTH_TOKEN": true,
		"CLAUDE_CONFIG_DIR":    true,
	}
	if len(baseAllowedEnvVars) != len(want) {
		t.Fatalf("baseAllowedEnvVars size drift: got %d keys, want %d (%v)", len(baseAllowedEnvVars), len(want), keysOf(baseAllowedEnvVars))
	}
	for k := range want {
		if !baseAllowedEnvVars[k] {
			t.Errorf("baseAllowedEnvVars missing expected key %q", k)
		}
	}
	for k := range baseAllowedEnvVars {
		if !want[k] {
			t.Errorf("baseAllowedEnvVars has unexpected key %q (allowlist widened without updating this snapshot)", k)
		}
	}
}

func keysOf(m map[string]bool) []string {
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	return out
}
