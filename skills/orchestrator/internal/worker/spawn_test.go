package worker

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
)

// TestSpawn_TargetDirWiredToConfig verifies that when a phase has TargetDir set,
// Spawn wires it into both WorkerConfig.TargetDir and the returned bundle,
// and that CLAUDE.md references the target repo rather than "current directory".
func TestSpawn_TargetDirWiredToConfig(t *testing.T) {
	wsPath := t.TempDir()
	for _, sub := range []string{"workers", "learnings"} {
		if err := os.MkdirAll(filepath.Join(wsPath, sub), 0700); err != nil {
			t.Fatalf("setup: %v", err)
		}
	}

	targetDir := t.TempDir()

	phase := &core.Phase{
		ID:        "phase-1",
		Name:      "Test Phase",
		Persona:   "senior-backend-engineer",
		ModelTier: "work",
		TargetDir: targetDir,
	}
	bundle := core.ContextBundle{
		Objective:   "do the task",
		Domain:      "dev",
		WorkspaceID: "ws-test",
		PhaseID:     "phase-1",
		TargetDir:   targetDir,
	}

	config, err := Spawn(wsPath, phase, bundle)
	if err != nil {
		t.Fatalf("Spawn: %v", err)
	}

	// WorkerConfig.TargetDir must equal what was on the phase/bundle.
	if config.TargetDir != targetDir {
		t.Errorf("config.TargetDir = %q; want %q", config.TargetDir, targetDir)
	}
	if config.EffortLevel != "medium" {
		t.Errorf("config.EffortLevel = %q; want medium for senior-backend-engineer work", config.EffortLevel)
	}

	// WorkerDir must be populated — it is where artifacts are written.
	if config.WorkerDir == "" {
		t.Error("config.WorkerDir must not be empty")
	}

	// Bundle.WorkerDir must match the returned config.WorkerDir so CLAUDE.md
	// references the correct artifact output path.
	if config.Bundle.WorkerDir != config.WorkerDir {
		t.Errorf("config.Bundle.WorkerDir = %q; want same as config.WorkerDir = %q",
			config.Bundle.WorkerDir, config.WorkerDir)
	}

	// Bundle.TargetDir must be preserved.
	if config.Bundle.TargetDir != targetDir {
		t.Errorf("config.Bundle.TargetDir = %q; want %q", config.Bundle.TargetDir, targetDir)
	}

	// CLAUDE.md must reference the target dir and not say "current directory".
	claudemd, err := os.ReadFile(filepath.Join(config.WorkerDir, "CLAUDE.md"))
	if err != nil {
		t.Fatalf("CLAUDE.md not written: %v", err)
	}
	md := string(claudemd)
	if !strings.Contains(md, targetDir) {
		t.Errorf("CLAUDE.md should contain TargetDir %q", targetDir)
	}
	if strings.Contains(md, "current directory") {
		t.Error("CLAUDE.md must not say 'current directory' when TargetDir is set")
	}
}

// TestSpawn_NoTargetDir_LegacyBehavior verifies that when TargetDir is absent,
// WorkerConfig.TargetDir stays empty and CLAUDE.md says "current directory".
func TestSpawn_NoTargetDir_LegacyBehavior(t *testing.T) {
	wsPath := t.TempDir()
	for _, sub := range []string{"workers", "learnings"} {
		if err := os.MkdirAll(filepath.Join(wsPath, sub), 0700); err != nil {
			t.Fatalf("setup: %v", err)
		}
	}

	phase := &core.Phase{
		ID:        "phase-2",
		Name:      "Legacy Phase",
		Persona:   "senior-backend-engineer",
		ModelTier: "work",
		// TargetDir intentionally empty
	}
	bundle := core.ContextBundle{
		Objective:   "do the task",
		Domain:      "dev",
		WorkspaceID: "ws-test",
		PhaseID:     "phase-2",
		// TargetDir intentionally empty
	}

	config, err := Spawn(wsPath, phase, bundle)
	if err != nil {
		t.Fatalf("Spawn: %v", err)
	}

	if config.TargetDir != "" {
		t.Errorf("config.TargetDir = %q; want empty when no target set", config.TargetDir)
	}
	if config.EffortLevel != "medium" {
		t.Errorf("config.EffortLevel = %q; want medium for senior-backend-engineer work", config.EffortLevel)
	}
	if config.WorkerDir == "" {
		t.Error("config.WorkerDir must not be empty")
	}

	claudemd, err := os.ReadFile(filepath.Join(config.WorkerDir, "CLAUDE.md"))
	if err != nil {
		t.Fatalf("CLAUDE.md not written: %v", err)
	}
	if !strings.Contains(string(claudemd), "current directory") {
		t.Error("CLAUDE.md should say 'current directory' when TargetDir is not set")
	}
}

// TestSpawn_WorkerDirIsInsideWorkspace verifies that the worker directory
// created by Spawn is a subdirectory of the workspace, regardless of TargetDir.
func TestSpawn_WorkerDirIsInsideWorkspace(t *testing.T) {
	wsPath := t.TempDir()
	for _, sub := range []string{"workers", "learnings"} {
		if err := os.MkdirAll(filepath.Join(wsPath, sub), 0700); err != nil {
			t.Fatalf("setup: %v", err)
		}
	}

	targetDir := t.TempDir()

	tests := []struct {
		name      string
		targetDir string
	}{
		{"with TargetDir", targetDir},
		{"without TargetDir", ""},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			phase := &core.Phase{
				ID:        "phase-3",
				Name:      "Test",
				Persona:   "senior-backend-engineer",
				ModelTier: "work",
				TargetDir: tt.targetDir,
			}
			bundle := core.ContextBundle{
				Objective:   "task",
				Domain:      "dev",
				WorkspaceID: "ws-test",
				PhaseID:     "phase-3",
				TargetDir:   tt.targetDir,
			}

			config, err := Spawn(wsPath, phase, bundle)
			if err != nil {
				t.Fatalf("Spawn: %v", err)
			}

			// WorkerDir must always live inside the workspace — never in TargetDir.
			if !strings.HasPrefix(config.WorkerDir, wsPath) {
				t.Errorf("WorkerDir %q is not inside workspace %q", config.WorkerDir, wsPath)
			}

			// TargetDir (the CWD for execution) is separate from WorkerDir.
			if tt.targetDir != "" && config.WorkerDir == tt.targetDir {
				t.Errorf("WorkerDir must not equal TargetDir: both are %q", config.WorkerDir)
			}
		})
	}
}
