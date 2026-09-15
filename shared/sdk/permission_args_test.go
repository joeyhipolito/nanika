package sdk

import (
	"slices"
	"strings"
	"testing"
)

// TestPermissionArgs_ZeroValueEmitsSkipPermissions is the named regression test
// that pins the byte-identical invariant: for nil / empty / ""-mode / "bypass"
// opts, every builder emits exactly ["--dangerously-skip-permissions"] and
// nothing else — no --permission-prompt-tool, no --permission-mode. This is the
// worker-safety guarantee: the orchestrator/worker path (which never sets
// PermissionMode) is unchanged from before preventive mode existed.
func TestPermissionArgs_ZeroValueEmitsSkipPermissions(t *testing.T) {
	t.Parallel()
	cases := []*AgentOptions{
		nil,
		{},
		{PermissionMode: ""},
		{PermissionMode: "bypass"},
	}
	for _, opts := range cases {
		for _, duplex := range []bool{false, true} {
			got := permissionArgs(opts, duplex)
			if len(got) != 1 || got[0] != "--dangerously-skip-permissions" {
				t.Fatalf("zero/bypass opts (%+v, duplex=%v) must emit only --dangerously-skip-permissions, got %v", opts, duplex, got)
			}
		}
	}
}

// TestPermissionArgs_PreventiveIsDuplexOnly pins that preventive activates the
// control-protocol gate only on the duplex path. One-shot entrypoints reject it.
func TestPermissionArgs_PreventiveIsDuplexOnly(t *testing.T) {
	t.Parallel()
	pre := &AgentOptions{PermissionMode: PermissionPreventive}
	if got := permissionArgs(pre, true); !slices.Equal(got,
		[]string{"--permission-mode", "default", "--permission-prompt-tool", "stdio"}) {
		t.Fatalf("duplex preventive argv drifted: %v", got)
	}
	if got := permissionArgs(pre, false); len(got) != 0 {
		t.Fatalf("one-shot preventive must never emit bypass: %v", got)
	}
}

// TestQueryBuildArgs_DefaultKeepsSkipPermissions guards the one-shot builder's
// default (worker) path at the argv level: the built args contain
// --dangerously-skip-permissions and never leak the preventive flags. Fails
// loudly if the default ever flips.
func TestQueryBuildArgs_DefaultKeepsSkipPermissions(t *testing.T) {
	t.Parallel()
	for _, opts := range []*AgentOptions{nil, {}, {Model: "claude-opus-4"}} {
		joined := strings.Join(queryBuildArgs(opts), " ")
		if !strings.Contains(joined, "--dangerously-skip-permissions") {
			t.Fatalf("queryBuildArgs(%+v) must keep --dangerously-skip-permissions: %s", opts, joined)
		}
		if strings.Contains(joined, "--permission-prompt-tool") {
			t.Fatalf("queryBuildArgs(%+v) must NOT emit --permission-prompt-tool by default: %s", opts, joined)
		}
	}
}
