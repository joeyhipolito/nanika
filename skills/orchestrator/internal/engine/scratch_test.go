package engine

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
)

// ---------------------------------------------------------------------------
// ExtractScratchBlock: parses <!-- scratch --> blocks from output
// ---------------------------------------------------------------------------

func TestExtractScratchBlock_SingleBlock(t *testing.T) {
	output := `Some worker output here.

<!-- scratch -->
Design decision: use map[string]string for scratch notes.
Gotcha: phase IDs may contain hyphens.
<!-- /scratch -->

More output after.`

	got := ExtractScratchBlock(output)
	if !strings.Contains(got, "Design decision") {
		t.Errorf("expected design decision note, got %q", got)
	}
	if !strings.Contains(got, "Gotcha: phase IDs") {
		t.Errorf("expected gotcha note, got %q", got)
	}
}

func TestExtractScratchBlock_MultipleBlocks(t *testing.T) {
	output := `First section.

<!-- scratch -->
Note one.
<!-- /scratch -->

Middle section.

<!-- scratch -->
Note two.
<!-- /scratch -->

End.`

	got := ExtractScratchBlock(output)
	if !strings.Contains(got, "Note one") {
		t.Error("missing first scratch block")
	}
	if !strings.Contains(got, "Note two") {
		t.Error("missing second scratch block")
	}
}

func TestExtractScratchBlock_NoBlocks(t *testing.T) {
	output := "Just regular output with no scratch markers."
	got := ExtractScratchBlock(output)
	if got != "" {
		t.Errorf("expected empty string for no blocks, got %q", got)
	}
}

func TestExtractScratchBlock_EmptyBlock(t *testing.T) {
	output := "<!-- scratch -->\n\n<!-- /scratch -->"
	got := ExtractScratchBlock(output)
	if got != "" {
		t.Errorf("expected empty string for empty block, got %q", got)
	}
}

func TestExtractScratchBlock_WhitespaceVariants(t *testing.T) {
	output := "<!--scratch-->\nContent here.\n<!--/scratch-->"
	got := ExtractScratchBlock(output)
	if !strings.Contains(got, "Content here") {
		t.Errorf("expected content with tight markers, got %q", got)
	}
}

// ---------------------------------------------------------------------------
// truncateScratch: caps content at a byte limit
// ---------------------------------------------------------------------------

func Test_truncateScratch_UnderLimit(t *testing.T) {
	content := "Short note."
	got := truncateScratch(content, maxScratchBytes)
	if got != content {
		t.Errorf("expected unchanged content, got %q", got)
	}
}

func Test_truncateScratch_OverLimit(t *testing.T) {
	content := strings.Repeat("x", 5000)
	got := truncateScratch(content, maxScratchBytes)
	if !strings.Contains(got, "[truncated") {
		t.Error("expected truncation marker")
	}
	// Content before truncation marker should be exactly maxScratchBytes
	if !strings.HasPrefix(got, strings.Repeat("x", maxScratchBytes)) {
		t.Error("truncated content should preserve first maxScratchBytes bytes")
	}
}

func Test_truncateScratch_ExactLimit(t *testing.T) {
	content := strings.Repeat("y", maxScratchBytes)
	got := truncateScratch(content, maxScratchBytes)
	if got != content {
		t.Error("content at exact limit should not be truncated")
	}
}

// ---------------------------------------------------------------------------
// extractScratch + collectPriorScratch: filesystem integration
// ---------------------------------------------------------------------------

func TestExtractScratch_WritesToDisk(t *testing.T) {
	tmpDir := t.TempDir()

	e := &Engine{
		workspace: &core.Workspace{Path: tmpDir},
		config:    &core.OrchestratorConfig{Verbose: true},
		phases:    make(map[string]*core.Phase),
	}

	phase := &core.Phase{ID: "phase-1", Name: "implement"}
	output := "Result.\n<!-- scratch -->\nKey insight: use buffered channels.\n<!-- /scratch -->\nDone."

	e.extractScratch(phase, output)

	scratchPath := filepath.Join(core.ScratchDir(tmpDir, "phase-1"), "notes.md")
	data, err := os.ReadFile(scratchPath)
	if err != nil {
		t.Fatalf("expected scratch file at %s: %v", scratchPath, err)
	}
	if !strings.Contains(string(data), "buffered channels") {
		t.Errorf("scratch file missing expected content, got %q", string(data))
	}
}

func TestExtractScratch_NoBlocksNoFile(t *testing.T) {
	tmpDir := t.TempDir()

	e := &Engine{
		workspace: &core.Workspace{Path: tmpDir},
		config:    &core.OrchestratorConfig{},
		phases:    make(map[string]*core.Phase),
	}

	phase := &core.Phase{ID: "phase-1", Name: "research"}
	e.extractScratch(phase, "Just plain output.")

	scratchDir := core.ScratchDir(tmpDir, "phase-1")
	if _, err := os.Stat(scratchDir); !os.IsNotExist(err) {
		t.Error("scratch dir should not be created when no blocks are present")
	}
}

func TestCollectPriorScratch_ReadsDependencyNotes(t *testing.T) {
	tmpDir := t.TempDir()

	// Create scratch notes for dependency phase
	depScratchDir := core.ScratchDir(tmpDir, "phase-1")
	if err := os.MkdirAll(depScratchDir, 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(depScratchDir, "notes.md"), []byte("Use optimistic locking."), 0600); err != nil {
		t.Fatal(err)
	}

	depPhase := &core.Phase{
		ID:     "phase-1",
		Name:   "design",
		Status: core.StatusCompleted,
	}
	currentPhase := &core.Phase{
		ID:           "phase-2",
		Name:         "implement",
		Dependencies: []string{"phase-1"},
	}

	e := &Engine{
		workspace: &core.Workspace{Path: tmpDir},
		config:    &core.OrchestratorConfig{},
		phases: map[string]*core.Phase{
			"phase-1": depPhase,
			"phase-2": currentPhase,
		},
	}

	scratch := e.collectPriorScratch(currentPhase)
	if scratch == nil {
		t.Fatal("expected non-nil scratch map")
	}
	if scratch["design"] != "Use optimistic locking." {
		t.Errorf("expected scratch note from design phase, got %q", scratch["design"])
	}
}

func TestCollectPriorScratch_NilWhenNoDependencies(t *testing.T) {
	e := &Engine{
		workspace: &core.Workspace{Path: t.TempDir()},
		config:    &core.OrchestratorConfig{},
		phases:    make(map[string]*core.Phase),
	}
	phase := &core.Phase{ID: "phase-1", Name: "standalone"}
	if scratch := e.collectPriorScratch(phase); scratch != nil {
		t.Errorf("expected nil, got %v", scratch)
	}
}

func TestCollectPriorScratch_SkipsFailedDeps(t *testing.T) {
	tmpDir := t.TempDir()

	// Create scratch notes for a failed dependency
	depScratchDir := core.ScratchDir(tmpDir, "phase-1")
	if err := os.MkdirAll(depScratchDir, 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(depScratchDir, "notes.md"), []byte("Should not appear."), 0600); err != nil {
		t.Fatal(err)
	}

	depPhase := &core.Phase{
		ID:     "phase-1",
		Name:   "design",
		Status: core.StatusFailed,
	}
	currentPhase := &core.Phase{
		ID:           "phase-2",
		Name:         "implement",
		Dependencies: []string{"phase-1"},
	}

	e := &Engine{
		workspace: &core.Workspace{Path: tmpDir},
		config:    &core.OrchestratorConfig{},
		phases: map[string]*core.Phase{
			"phase-1": depPhase,
			"phase-2": currentPhase,
		},
	}

	if scratch := e.collectPriorScratch(currentPhase); scratch != nil {
		t.Errorf("expected nil (failed dep), got %v", scratch)
	}
}

func TestCollectPriorScratch_MultipleDeps(t *testing.T) {
	tmpDir := t.TempDir()

	// Dep 1: has scratch
	dep1Dir := core.ScratchDir(tmpDir, "phase-1")
	os.MkdirAll(dep1Dir, 0700)
	os.WriteFile(filepath.Join(dep1Dir, "notes.md"), []byte("Schema: users table."), 0600)

	// Dep 2: has scratch
	dep2Dir := core.ScratchDir(tmpDir, "phase-2")
	os.MkdirAll(dep2Dir, 0700)
	os.WriteFile(filepath.Join(dep2Dir, "notes.md"), []byte("API: REST endpoints."), 0600)

	// Dep 3: no scratch
	// (no directory created)

	phases := map[string]*core.Phase{
		"phase-1": {ID: "phase-1", Name: "schema", Status: core.StatusCompleted},
		"phase-2": {ID: "phase-2", Name: "api", Status: core.StatusCompleted},
		"phase-3": {ID: "phase-3", Name: "docs", Status: core.StatusCompleted},
		"phase-4": {
			ID: "phase-4", Name: "integrate",
			Dependencies: []string{"phase-1", "phase-2", "phase-3"},
		},
	}

	e := &Engine{
		workspace: &core.Workspace{Path: tmpDir},
		config:    &core.OrchestratorConfig{},
		phases:    phases,
	}

	scratch := e.collectPriorScratch(phases["phase-4"])
	if scratch == nil {
		t.Fatal("expected non-nil scratch")
	}
	if len(scratch) != 2 {
		t.Errorf("expected 2 entries, got %d", len(scratch))
	}
	if scratch["schema"] != "Schema: users table." {
		t.Errorf("unexpected schema scratch: %q", scratch["schema"])
	}
	if scratch["api"] != "API: REST endpoints." {
		t.Errorf("unexpected api scratch: %q", scratch["api"])
	}
}
