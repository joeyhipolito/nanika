package sdk

import (
	"context"
	"errors"
	"path/filepath"
	"slices"
	"testing"
)

func TestPreventiveOneShotRejectedBeforeSpawn(t *testing.T) {
	opts := &AgentOptions{PermissionMode: PermissionPreventive, CLIPath: filepath.Join(t.TempDir(), "must-not-run"), ResumeSessionID: "saved"}
	if _, err := QueryText(context.Background(), "test", opts); !errors.Is(err, ErrPreventiveRequiresDuplex) {
		t.Fatalf("QueryText: %v", err)
	}
	tr := &SubprocessTransport{}
	if err := tr.Start(context.Background(), opts); !errors.Is(err, ErrPreventiveRequiresDuplex) {
		t.Fatalf("Start: %v", err)
	}
	if tr.cmd != nil {
		t.Fatal("transport constructed command for denied permission mode")
	}
}

func TestTransportPreservesPermissionsAndSharedOptions(t *testing.T) {
	for _, mode := range []string{"", "bypass"} {
		t.Run("mode="+mode, func(t *testing.T) {
			tr := &SubprocessTransport{}
			opts := &AgentOptions{CLIPath: filepath.Join(t.TempDir(), "missing"), PermissionMode: mode, EffortLevel: "high", AppendSystemPrompt: "append", SystemPrompt: "replace", DisableMCP: true, AddDirs: []string{"extra"}}
			if err := tr.Start(context.Background(), opts); err == nil {
				t.Fatal("missing executable unexpectedly started")
			}
			if tr.cmd == nil {
				t.Fatal("command not constructed")
			}
			args := tr.cmd.Args
			if slices.Contains(args, "--dangerously-skip-permissions") != (mode == "bypass") {
				t.Fatalf("permissions changed: %v", args)
			}
			assertFlag(t, args, "--effort", "high")
			assertFlag(t, args, "--append-system-prompt", "append")
			assertFlag(t, args, "--system-prompt", "")
			assertFlag(t, args, "--mcp-config", `{"mcpServers":{}}`)
			assertFlag(t, args, "--add-dir", "extra")
		})
	}
}

func TestNewQueryAlreadyCancelledDoesNotSpawn(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	q, err := NewQuery(ctx, &AgentOptions{CLIPath: filepath.Join(t.TempDir(), "must-not-run")})
	if q != nil || !errors.Is(err, context.Canceled) {
		t.Fatalf("query=%v error=%v", q, err)
	}
}
