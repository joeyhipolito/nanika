package sdk

import (
	"slices"
	"testing"
)

// flagValue returns the argument immediately following flag, or ("", false) when
// flag is absent (or is the final token with no value).
func flagValue(args []string, flag string) (string, bool) {
	i := slices.Index(args, flag)
	if i < 0 || i+1 >= len(args) {
		return "", false
	}
	return args[i+1], true
}

// TestQueryBuildArgs_AppendSystemPromptAndEffort pins the exact argv produced for
// the two flags added in the briefing/discovery phase: --append-system-prompt
// (persona) and --effort. It also proves --system-prompt is NOT emitted when
// AppendSystemPrompt is set (append wins; the harness's own context assembly must
// not be silently replaced).
func TestQueryBuildArgs_AppendSystemPromptAndEffort(t *testing.T) {
	tests := []struct {
		name       string
		opts       *AgentOptions
		wantAppend string // expected --append-system-prompt value; "" = flag absent
		wantSystem string // expected --system-prompt value; "" = flag absent
		wantEffort string // expected --effort value; "" = flag absent
	}{
		{
			name:       "append and effort both emitted",
			opts:       &AgentOptions{AppendSystemPrompt: "You are the alpha persona.", EffortLevel: "high"},
			wantAppend: "You are the alpha persona.",
			wantEffort: "high",
		},
		{
			name:       "append suppresses system-prompt when both set",
			opts:       &AgentOptions{AppendSystemPrompt: "persona text", SystemPrompt: "REPLACE EVERYTHING"},
			wantAppend: "persona text",
			wantSystem: "", // must be absent
		},
		{
			name:       "system-prompt still emitted when append is empty",
			opts:       &AgentOptions{SystemPrompt: "full replacement"},
			wantAppend: "",
			wantSystem: "full replacement",
		},
		{
			name:       "effort alone",
			opts:       &AgentOptions{EffortLevel: "low"},
			wantEffort: "low",
		},
		{
			name: "neither flag when unset",
			opts: &AgentOptions{Model: "claude-opus-4-8"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			args := queryBuildArgs(tt.opts)
			assertFlag(t, args, "--append-system-prompt", tt.wantAppend)
			assertFlag(t, args, "--system-prompt", tt.wantSystem)
			assertFlag(t, args, "--effort", tt.wantEffort)
		})
	}
}

// assertFlag checks that flag carries want as its value, or is entirely absent
// when want is "".
func assertFlag(t *testing.T, args []string, flag, want string) {
	t.Helper()
	got, has := flagValue(args, flag)
	if want == "" {
		if has {
			t.Errorf("%s present (=%q); want absent. args=%v", flag, got, args)
		}
		return
	}
	if !has || got != want {
		t.Errorf("%s = %q (present=%v); want %q. args=%v", flag, got, has, want, args)
	}
}

// TestQueryBuildArgs_ExactArgvForPersonaTurn asserts the full argv for a
// representative persona+effort turn, so a reordering or accidental extra flag is
// caught, not just presence.
func TestQueryBuildArgs_ExactArgvForPersonaTurn(t *testing.T) {
	opts := &AgentOptions{
		Model:              "claude-opus-4-8",
		AppendSystemPrompt: "persona",
		EffortLevel:        "high",
	}
	got := queryBuildArgs(opts)
	want := []string{
		"--output-format", "stream-json",
		"--print",
		"--verbose",
		"--include-partial-messages",
		"--dangerously-skip-permissions",
		"--model", "claude-opus-4-8",
		"--append-system-prompt", "persona",
		"--effort", "high",
	}
	if !slices.Equal(got, want) {
		t.Errorf("queryBuildArgs argv mismatch\n got = %v\nwant = %v", got, want)
	}
}
