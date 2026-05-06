package engine

import "testing"

func TestResolveModelAlias(t *testing.T) {
	tests := []struct {
		name     string
		provider string
		alias    string
		want     string
	}{
		{"anthropic sonnet", "anthropic", "sonnet", "claude-sonnet-4-6"},
		{"anthropic opus", "anthropic", "opus", "claude-opus-4-7"},
		{"anthropic haiku", "anthropic", "haiku", "claude-haiku-4-5-20251001"},
		{"anthropic full id passthrough", "anthropic", "claude-sonnet-4-5", "claude-sonnet-4-5"},
		{"anthropic unknown passthrough", "anthropic", "weird-model", "weird-model"},
		{"anthropic empty", "anthropic", "", ""},
		{"openrouter sonnet", "openrouter", "sonnet", "anthropic/claude-sonnet-4.6"},
		{"openrouter opus", "openrouter", "opus", "anthropic/claude-opus-4.7"},
		{"openrouter namespaced passthrough", "openrouter", "openai/gpt-4o", "openai/gpt-4o"},
		{"openai passthrough", "openai", "gpt-4o", "gpt-4o"},
		{"unknown provider passthrough", "elsewhere", "sonnet", "sonnet"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := resolveModelAlias(tc.provider, tc.alias)
			if got != tc.want {
				t.Fatalf("resolveModelAlias(%q,%q) = %q, want %q", tc.provider, tc.alias, got, tc.want)
			}
		})
	}
}

func TestAnthropicProviderResolveModel_AliasMapped(t *testing.T) {
	p := anthropicProvider{}
	if got := p.ResolveModel("sonnet"); got != "claude-sonnet-4-6" {
		t.Fatalf("anthropicProvider.ResolveModel(sonnet) = %q", got)
	}
	if got := p.ResolveModel(""); got != "claude-sonnet-4-6" {
		t.Fatalf("anthropicProvider.ResolveModel(empty) = %q", got)
	}
	if got := p.ResolveModel("claude-opus-4-7"); got != "claude-opus-4-7" {
		t.Fatalf("anthropicProvider.ResolveModel(full id) = %q", got)
	}
}
