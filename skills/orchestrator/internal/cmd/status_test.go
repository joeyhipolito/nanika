package cmd

import (
	"bytes"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/config"
	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

func TestShowStatus_UsesEventCountsForTerminalCheckpointStatus(t *testing.T) {
	t.Setenv(config.EnvVar, t.TempDir())
	wsPath := writeStatusWorkspace(t, "20260315-teststatus", "completed", []*core.Phase{
		{ID: "p1", Status: core.StatusCompleted},
		{ID: "p2", Status: core.StatusPending},
	})
	writeEventLog(t, "20260315-teststatus", []event.Event{
		{Type: event.MissionStarted, MissionID: "20260315-teststatus", Timestamp: time.Now().UTC()},
		{Type: event.PhaseStarted, MissionID: "20260315-teststatus", PhaseID: "p1", Timestamp: time.Now().UTC()},
		{Type: event.PhaseCompleted, MissionID: "20260315-teststatus", PhaseID: "p1", Timestamp: time.Now().UTC()},
		{Type: event.PhaseStarted, MissionID: "20260315-teststatus", PhaseID: "p2", Timestamp: time.Now().UTC()},
		{Type: event.PhaseCompleted, MissionID: "20260315-teststatus", PhaseID: "p2", Timestamp: time.Now().UTC()},
		{Type: event.MissionCompleted, MissionID: "20260315-teststatus", Timestamp: time.Now().UTC()},
	})
	_ = wsPath

	cmd := &cobra.Command{}
	var stdout bytes.Buffer
	var stderr bytes.Buffer
	cmd.SetOut(&stdout)
	cmd.SetErr(&stderr)

	if err := showStatus(cmd, nil); err != nil {
		t.Fatalf("showStatus returned error: %v", err)
	}

	out := stdout.String()
	if !strings.Contains(out, "20260315-teststatus [completed] 2/2 phases") {
		t.Fatalf("expected event-derived completed count in output, got:\n%s", out)
	}
	if stderr.Len() != 0 {
		t.Fatalf("expected no stderr, got: %s", stderr.String())
	}
}

func TestShowStatus_WarnsAndFallsBackWhenProjectionFails(t *testing.T) {
	t.Setenv(config.EnvVar, t.TempDir())
	writeStatusWorkspace(t, "20260315-testwarn", "in_progress", []*core.Phase{
		{ID: "p1", Status: core.StatusCompleted},
		{ID: "p2", Status: core.StatusPending},
	})

	orig := projectFromLog
	projectFromLog = func(string) (*event.MissionSnap, error) {
		return nil, errors.New("boom")
	}
	defer func() { projectFromLog = orig }()

	cmd := &cobra.Command{}
	var stdout bytes.Buffer
	var stderr bytes.Buffer
	cmd.SetOut(&stdout)
	cmd.SetErr(&stderr)

	if err := showStatus(cmd, nil); err != nil {
		t.Fatalf("showStatus returned error: %v", err)
	}

	if got := stdout.String(); !strings.Contains(got, "20260315-testwarn [in_progress] 1/2 phases") {
		t.Fatalf("expected checkpoint fallback output, got:\n%s", got)
	}
	if got := stderr.String(); !strings.Contains(got, "warning: live status unavailable for 20260315-testwarn: boom") {
		t.Fatalf("expected warning on stderr, got:\n%s", got)
	}
}

// ---------- status overlay: LiveState vs checkpoint -----------------------

// TestShowStatus_NilSnapFallsBackToCheckpoint covers the branch where
// projectFromLog returns (nil, nil) — i.e. the event log file does not exist.
// showStatus must fall back to checkpoint-derived phase counts and keep the
// checkpoint status unchanged.
func TestShowStatus_NilSnapFallsBackToCheckpoint(t *testing.T) {
	t.Setenv(config.EnvVar, t.TempDir())
	writeStatusWorkspace(t, "20260316-nilsnap", "in_progress", []*core.Phase{
		{ID: "p1", Status: core.StatusCompleted},
		{ID: "p2", Status: core.StatusPending},
	})

	orig := projectFromLog
	projectFromLog = func(string) (*event.MissionSnap, error) {
		return nil, nil // no event log — caller should fall back to checkpoint
	}
	defer func() { projectFromLog = orig }()

	cmd := &cobra.Command{}
	var stdout, stderr bytes.Buffer
	cmd.SetOut(&stdout)
	cmd.SetErr(&stderr)

	if err := showStatus(cmd, nil); err != nil {
		t.Fatalf("showStatus returned error: %v", err)
	}

	// Status stays from checkpoint; completed count comes from checkpoint phases.
	if got := stdout.String(); !strings.Contains(got, "20260316-nilsnap [in_progress] 1/2 phases") {
		t.Fatalf("expected checkpoint fallback output, got:\n%s", got)
	}
	// No warning should appear for a clean nil-snap (not an error).
	if stderr.Len() != 0 {
		t.Fatalf("expected no stderr for nil snap, got: %s", stderr.String())
	}
}

// TestShowStatus_LiveStatusWinsWhenCheckpointIsInProgress verifies that when
// the checkpoint says "in_progress" but the event-log projection reports a
// terminal status, the live status is used for display.
func TestShowStatus_LiveStatusWinsWhenCheckpointIsInProgress(t *testing.T) {
	t.Setenv(config.EnvVar, t.TempDir())
	writeStatusWorkspace(t, "20260316-livewins", "in_progress", []*core.Phase{
		{ID: "p1", Status: core.StatusPending},
		{ID: "p2", Status: core.StatusPending},
	})

	orig := projectFromLog
	projectFromLog = func(string) (*event.MissionSnap, error) {
		return &event.MissionSnap{
			MissionID: "20260316-livewins",
			Status:    "completed",
			Phases: map[string]*event.PhaseSnap{
				"p1": {ID: "p1", Status: "completed"},
				"p2": {ID: "p2", Status: "completed"},
			},
		}, nil
	}
	defer func() { projectFromLog = orig }()

	cmd := &cobra.Command{}
	var stdout, stderr bytes.Buffer
	cmd.SetOut(&stdout)
	cmd.SetErr(&stderr)

	if err := showStatus(cmd, nil); err != nil {
		t.Fatalf("showStatus returned error: %v", err)
	}

	out := stdout.String()
	if !strings.Contains(out, "20260316-livewins [completed]") {
		t.Fatalf("expected live status 'completed' to win over checkpoint 'in_progress', got:\n%s", out)
	}
	if !strings.Contains(out, "2/2 phases") {
		t.Fatalf("expected live phase count 2/2, got:\n%s", out)
	}
	if stderr.Len() != 0 {
		t.Fatalf("expected no stderr, got: %s", stderr.String())
	}
}

// TestShowStatus_CheckpointTerminalStatusWinsOverLive verifies that when the
// checkpoint already carries a terminal status (completed/failed/stalled), the
// live projection's status is NOT substituted — manual overrides must win.
func TestShowStatus_CheckpointTerminalStatusWinsOverLive(t *testing.T) {
	t.Setenv(config.EnvVar, t.TempDir())
	writeStatusWorkspace(t, "20260316-cpwins", "failed", []*core.Phase{
		{ID: "p1", Status: core.StatusCompleted},
		{ID: "p2", Status: core.StatusPending},
	})

	orig := projectFromLog
	projectFromLog = func(string) (*event.MissionSnap, error) {
		// Stale live state that disagrees with the checkpoint terminal status.
		return &event.MissionSnap{
			MissionID: "20260316-cpwins",
			Status:    "in_progress",
			Phases: map[string]*event.PhaseSnap{
				"p1": {ID: "p1", Status: "running"},
			},
		}, nil
	}
	defer func() { projectFromLog = orig }()

	cmd := &cobra.Command{}
	var stdout, stderr bytes.Buffer
	cmd.SetOut(&stdout)
	cmd.SetErr(&stderr)

	if err := showStatus(cmd, nil); err != nil {
		t.Fatalf("showStatus returned error: %v", err)
	}

	out := stdout.String()
	if !strings.Contains(out, "20260316-cpwins [failed]") {
		t.Fatalf("checkpoint terminal status 'failed' must win over stale live 'in_progress', got:\n%s", out)
	}
	if stderr.Len() != 0 {
		t.Fatalf("expected no stderr, got: %s", stderr.String())
	}
}

func TestShowStatus_ShowsRunningMissions(t *testing.T) {
	t.Setenv(config.EnvVar, t.TempDir())

	startTime := time.Now().UTC().Add(-5 * time.Minute)
	phaseStart := startTime.Add(30 * time.Second)
	writeEventLog(t, "20260323-running01", []event.Event{
		{Type: event.MissionStarted, MissionID: "20260323-running01", Timestamp: startTime},
		{Type: event.PhaseStarted, MissionID: "20260323-running01", PhaseID: "p1", Timestamp: phaseStart,
			Data: map[string]any{"name": "implement"}},
	})

	cmd := &cobra.Command{}
	var stdout, stderr bytes.Buffer
	cmd.SetOut(&stdout)
	cmd.SetErr(&stderr)

	if err := showStatus(cmd, nil); err != nil {
		t.Fatalf("showStatus returned error: %v", err)
	}

	out := stdout.String()
	if !strings.Contains(out, "Running missions:") {
		t.Fatalf("expected 'Running missions:' header, got:\n%s", out)
	}
	if !strings.Contains(out, "20260323-running01") {
		t.Fatalf("expected running mission ID in output, got:\n%s", out)
	}
	if !strings.Contains(out, "phase: implement") {
		t.Fatalf("expected current phase name in output, got:\n%s", out)
	}
	if !strings.Contains(out, "elapsed:") {
		t.Fatalf("expected elapsed duration in output, got:\n%s", out)
	}
	if stderr.Len() != 0 {
		t.Fatalf("expected no stderr, got: %s", stderr.String())
	}
}

func TestShowStatus_CompletedMissionsNotInRunning(t *testing.T) {
	t.Setenv(config.EnvVar, t.TempDir())

	now := time.Now().UTC()
	writeEventLog(t, "20260323-done01", []event.Event{
		{Type: event.MissionStarted, MissionID: "20260323-done01", Timestamp: now.Add(-10 * time.Minute)},
		{Type: event.PhaseStarted, MissionID: "20260323-done01", PhaseID: "p1", Timestamp: now.Add(-9 * time.Minute),
			Data: map[string]any{"name": "implement"}},
		{Type: event.PhaseCompleted, MissionID: "20260323-done01", PhaseID: "p1", Timestamp: now.Add(-1 * time.Minute)},
		{Type: event.MissionCompleted, MissionID: "20260323-done01", Timestamp: now},
	})

	cmd := &cobra.Command{}
	var stdout bytes.Buffer
	cmd.SetOut(&stdout)
	cmd.SetErr(&bytes.Buffer{})

	if err := showStatus(cmd, nil); err != nil {
		t.Fatalf("showStatus returned error: %v", err)
	}

	out := stdout.String()
	if strings.Contains(out, "Running missions:") {
		t.Fatalf("completed mission should not appear in running section, got:\n%s", out)
	}
}

func TestShowStatus_NoRunningMissionsNoHeader(t *testing.T) {
	t.Setenv(config.EnvVar, t.TempDir())

	cmd := &cobra.Command{}
	var stdout bytes.Buffer
	cmd.SetOut(&stdout)
	cmd.SetErr(&bytes.Buffer{})

	if err := showStatus(cmd, nil); err != nil {
		t.Fatalf("showStatus returned error: %v", err)
	}

	if strings.Contains(stdout.String(), "Running missions:") {
		t.Fatalf("expected no running missions header when events dir is empty, got:\n%s", stdout.String())
	}
}

func writeStatusWorkspace(t *testing.T, workspaceID, status string, phases []*core.Phase) string {
	t.Helper()
	base, err := config.Dir()
	if err != nil {
		t.Fatalf("config dir: %v", err)
	}
	wsPath := filepath.Join(base, "workspaces", workspaceID)
	if err := os.MkdirAll(wsPath, 0700); err != nil {
		t.Fatalf("mkdir workspace: %v", err)
	}
	if err := os.WriteFile(filepath.Join(wsPath, "mission.md"), []byte("test mission"), 0600); err != nil {
		t.Fatalf("write mission: %v", err)
	}
	plan := &core.Plan{
		ID:     "plan-1",
		Task:   "test mission",
		Phases: phases,
	}
	if err := core.SaveCheckpoint(wsPath, plan, "dev", status, time.Now().UTC()); err != nil {
		t.Fatalf("save checkpoint: %v", err)
	}
	return wsPath
}

func writeEventLog(t *testing.T, workspaceID string, events []event.Event) {
	t.Helper()
	logPath, err := event.EventLogPath(workspaceID)
	if err != nil {
		t.Fatalf("event log path: %v", err)
	}
	if err := os.MkdirAll(filepath.Dir(logPath), 0700); err != nil {
		t.Fatalf("mkdir events dir: %v", err)
	}
	f, err := os.Create(logPath)
	if err != nil {
		t.Fatalf("create log: %v", err)
	}
	defer f.Close()
	for _, ev := range events {
		data, err := json.Marshal(ev)
		if err != nil {
			t.Fatalf("marshal event: %v", err)
		}
		if _, err := f.Write(data); err != nil {
			t.Fatalf("write event: %v", err)
		}
		if _, err := f.Write([]byte{'\n'}); err != nil {
			t.Fatalf("write newline: %v", err)
		}
	}
}
