package cmd

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

// setupTestWorkspace creates a minimal workspace with a checkpoint for testing
// the cancel command. Returns the workspace path and cleanup function.
func setupTestWorkspace(t *testing.T, status string, phases []*core.Phase) string {
	t.Helper()

	home, err := os.UserHomeDir()
	if err != nil {
		t.Fatalf("UserHomeDir: %v", err)
	}
	wsBase := filepath.Join(home, ".alluka", "workspaces")
	if err := os.MkdirAll(wsBase, 0700); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	wsID := "test-cancel-" + time.Now().Format("150405")
	wsPath := filepath.Join(wsBase, wsID)
	if err := os.MkdirAll(wsPath, 0700); err != nil {
		t.Fatalf("mkdir ws: %v", err)
	}
	t.Cleanup(func() { os.RemoveAll(wsPath) })

	// Write mission.md
	if err := os.WriteFile(filepath.Join(wsPath, "mission.md"), []byte("test mission"), 0600); err != nil {
		t.Fatalf("write mission: %v", err)
	}

	// Save checkpoint
	plan := &core.Plan{
		ID:            "test-plan",
		Task:          "test mission",
		Phases:        phases,
		ExecutionMode: "sequential",
	}
	if err := core.SaveCheckpoint(wsPath, plan, "dev", status, time.Now()); err != nil {
		t.Fatalf("save checkpoint: %v", err)
	}

	return wsPath
}

func TestManualCancel_SkipsPendingPhases(t *testing.T) {
	phases := []*core.Phase{
		{ID: "p1", Name: "done", Status: core.StatusCompleted},
		{ID: "p2", Name: "running", Status: core.StatusRunning},
		{ID: "p3", Name: "pending", Status: core.StatusPending},
	}
	wsPath := setupTestWorkspace(t, "in_progress", phases)
	wsID := filepath.Base(wsPath)

	// Load checkpoint for manualCancel
	cp, err := core.LoadCheckpoint(wsPath)
	if err != nil {
		t.Fatalf("load checkpoint: %v", err)
	}

	// Clean up event log created by manualCancel
	logPath, _ := event.EventLogPath(wsID)
	t.Cleanup(func() { os.Remove(logPath) })

	if err := manualCancel(wsPath, wsID, cp); err != nil {
		t.Fatalf("manualCancel: %v", err)
	}

	// Reload checkpoint and verify
	cp2, err := core.LoadCheckpoint(wsPath)
	if err != nil {
		t.Fatalf("reload checkpoint: %v", err)
	}

	if cp2.Status != "cancelled" {
		t.Errorf("status = %q; want %q", cp2.Status, "cancelled")
	}

	// Phase 1 should still be completed (terminal, not touched)
	if cp2.Plan.Phases[0].Status != core.StatusCompleted {
		t.Errorf("phase[0].Status = %q; want completed", cp2.Plan.Phases[0].Status)
	}

	// Phase 2 (was running) should be skipped
	if cp2.Plan.Phases[1].Status != core.StatusSkipped {
		t.Errorf("phase[1].Status = %q; want skipped", cp2.Plan.Phases[1].Status)
	}

	// Phase 3 (was pending) should be skipped
	if cp2.Plan.Phases[2].Status != core.StatusSkipped {
		t.Errorf("phase[2].Status = %q; want skipped", cp2.Plan.Phases[2].Status)
	}

	// Cancel sentinel should exist
	if !core.HasCancelSentinel(wsPath) {
		t.Error("cancel sentinel should exist after manual cancel")
	}
}

func TestManualCancel_EmitsEvents(t *testing.T) {
	phases := []*core.Phase{
		{ID: "p1", Name: "pending", Status: core.StatusPending},
	}
	wsPath := setupTestWorkspace(t, "in_progress", phases)
	wsID := filepath.Base(wsPath)

	// Create events directory so the emitter can write
	eventsDir, err := event.EventLogsDir()
	if err != nil {
		t.Fatalf("events dir: %v", err)
	}
	if err := os.MkdirAll(eventsDir, 0700); err != nil {
		t.Fatalf("mkdir events: %v", err)
	}

	cp, err := core.LoadCheckpoint(wsPath)
	if err != nil {
		t.Fatalf("load checkpoint: %v", err)
	}

	if err := manualCancel(wsPath, wsID, cp); err != nil {
		t.Fatalf("manualCancel: %v", err)
	}

	// Read event log and verify mission.cancelled was emitted
	logPath, _ := event.EventLogPath(wsID)
	data, err := os.ReadFile(logPath)
	if err != nil {
		t.Fatalf("read event log: %v", err)
	}
	t.Cleanup(func() { os.Remove(logPath) })

	content := string(data)
	if content == "" {
		t.Fatal("event log is empty")
	}

	// Parse events
	var foundPhaseSkipped, foundMissionCancelled bool
	for _, line := range strings.Split(content, "\n") {
		if line == "" {
			continue
		}
		var ev event.Event
		if err := json.Unmarshal([]byte(line), &ev); err != nil {
			continue
		}
		switch ev.Type {
		case event.PhaseSkipped:
			foundPhaseSkipped = true
		case event.MissionCancelled:
			foundMissionCancelled = true
			if source, ok := ev.Data["source"].(string); !ok || source != "orchestrator cancel" {
				t.Errorf("mission.cancelled source = %v; want %q", ev.Data["source"], "orchestrator cancel")
			}
		}
	}

	if !foundPhaseSkipped {
		t.Error("expected phase.skipped event in log")
	}
	if !foundMissionCancelled {
		t.Error("expected mission.cancelled event in log")
	}
}

func TestRunCancel_AlreadyCompleted(t *testing.T) {
	phases := []*core.Phase{
		{ID: "p1", Name: "done", Status: core.StatusCompleted},
	}
	wsPath := setupTestWorkspace(t, "completed", phases)
	wsID := filepath.Base(wsPath)

	// runCancel should return nil without modifying anything
	cmd := rootCmd
	cmd.SetArgs([]string{"cancel", wsID})
	err := cmd.Execute()
	if err != nil {
		t.Errorf("cancel on completed mission should not error: %v", err)
	}

	// Verify checkpoint was not modified
	cp, _ := core.LoadCheckpoint(wsPath)
	if cp.Status != "completed" {
		t.Errorf("status should remain completed, got %q", cp.Status)
	}
}

func TestRunCancel_NoPIDFile(t *testing.T) {
	phases := []*core.Phase{
		{ID: "p1", Name: "pending", Status: core.StatusPending},
	}
	wsPath := setupTestWorkspace(t, "in_progress", phases)
	wsID := filepath.Base(wsPath)

	// No PID file — should fall through to manual cleanup
	cmd := rootCmd
	cmd.SetArgs([]string{"cancel", wsID})
	err := cmd.Execute()
	if err != nil {
		t.Errorf("cancel with no PID should not error: %v", err)
	}

	// Verify checkpoint was updated
	cp, _ := core.LoadCheckpoint(wsPath)
	if cp.Status != "cancelled" {
		t.Errorf("status = %q; want cancelled", cp.Status)
	}

	// Clean up event log
	logPath, _ := event.EventLogPath(wsID)
	os.Remove(logPath)
}

