package cmd

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/spf13/pflag"
)

// setTestConfigDir redirects the orchestrator config dir to a temp dir so
// hooks commands open an isolated learning DB instead of the real one.
func setTestConfigDir(t *testing.T) string {
	t.Helper()
	tmpDir := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", tmpDir)
	return tmpDir
}

// resetCmdFlags resets the Changed state and restores default values for all
// flags in the command tree. Required between Execute() calls in tests because
// cobra/pflag accumulates flag state across calls to the same rootCmd instance.
func resetCmdFlags(t *testing.T) {
	t.Helper()
	walk := func(flags *pflag.FlagSet) {
		flags.VisitAll(func(f *pflag.Flag) {
			f.Changed = false
			// Best-effort restore — ignore errors (e.g. bool flags reject non-bool defaults).
			_ = f.Value.Set(f.DefValue)
		})
	}
	walk(rootCmd.PersistentFlags())
	walk(rootCmd.Flags())
	for _, sub := range rootCmd.Commands() {
		walk(sub.Flags())
		walk(sub.PersistentFlags())
		for _, sub2 := range sub.Commands() {
			walk(sub2.Flags())
			walk(sub2.PersistentFlags())
		}
	}
}

// ---------------------------------------------------------------------------
// flush-context — required-flag tests first (before any test sets flags)
// ---------------------------------------------------------------------------

func TestHooksFlushContext_MissingRequiredFlags(t *testing.T) {
	resetCmdFlags(t)
	setTestConfigDir(t)

	rootCmd.SetArgs([]string{"hooks", "flush-context"})
	if err := rootCmd.Execute(); err == nil {
		t.Error("expected error when required flags are missing, got nil")
	}
}

func TestHooksFlushContext_MissingOutputFlag(t *testing.T) {
	resetCmdFlags(t)
	setTestConfigDir(t)

	rootCmd.SetArgs([]string{"hooks", "flush-context", "--query", "some query"})
	if err := rootCmd.Execute(); err == nil {
		t.Error("expected error when --output flag is missing, got nil")
	}
}

func TestHooksFlushContext_WritesFileForEmptyDB(t *testing.T) {
	resetCmdFlags(t)
	configDir := setTestConfigDir(t)
	outPath := filepath.Join(configDir, "ctx.md")

	rootCmd.SetArgs([]string{"hooks", "flush-context", "--query", "goroutine leaks", "--output", outPath})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("flush-context: %v", err)
	}

	if _, err := os.Stat(outPath); err != nil {
		t.Errorf("output file not created: %v", err)
	}
}

func TestHooksFlushContext_CreatesNestedOutputDir(t *testing.T) {
	resetCmdFlags(t)
	configDir := setTestConfigDir(t)
	outPath := filepath.Join(configDir, "nested", "ctx.md")

	rootCmd.SetArgs([]string{"hooks", "flush-context", "--query", "some query", "--output", outPath})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("flush-context: %v", err)
	}

	if _, err := os.Stat(outPath); err != nil {
		t.Errorf("nested output file not created: %v", err)
	}
}

// ---------------------------------------------------------------------------
// inject-context
// ---------------------------------------------------------------------------

func TestHooksInjectContext_EmptyDB(t *testing.T) {
	resetCmdFlags(t)
	setTestConfigDir(t)

	rootCmd.SetArgs([]string{"hooks", "inject-context", "--query", "goroutine leaks"})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("inject-context with empty DB: %v", err)
	}
}

func TestHooksInjectContext_EmptyDB_NoQuery(t *testing.T) {
	resetCmdFlags(t)
	setTestConfigDir(t)

	// Cold-start: --query is optional; empty DB should produce no output but no error.
	rootCmd.SetArgs([]string{"hooks", "inject-context"})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("inject-context cold-start with empty DB: %v", err)
	}
}

func TestHooksInjectContext_CustomLimit(t *testing.T) {
	resetCmdFlags(t)
	setTestConfigDir(t)

	rootCmd.SetArgs([]string{"hooks", "inject-context", "--query", "test", "--limit", "5"})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("inject-context with custom limit: %v", err)
	}
}

func TestHooksInjectContext_NanikaNoInject(t *testing.T) {
	resetCmdFlags(t)
	setTestConfigDir(t)
	t.Setenv("NANIKA_NO_INJECT", "1")

	rootCmd.SetArgs([]string{"hooks", "inject-context", "--query", "goroutine leaks"})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("inject-context with NANIKA_NO_INJECT=1: %v", err)
	}
}

func TestHooksInjectContext_MaxBytes(t *testing.T) {
	resetCmdFlags(t)
	setTestConfigDir(t)

	rootCmd.SetArgs([]string{"hooks", "inject-context", "--max-bytes", "4096"})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("inject-context with --max-bytes: %v", err)
	}
}

// ---------------------------------------------------------------------------
// snapshot-session
// ---------------------------------------------------------------------------

func TestHooksSnapshotSession_WorkspaceNotFound(t *testing.T) {
	resetCmdFlags(t)
	setTestConfigDir(t)

	rootCmd.SetArgs([]string{"hooks", "snapshot-session", "--workspace", "definitely-nonexistent-ws-xyz"})
	err := rootCmd.Execute()
	if err == nil {
		t.Error("expected error for nonexistent workspace, got nil")
	}
	if !strings.Contains(err.Error(), "not found") {
		t.Errorf("error %q should mention 'not found'", err.Error())
	}
}

func TestHooksSnapshotSession_ExplicitEmptyWorkspace(t *testing.T) {
	resetCmdFlags(t)
	setTestConfigDir(t)

	wsPath := t.TempDir()
	rootCmd.SetArgs([]string{"hooks", "snapshot-session", "--workspace", wsPath})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("snapshot-session with empty workspace: %v", err)
	}
}

func TestHooksSnapshotSession_WorkspaceWithEmptyWorkers(t *testing.T) {
	resetCmdFlags(t)
	setTestConfigDir(t)

	wsPath := t.TempDir()
	if err := os.MkdirAll(filepath.Join(wsPath, "workers"), 0700); err != nil {
		t.Fatalf("mkdir workers: %v", err)
	}

	rootCmd.SetArgs([]string{"hooks", "snapshot-session", "--workspace", wsPath})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("snapshot-session with empty workers dir: %v", err)
	}
}

func TestHooksSnapshotSession_WorkspaceWithValidOutputMd(t *testing.T) {
	resetCmdFlags(t)
	setTestConfigDir(t)

	wsPath := t.TempDir()
	workerDir := filepath.Join(wsPath, "workers", "worker-1")
	if err := os.MkdirAll(workerDir, 0700); err != nil {
		t.Fatalf("mkdir worker: %v", err)
	}
	if err := os.WriteFile(
		filepath.Join(workerDir, "output.md"),
		[]byte("LEARNING: Always use context cancellation to avoid goroutine leaks.\n"),
		0600,
	); err != nil {
		t.Fatalf("write output.md: %v", err)
	}

	rootCmd.SetArgs([]string{"hooks", "snapshot-session", "--workspace", wsPath})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("snapshot-session with valid output.md: %v", err)
	}
}

func TestHooksSnapshotSession_WorkspaceWithNoValidMarkers(t *testing.T) {
	resetCmdFlags(t)
	setTestConfigDir(t)

	wsPath := t.TempDir()
	workerDir := filepath.Join(wsPath, "workers", "worker-1")
	if err := os.MkdirAll(workerDir, 0700); err != nil {
		t.Fatalf("mkdir worker: %v", err)
	}
	if err := os.WriteFile(
		filepath.Join(workerDir, "output.md"),
		[]byte("no markers here, just plain output text with nothing special.\n"),
		0600,
	); err != nil {
		t.Fatalf("write output.md: %v", err)
	}

	rootCmd.SetArgs([]string{"hooks", "snapshot-session", "--workspace", wsPath})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("snapshot-session with no valid markers: %v", err)
	}
}

func TestHooksSnapshotSession_WorkerWithMissingOutputMd(t *testing.T) {
	resetCmdFlags(t)
	setTestConfigDir(t)

	wsPath := t.TempDir()
	if err := os.MkdirAll(filepath.Join(wsPath, "workers", "worker-1"), 0700); err != nil {
		t.Fatalf("mkdir worker: %v", err)
	}

	rootCmd.SetArgs([]string{"hooks", "snapshot-session", "--workspace", wsPath})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("snapshot-session should not error when output.md is missing: %v", err)
	}
}
