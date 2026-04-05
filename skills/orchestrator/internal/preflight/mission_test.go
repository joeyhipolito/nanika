package preflight

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
)

// writeFakeCheckpoint writes a minimal checkpoint.json envelope to wsDir.
func writeFakeCheckpoint(t *testing.T, wsDir string, cp core.Checkpoint) {
	t.Helper()
	type envelope struct {
		Version int            `json:"version"`
		Payload core.Checkpoint `json:"payload"`
	}
	data, err := json.Marshal(envelope{Version: 1, Payload: cp})
	if err != nil {
		t.Fatalf("marshal checkpoint: %v", err)
	}
	if err := os.WriteFile(filepath.Join(wsDir, "checkpoint.json"), data, 0600); err != nil {
		t.Fatalf("write checkpoint: %v", err)
	}
}

// writeFakeEventLog writes a JSONL event log with the given timestamps.
func writeFakeEventLog(t *testing.T, eventsDir, missionID string, timestamps []time.Time) {
	t.Helper()
	if err := os.MkdirAll(eventsDir, 0700); err != nil {
		t.Fatalf("mkdir events: %v", err)
	}
	f, err := os.Create(filepath.Join(eventsDir, missionID+".jsonl"))
	if err != nil {
		t.Fatalf("create event log: %v", err)
	}
	defer f.Close()
	for _, ts := range timestamps {
		line, _ := json.Marshal(map[string]any{"timestamp": ts.UTC().Format(time.RFC3339Nano)})
		f.Write(line)
		f.Write([]byte{'\n'})
	}
}

func TestMissionSection_Metadata(t *testing.T) {
	s := &missionSection{}
	if got := s.Name(); got != "mission" {
		t.Errorf("Name() = %q, want %q", got, "mission")
	}
	if got := s.Priority(); got != 5 {
		t.Errorf("Priority() = %d, want 5", got)
	}
}

func TestMissionSection_NoWorkspacesDir(t *testing.T) {
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", filepath.Join(t.TempDir(), "nonexistent"))
	s := &missionSection{}
	blk, err := s.Fetch(context.Background())
	if err != nil {
		t.Fatalf("Fetch returned error: %v", err)
	}
	if blk.Body != "" {
		t.Errorf("expected empty body for missing workspaces dir, got %q", blk.Body)
	}
}

func TestMissionSection_EmptyWorkspacesDir(t *testing.T) {
	base := t.TempDir()
	if err := os.MkdirAll(filepath.Join(base, "workspaces"), 0700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", base)

	s := &missionSection{}
	blk, err := s.Fetch(context.Background())
	if err != nil {
		t.Fatalf("Fetch returned error: %v", err)
	}
	if blk.Body != "" {
		t.Errorf("expected empty body for empty workspaces dir, got %q", blk.Body)
	}
}

func TestMissionSection_ReturnsLatestByMtime(t *testing.T) {
	base := t.TempDir()
	wsBase := filepath.Join(base, "workspaces")

	// Create two workspace dirs.
	for _, id := range []string{"ws-old", "ws-new"} {
		dir := filepath.Join(wsBase, id)
		if err := os.MkdirAll(dir, 0700); err != nil {
			t.Fatal(err)
		}
		now := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)
		if id == "ws-new" {
			now = now.Add(time.Hour)
		}
		writeFakeCheckpoint(t, dir, core.Checkpoint{
			WorkspaceID: id,
			Status:      "completed",
		})
		// Touch checkpoint to control mtime.
		if err := os.Chtimes(filepath.Join(dir, "checkpoint.json"), now, now); err != nil {
			t.Fatal(err)
		}
	}

	t.Setenv("ORCHESTRATOR_CONFIG_DIR", base)
	s := &missionSection{}
	blk, err := s.Fetch(context.Background())
	if err != nil {
		t.Fatalf("Fetch returned error: %v", err)
	}
	if blk.Body == "" {
		t.Fatal("expected non-empty body")
	}
	if !contains(blk.Body, "ws-new") {
		t.Errorf("expected body to reference ws-new, got:\n%s", blk.Body)
	}
	if contains(blk.Body, "ws-old") {
		t.Errorf("body should not reference ws-old, got:\n%s", blk.Body)
	}
}

func TestMissionSection_BodyFields(t *testing.T) {
	base := t.TempDir()
	wsBase := filepath.Join(base, "workspaces")
	wsDir := filepath.Join(wsBase, "abc-123")
	if err := os.MkdirAll(wsDir, 0700); err != nil {
		t.Fatal(err)
	}

	now := time.Date(2026, 3, 10, 12, 0, 0, 0, time.UTC)
	end := now
	writeFakeCheckpoint(t, wsDir, core.Checkpoint{
		WorkspaceID: "abc-123",
		Status:      "in_progress",
		Plan: &core.Plan{
			ID: "plan_xyz",
			Phases: []*core.Phase{
				{Name: "implement", Status: core.StatusCompleted, EndTime: &end},
				{Name: "review", Status: core.StatusRunning},
			},
		},
	})

	ts := now.Add(5 * time.Minute)
	writeFakeEventLog(t, filepath.Join(base, "events"), "abc-123", []time.Time{now, ts})

	t.Setenv("ORCHESTRATOR_CONFIG_DIR", base)
	s := &missionSection{}
	blk, err := s.Fetch(context.Background())
	if err != nil {
		t.Fatalf("Fetch returned error: %v", err)
	}

	for _, want := range []string{"abc-123", "in_progress", "review", "2026-03-10T12:05:00Z"} {
		if !contains(blk.Body, want) {
			t.Errorf("body missing %q:\n%s", want, blk.Body)
		}
	}
}

func TestMissionSection_LastEventFallsBackToCheckpointMtime(t *testing.T) {
	base := t.TempDir()
	wsDir := filepath.Join(base, "workspaces", "no-events")
	if err := os.MkdirAll(wsDir, 0700); err != nil {
		t.Fatal(err)
	}
	writeFakeCheckpoint(t, wsDir, core.Checkpoint{WorkspaceID: "no-events", Status: "completed"})

	// No events dir — should fall back to checkpoint mtime.
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", base)
	s := &missionSection{}
	blk, err := s.Fetch(context.Background())
	if err != nil {
		t.Fatalf("Fetch returned error: %v", err)
	}
	// Body should still have last_event populated (from checkpoint mtime).
	if !contains(blk.Body, "last_event:") {
		t.Errorf("expected last_event in body, got:\n%s", blk.Body)
	}
}

func TestCurrentPhaseName_PrefersRunning(t *testing.T) {
	end := time.Now()
	phases := []*core.Phase{
		{Name: "a", Status: core.StatusCompleted, EndTime: &end},
		{Name: "b", Status: core.StatusRunning},
		{Name: "c", Status: core.StatusPending},
	}
	if got := currentPhaseName(phases); got != "b" {
		t.Errorf("currentPhaseName = %q, want %q", got, "b")
	}
}

func TestCurrentPhaseName_FallsBackToLatestEnded(t *testing.T) {
	early := time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)
	late := early.Add(time.Hour)
	phases := []*core.Phase{
		{Name: "a", Status: core.StatusCompleted, EndTime: &early},
		{Name: "b", Status: core.StatusCompleted, EndTime: &late},
	}
	if got := currentPhaseName(phases); got != "b" {
		t.Errorf("currentPhaseName = %q, want %q", got, "b")
	}
}

func TestCurrentPhaseName_EmptyPhases(t *testing.T) {
	if got := currentPhaseName(nil); got != "" {
		t.Errorf("currentPhaseName(nil) = %q, want %q", got, "")
	}
}

// contains is a thin helper to avoid importing strings in test assertions.
func contains(s, substr string) bool {
	return len(s) >= len(substr) && (s == substr || len(substr) == 0 ||
		func() bool {
			for i := 0; i <= len(s)-len(substr); i++ {
				if s[i:i+len(substr)] == substr {
					return true
				}
			}
			return false
		}())
}
