package engine

// Model alias resolution.
//
// The persona/router layer hands phases short tier-style aliases like
// "sonnet", "opus", or "haiku" — Claude Code (subprocess executor) accepts
// those directly, but Anthropic's REST Messages API requires fully-qualified
// model IDs (e.g. "claude-sonnet-4-6"). Without resolution, an API executor
// run hits a 404 not_found_error.
//
// Resolvers below normalise short aliases to provider-appropriate full IDs.
// Anything that already looks like a full ID passes through unchanged. Unknown
// values pass through too: users may legitimately set rare or new model IDs we
// don't have hard-coded, and surfacing a clear server-side 404 is preferable
// to a client-side reject for those.

// anthropicAliases maps short tier names to the project's current preferred
// Anthropic model IDs. Keep in sync with cmd/nanika/internal/setup/setup.go
// and internal/router/routing_config.
var anthropicAliases = map[string]string{
	"sonnet": "claude-sonnet-4-6",
	"opus":   "claude-opus-4-7",
	"haiku":  "claude-haiku-4-5-20251001",
}

// openaiAliases — currently no short aliases used; left empty for symmetry
// and future use (e.g. mapping "gpt-5" to a dated snapshot).
var openaiAliases = map[string]string{}

// openrouterAliases prefixes the Anthropic short names with "anthropic/" so
// OpenRouter's namespaced model IDs work.
var openrouterAliases = map[string]string{
	"sonnet": "anthropic/claude-sonnet-4.6",
	"opus":   "anthropic/claude-opus-4.7",
	"haiku":  "anthropic/claude-haiku-4.5",
}

// resolveModelAlias returns the full model ID for a (provider, alias) pair.
// If the input is already a full ID, or unknown, it is returned unchanged.
func resolveModelAlias(provider, alias string) string {
	if alias == "" {
		return alias
	}
	var table map[string]string
	switch provider {
	case "anthropic":
		table = anthropicAliases
	case "openai":
		table = openaiAliases
	case "openrouter":
		table = openrouterAliases
	default:
		return alias
	}
	if full, ok := table[alias]; ok {
		return full
	}
	return alias
}
