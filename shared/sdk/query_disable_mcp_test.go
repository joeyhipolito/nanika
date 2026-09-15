package sdk

import (
	"slices"
	"testing"
)

// TestQueryBuildArgs_DisableMCP pins the TRK-1135 spawn-latency fix: when
// AgentOptions.DisableMCP is set, the claude argv builders emit
// `--strict-mcp-config --mcp-config {"mcpServers":{}}` (strict makes the empty
// config authoritative instead of merging with ~/.claude.json); when unset,
// neither flag appears. Arg-shape only — no live CLI.
func TestQueryBuildArgs_DisableMCP(t *testing.T) {
	t.Run("emitted when set", func(t *testing.T) {
		args := queryBuildArgs(&AgentOptions{DisableMCP: true})
		if !slices.Contains(args, "--strict-mcp-config") {
			t.Errorf("--strict-mcp-config absent. args=%v", args)
		}
		assertFlag(t, args, "--mcp-config", `{"mcpServers":{}}`)
	})
	t.Run("absent when unset", func(t *testing.T) {
		args := queryBuildArgs(&AgentOptions{Model: "claude-sonnet-5"})
		if slices.Contains(args, "--strict-mcp-config") {
			t.Errorf("--strict-mcp-config present; want absent. args=%v", args)
		}
		assertFlag(t, args, "--mcp-config", "")
	})
}
