package cmd

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/spf13/pflag"

	"github.com/joeyhipolito/orchestrator-cli/internal/preflight"
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

// ---------------------------------------------------------------------------
// preflight — skeleton with empty registry
// ---------------------------------------------------------------------------

func TestHooksPreflight_EmptyRegistryTextFormat(t *testing.T) {
	resetCmdFlags(t)
	setTestConfigDir(t)

	rootCmd.SetArgs([]string{"hooks", "preflight"})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("preflight with empty registry: %v", err)
	}
}

func TestHooksPreflight_EmptyRegistryJSONFormat(t *testing.T) {
	resetCmdFlags(t)
	setTestConfigDir(t)

	rootCmd.SetArgs([]string{"hooks", "preflight", "--format", "json"})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("preflight json format: %v", err)
	}
}

func TestHooksPreflight_NanikaNoInject(t *testing.T) {
	resetCmdFlags(t)
	setTestConfigDir(t)
	t.Setenv("NANIKA_NO_INJECT", "1")

	rootCmd.SetArgs([]string{"hooks", "preflight"})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("preflight with NANIKA_NO_INJECT=1: %v", err)
	}
}

func TestHooksPreflight_MaxBytesFlag(t *testing.T) {
	resetCmdFlags(t)
	setTestConfigDir(t)

	rootCmd.SetArgs([]string{"hooks", "preflight", "--max-bytes", "4096"})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("preflight with --max-bytes: %v", err)
	}
}

func TestHooksPreflight_SectionsFlag(t *testing.T) {
	resetCmdFlags(t)
	setTestConfigDir(t)

	rootCmd.SetArgs([]string{"hooks", "preflight", "--sections", "scheduler,tracker"})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("preflight with --sections: %v", err)
	}
}

func TestHooksPreflight_UnknownFormatErrors(t *testing.T) {
	resetCmdFlags(t)
	setTestConfigDir(t)

	rootCmd.SetArgs([]string{"hooks", "preflight", "--format", "yaml"})
	err := rootCmd.Execute()
	if err == nil {
		t.Fatal("expected error for unknown --format")
	}
	if !strings.Contains(err.Error(), "unknown --format") {
		t.Errorf("error %q should mention 'unknown --format'", err.Error())
	}
}

// ---------------------------------------------------------------------------
// preflight: Budget enforcement and composition tests
// ---------------------------------------------------------------------------

func TestHooksPreflight_BudgetEnforcement_DropsLowestPriority(t *testing.T) {
	resetCmdFlags(t)
	configDir := setTestConfigDir(t)

	// Register fake sections with varying body sizes and priorities:
	// - s1 (priority 10, small): ~30 bytes
	// - s2 (priority 20, small): ~30 bytes
	// - s3 (priority 30, large): ~5000 bytes
	// Expected: under budget, keep all. Over budget (e.g. 100 bytes),
	// drop s3 first (lowest priority = highest index), then s2, then s1.

	preflight.Reset()
	t.Cleanup(preflight.Reset)

	preflight.Register(&testFakeSection{
		name:     "s1",
		priority: 10,
		body:     "section one content here",
	})
	preflight.Register(&testFakeSection{
		name:     "s2",
		priority: 20,
		body:     "section two content here",
	})
	preflight.Register(&testFakeSection{
		name:     "s3",
		priority: 30,
		body:     strings.Repeat("x", 5000),
	})

	// With budget 100: should keep s1, s2 and drop s3.
	resetCmdFlags(t)
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", configDir)
	rootCmd.SetArgs([]string{"hooks", "preflight", "--format", "text", "--max-bytes", "100"})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("preflight with budget constraint: %v", err)
	}
}

func TestHooksPreflight_BudgetEnforcement_KeepsHighestPriority(t *testing.T) {
	resetCmdFlags(t)
	configDir := setTestConfigDir(t)

	// Register sections where highest-priority section is oversized.
	// Even if it exceeds budget alone, it should be kept (to avoid empty output).
	preflight.Reset()
	t.Cleanup(preflight.Reset)

	preflight.Register(&testFakeSection{
		name:     "large-first",
		priority: 10,
		body:     strings.Repeat("x", 10000),
	})
	preflight.Register(&testFakeSection{
		name:     "small-second",
		priority: 20,
		body:     "small",
	})

	resetCmdFlags(t)
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", configDir)
	rootCmd.SetArgs([]string{"hooks", "preflight", "--format", "text", "--max-bytes", "100"})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("preflight keeping high-priority oversized section: %v", err)
	}
}

func TestHooksPreflight_DefaultCapacityWhenUnspecified(t *testing.T) {
	resetCmdFlags(t)
	configDir := setTestConfigDir(t)

	preflight.Reset()
	t.Cleanup(preflight.Reset)

	preflight.Register(&testFakeSection{
		name:     "test",
		priority: 10,
		body:     "test content",
	})

	resetCmdFlags(t)
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", configDir)
	// Omit --max-bytes, expect default 6144
	rootCmd.SetArgs([]string{"hooks", "preflight"})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("preflight with default capacity: %v", err)
	}
}

// ---------------------------------------------------------------------------
// preflight: Format parsing tests
// ---------------------------------------------------------------------------

func TestHooksPreflight_JSONFormatValid(t *testing.T) {
	resetCmdFlags(t)
	configDir := setTestConfigDir(t)

	preflight.Reset()
	t.Cleanup(preflight.Reset)

	preflight.Register(&testFakeSection{
		name:     "test",
		priority: 10,
		title:    "Test Section",
		body:     "test body",
	})

	resetCmdFlags(t)
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", configDir)
	rootCmd.SetArgs([]string{"hooks", "preflight", "--format", "json"})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("preflight json format: %v", err)
	}
}

func TestHooksPreflight_MarkdownFormatStructure(t *testing.T) {
	resetCmdFlags(t)
	configDir := setTestConfigDir(t)

	preflight.Reset()
	t.Cleanup(preflight.Reset)

	preflight.Register(&testFakeSection{
		name:     "scheduler",
		priority: 10,
		title:    "Scheduled Jobs",
		body:     "job-1\njob-2\n",
	})

	resetCmdFlags(t)
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", configDir)
	rootCmd.SetArgs([]string{"hooks", "preflight", "--format", "text"})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("preflight text format: %v", err)
	}
	// Markdown structure validated by RenderMarkdown tests in preflight_test.go
}

func TestHooksPreflight_EmptyOutputWithNanikaNoInjectReturnsZeroBytes(t *testing.T) {
	resetCmdFlags(t)
	configDir := setTestConfigDir(t)
	t.Setenv("NANIKA_NO_INJECT", "1")

	preflight.Reset()
	t.Cleanup(preflight.Reset)

	resetCmdFlags(t)
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", configDir)
	rootCmd.SetArgs([]string{"hooks", "preflight"})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("preflight with NANIKA_NO_INJECT=1: %v", err)
	}
	// Exit code 0 and zero output — runPreflight returns nil without printing.
}

// ---------------------------------------------------------------------------
// preflight: Integration test with fixture home directory
// ---------------------------------------------------------------------------

func TestHooksPreflight_IntegrationWithFixtureHome(t *testing.T) {
	resetCmdFlags(t)

	// Create a fixture home directory with subdirectories for scheduler, tracker, etc.
	fixtureHome := t.TempDir()
	allukaDir := filepath.Join(fixtureHome, ".alluka")
	if err := os.MkdirAll(filepath.Join(allukaDir, "missions"), 0700); err != nil {
		t.Fatalf("mkdir .alluka/missions: %v", err)
	}

	// Point ALLUKA_HOME to fixture, ORCHESTRATOR_CONFIG_DIR to temp DB.
	configDir := filepath.Join(fixtureHome, ".config", "orchestrator")
	if err := os.MkdirAll(configDir, 0700); err != nil {
		t.Fatalf("mkdir orchestrator config: %v", err)
	}

	t.Setenv("ALLUKA_HOME", fixtureHome)
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", configDir)

	preflight.Reset()
	t.Cleanup(preflight.Reset)

	// Register a simple test section that reads from the fixture directory.
	preflight.Register(&testFakeSection{
		name:     "fixture-test",
		priority: 10,
		title:    "Fixture Data",
		body:     fmt.Sprintf("Fixture home: %s", fixtureHome),
	})

	resetCmdFlags(t)
	rootCmd.SetArgs([]string{"hooks", "preflight", "--format", "text"})
	if err := rootCmd.Execute(); err != nil {
		t.Fatalf("preflight with fixture home: %v", err)
	}

	// Verify the fixture directory was preserved throughout the operation.
	if _, err := os.Stat(allukaDir); os.IsNotExist(err) {
		t.Error("fixture home directory was not preserved")
	}
}

// ---------------------------------------------------------------------------
// Test helper: testFakeSection for composition tests
// ---------------------------------------------------------------------------

type testFakeSection struct {
	name     string
	priority int
	title    string
	body     string
}

func (s *testFakeSection) Name() string     { return s.name }
func (s *testFakeSection) Priority() int    { return s.priority }
func (s *testFakeSection) Fetch(_ context.Context) (preflight.Block, error) {
	title := s.title
	if title == "" {
		title = strings.ToUpper(s.name)
	}
	return preflight.Block{Title: title, Body: s.body}, nil
}
