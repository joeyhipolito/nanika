package daemon

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/config"
	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

// ---------- test helpers --------------------------------------------------

// newDiskTestServer redirects the config dir to a per-test temp directory and
// returns a fresh APIServer + Bus pair.  All on-disk state (checkpoints, event
// logs) written during the test is isolated to that temp dir.
func newDiskTestServer(t *testing.T) (*APIServer, *event.Bus) {
	t.Helper()
	t.Setenv(config.EnvVar, t.TempDir())
	bus := event.NewBus()
	srv := NewAPIServer(bus, event.NewBusEmitter(bus), Config{})
	t.Cleanup(func() { srv.Close() })
	return srv, bus
}

// writeOverlayCheckpoint writes a checkpoint.json with the given status and
// phases into the configured workspaces directory.
func writeOverlayCheckpoint(t *testing.T, missionID, status string, phases []*core.Phase) {
	t.Helper()
	base, err := config.Dir()
	if err != nil {
		t.Fatalf("config.Dir: %v", err)
	}
	wsPath := filepath.Join(base, "workspaces", missionID)
	if err := os.MkdirAll(wsPath, 0700); err != nil {
		t.Fatalf("mkdir workspace: %v", err)
	}
	plan := &core.Plan{Task: "test task", Phases: phases}
	if err := core.SaveCheckpoint(wsPath, plan, "test", status, time.Now().UTC()); err != nil {
		t.Fatalf("save checkpoint: %v", err)
	}
}

// writeOverlayEventLog writes events as JSONL to the configured events directory.
// Passing a nil or empty slice still creates the file (required for the detail
// endpoint which does os.Stat on the log path to determine existence).
func writeOverlayEventLog(t *testing.T, missionID string, events []event.Event) {
	t.Helper()
	logPath, err := event.EventLogPath(missionID)
	if err != nil {
		t.Fatalf("event log path: %v", err)
	}
	if err := os.MkdirAll(filepath.Dir(logPath), 0700); err != nil {
		t.Fatalf("mkdir events dir: %v", err)
	}
	f, err := os.Create(logPath)
	if err != nil {
		t.Fatalf("create event log: %v", err)
	}
	defer f.Close()
	enc := json.NewEncoder(f)
	for _, ev := range events {
		if err := enc.Encode(ev); err != nil {
			t.Fatalf("encode event: %v", err)
		}
	}
}

// waitForLiveMissionStatus polls srv.liveState until the mission has the
// expected status or the deadline expires.
func waitForLiveMissionStatus(t *testing.T, srv *APIServer, missionID, want string) {
	t.Helper()
	deadline := time.Now().Add(200 * time.Millisecond)
	for time.Now().Before(deadline) {
		if snap := srv.liveState.Mission(missionID); snap != nil && snap.Status == want {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for live mission %q status %q", missionID, want)
}

// waitForLivePhaseStatus polls srv.liveState until the phase has the expected
// status or the deadline expires.
func waitForLivePhaseStatus(t *testing.T, srv *APIServer, missionID, phaseID, want string) {
	t.Helper()
	deadline := time.Now().Add(200 * time.Millisecond)
	for time.Now().Before(deadline) {
		if snap := srv.liveState.Mission(missionID); snap != nil {
			if p, ok := snap.Phases[phaseID]; ok && p.Status == want {
				return
			}
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for live phase %q/%q status %q", missionID, phaseID, want)
}

// doJSONRequest is a one-liner helper for issuing a GET and unmarshalling JSON.
func doJSONRequest(t *testing.T, srv *APIServer, path string, out any) int {
	t.Helper()
	req := httptest.NewRequest(http.MethodGet, path, nil)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)
	if out != nil && rec.Code == http.StatusOK {
		if err := json.Unmarshal(rec.Body.Bytes(), out); err != nil {
			t.Fatalf("%s: unmarshal: %v", path, err)
		}
	}
	return rec.Code
}

// ---------- (1) status overlay: LiveState wins over in_progress checkpoint --

// TestGetMission_LiveStateWinsOverInProgress: checkpoint says "in_progress",
// LiveState says "completed" → the detail endpoint must return "completed".
func TestGetMission_LiveStateWinsOverInProgress(t *testing.T) {
	srv, bus := newDiskTestServer(t)

	const id = "20260316-overlay-detail"
	writeOverlayCheckpoint(t, id, "in_progress", []*core.Phase{
		{ID: "p1", Status: core.StatusPending},
	})
	writeOverlayEventLog(t, id, nil)

	bus.Publish(event.New(event.MissionStarted, id, "", "", nil))
	bus.Publish(event.New(event.MissionCompleted, id, "", "", nil))
	waitForLiveMissionStatus(t, srv, id, "completed")

	var detail MissionDetail
	if code := doJSONRequest(t, srv, "/api/missions/"+id, &detail); code != http.StatusOK {
		t.Fatalf("detail: want 200, got %d", code)
	}
	if detail.Status != "completed" {
		t.Errorf("detail: live 'completed' should win over checkpoint 'in_progress'; got %q", detail.Status)
	}
}

// TestGetMission_CheckpointTerminalStatusWins: checkpoint says "failed"
// (terminal/manually-set), LiveState says "in_progress" → checkpoint wins.
func TestGetMission_CheckpointTerminalStatusWins(t *testing.T) {
	srv, bus := newDiskTestServer(t)

	const id = "20260316-cpwins-detail"
	writeOverlayCheckpoint(t, id, "failed", []*core.Phase{
		{ID: "p1", Status: core.StatusCompleted},
	})
	writeOverlayEventLog(t, id, nil)

	// LiveState sees the mission as in_progress (stale / lagging projection).
	bus.Publish(event.New(event.MissionStarted, id, "", "", nil))
	waitForLiveMissionStatus(t, srv, id, "in_progress")

	var detail MissionDetail
	if code := doJSONRequest(t, srv, "/api/missions/"+id, &detail); code != http.StatusOK {
		t.Fatalf("detail: want 200, got %d", code)
	}
	if detail.Status != "failed" {
		t.Errorf("detail: checkpoint 'failed' must win over live 'in_progress'; got %q", detail.Status)
	}
}

// ---------- (1) same overlay logic on the list endpoint -------------------

// TestListMissions_LiveStateStatusOverlay: checkpoint says "in_progress",
// LiveState says "completed" → the list entry must show "completed".
func TestListMissions_LiveStateStatusOverlay(t *testing.T) {
	srv, bus := newDiskTestServer(t)

	const id = "20260316-overlay-list"
	writeOverlayCheckpoint(t, id, "in_progress", nil)
	writeOverlayEventLog(t, id, nil)

	bus.Publish(event.New(event.MissionStarted, id, "", "", nil))
	bus.Publish(event.New(event.MissionCompleted, id, "", "", nil))
	waitForLiveMissionStatus(t, srv, id, "completed")

	var missions []MissionLogInfo
	if code := doJSONRequest(t, srv, "/api/missions", &missions); code != http.StatusOK {
		t.Fatalf("list: want 200, got %d", code)
	}

	var found *MissionLogInfo
	for i := range missions {
		if missions[i].MissionID == id {
			found = &missions[i]
			break
		}
	}
	if found == nil {
		t.Fatalf("list: mission %q not returned", id)
	}
	if found.Status != "completed" {
		t.Errorf("list: live 'completed' should win over checkpoint 'in_progress'; got %q", found.Status)
	}
}

// TestListMissions_CheckpointTerminalStatusWins: checkpoint says "stalled",
// LiveState says "in_progress" → list entry must keep "stalled".
func TestListMissions_CheckpointTerminalStatusWins(t *testing.T) {
	srv, bus := newDiskTestServer(t)

	const id = "20260316-cpwins-list"
	writeOverlayCheckpoint(t, id, "stalled", nil)
	writeOverlayEventLog(t, id, nil)

	bus.Publish(event.New(event.MissionStarted, id, "", "", nil))
	waitForLiveMissionStatus(t, srv, id, "in_progress")

	var missions []MissionLogInfo
	if code := doJSONRequest(t, srv, "/api/missions", &missions); code != http.StatusOK {
		t.Fatalf("list: want 200, got %d", code)
	}

	var found *MissionLogInfo
	for i := range missions {
		if missions[i].MissionID == id {
			found = &missions[i]
			break
		}
	}
	if found == nil {
		t.Fatalf("list: mission %q not returned", id)
	}
	if found.Status != "stalled" {
		t.Errorf("list: checkpoint 'stalled' must win over live 'in_progress'; got %q", found.Status)
	}
}

// ---------- (1) DAG endpoint phase status overlay -------------------------

// TestMissionDAG_PhaseStatusOverlay: checkpoint phases start as "pending";
// LiveState sees p1 as "completed" → the DAG node for p1 must show "completed".
func TestMissionDAG_PhaseStatusOverlay(t *testing.T) {
	srv, bus := newDiskTestServer(t)

	const id = "20260316-dag-overlay"
	writeOverlayCheckpoint(t, id, "in_progress", []*core.Phase{
		{ID: "p1", Name: "build", Status: core.StatusPending},
		{ID: "p2", Name: "test", Status: core.StatusPending, Dependencies: []string{"p1"}},
	})
	writeOverlayEventLog(t, id, nil)

	bus.Publish(event.New(event.MissionStarted, id, "", "", nil))
	bus.Publish(event.New(event.PhaseStarted, id, "p1", "", map[string]any{"name": "build"}))
	bus.Publish(event.New(event.PhaseCompleted, id, "p1", "", nil))
	waitForLivePhaseStatus(t, srv, id, "p1", "completed")

	var dag DAGResponse
	if code := doJSONRequest(t, srv, "/api/missions/"+id+"/dag", &dag); code != http.StatusOK {
		t.Fatalf("dag: want 200, got %d", code)
	}

	var p1Status string
	for _, n := range dag.Nodes {
		if n.ID == "p1" {
			p1Status = n.Status
		}
	}
	if p1Status != "completed" {
		t.Errorf("dag node p1: expected live 'completed', got %q", p1Status)
	}
}

// TestMissionDAG_RetryingStatusOverlay: LiveState "retrying" (live-only) must
// surface in DAG node status even though it never appears in checkpoint.
func TestMissionDAG_RetryingStatusOverlay(t *testing.T) {
	srv, bus := newDiskTestServer(t)

	const id = "20260316-dag-retry"
	writeOverlayCheckpoint(t, id, "in_progress", []*core.Phase{
		{ID: "p1", Name: "flaky", Status: core.StatusRunning},
	})
	writeOverlayEventLog(t, id, nil)

	bus.Publish(event.New(event.MissionStarted, id, "", "", nil))
	bus.Publish(event.New(event.PhaseStarted, id, "p1", "", map[string]any{"name": "flaky"}))
	bus.Publish(event.New(event.PhaseRetrying, id, "p1", "", map[string]any{"attempt": 2}))
	waitForLivePhaseStatus(t, srv, id, "p1", "retrying")

	var dag DAGResponse
	if code := doJSONRequest(t, srv, "/api/missions/"+id+"/dag", &dag); code != http.StatusOK {
		t.Fatalf("dag: want 200, got %d", code)
	}

	var p1Status string
	for _, n := range dag.Nodes {
		if n.ID == "p1" {
			p1Status = n.Status
		}
	}
	if p1Status != "retrying" {
		t.Errorf("dag node p1: expected live 'retrying', got %q", p1Status)
	}
}

// ---------- (2) nil-fallback when no event log exists ---------------------

// TestGetMission_NoEventLog_Returns404: the detail endpoint uses os.Stat on the
// event log path; a missing log must produce 404 (not a panic or 500).
func TestGetMission_NoEventLog_Returns404(t *testing.T) {
	srv, _ := newDiskTestServer(t)

	// Write a checkpoint but no event log.
	const id = "20260316-no-log"
	writeOverlayCheckpoint(t, id, "in_progress", nil)
	// Deliberately skip writeOverlayEventLog.

	code := doJSONRequest(t, srv, "/api/missions/"+id, nil)
	if code != http.StatusNotFound {
		t.Errorf("detail without event log: want 404, got %d", code)
	}
}

// TestListMissions_NoEventLog_MissionAbsent: missions with no event log file
// must not appear in the list (the list is derived from .jsonl files in the
// events directory).
func TestListMissions_NoEventLog_MissionAbsent(t *testing.T) {
	srv, _ := newDiskTestServer(t)

	const id = "20260316-no-log-list"
	writeOverlayCheckpoint(t, id, "in_progress", nil)
	// No event log written.

	var missions []MissionLogInfo
	if code := doJSONRequest(t, srv, "/api/missions", &missions); code != http.StatusOK {
		t.Fatalf("list: want 200, got %d", code)
	}
	for _, m := range missions {
		if m.MissionID == id {
			t.Errorf("list: mission %q should not appear without an event log", id)
		}
	}
}

// ---------- (3) cross-endpoint status consistency -------------------------

// TestStatusConsistency_ListDetailDAGAgree verifies that all three endpoints
// (/api/missions, /api/missions/{id}, /api/missions/{id}/dag) report the same
// mission-level status when LiveState and the checkpoint agree.
func TestStatusConsistency_ListDetailDAGAgree(t *testing.T) {
	srv, bus := newDiskTestServer(t)

	const id = "20260316-consistency"
	writeOverlayCheckpoint(t, id, "in_progress", []*core.Phase{
		{ID: "p1", Name: "build", Status: core.StatusPending},
	})
	writeOverlayEventLog(t, id, nil)

	bus.Publish(event.New(event.MissionStarted, id, "", "", nil))
	bus.Publish(event.New(event.PhaseStarted, id, "p1", "", map[string]any{"name": "build"}))
	waitForLivePhaseStatus(t, srv, id, "p1", "running")

	// List.
	var missions []MissionLogInfo
	if code := doJSONRequest(t, srv, "/api/missions", &missions); code != http.StatusOK {
		t.Fatalf("list: want 200, got %d", code)
	}
	var listStatus string
	for _, m := range missions {
		if m.MissionID == id {
			listStatus = m.Status
		}
	}
	if listStatus == "" {
		t.Fatalf("list: mission %q not found", id)
	}

	// Detail.
	var detail MissionDetail
	if code := doJSONRequest(t, srv, "/api/missions/"+id, &detail); code != http.StatusOK {
		t.Fatalf("detail: want 200, got %d", code)
	}

	// DAG.
	var dag DAGResponse
	if code := doJSONRequest(t, srv, "/api/missions/"+id+"/dag", &dag); code != http.StatusOK {
		t.Fatalf("dag: want 200, got %d", code)
	}

	// List and detail must agree on mission-level status.
	if listStatus != detail.Status {
		t.Errorf("list status %q != detail status %q", listStatus, detail.Status)
	}

	// DAG p1 node must reflect the live phase status.
	var dagP1Status string
	for _, n := range dag.Nodes {
		if n.ID == "p1" {
			dagP1Status = n.Status
		}
	}
	if dagP1Status != "running" {
		t.Errorf("dag node p1: expected live 'running', got %q", dagP1Status)
	}
}

// TestStatusConsistency_TerminalCheckpointAllEndpoints verifies that a terminal
// checkpoint status (cancelled) is returned consistently by list, detail, and
// that the DAG node status overlays still reflect live phase state.
func TestStatusConsistency_TerminalCheckpointAllEndpoints(t *testing.T) {
	srv, bus := newDiskTestServer(t)

	const id = "20260316-consistency-terminal"
	writeOverlayCheckpoint(t, id, "cancelled", []*core.Phase{
		{ID: "p1", Name: "build", Status: core.StatusCompleted},
		{ID: "p2", Name: "deploy", Status: core.StatusSkipped},
	})
	writeOverlayEventLog(t, id, nil)

	// LiveState sees a cancelled mission with skipped phases.
	bus.Publish(event.New(event.MissionStarted, id, "", "", nil))
	bus.Publish(event.New(event.PhaseStarted, id, "p1", "", map[string]any{"name": "build"}))
	bus.Publish(event.New(event.PhaseCompleted, id, "p1", "", nil))
	bus.Publish(event.New(event.MissionCancelled, id, "", "", nil))
	bus.Publish(event.New(event.PhaseSkipped, id, "p2", "", map[string]any{"reason": "cancelled"}))
	waitForLivePhaseStatus(t, srv, id, "p2", "skipped")

	// List must show the checkpoint's terminal status.
	var missions []MissionLogInfo
	if code := doJSONRequest(t, srv, "/api/missions", &missions); code != http.StatusOK {
		t.Fatalf("list: want 200, got %d", code)
	}
	var listStatus string
	for _, m := range missions {
		if m.MissionID == id {
			listStatus = m.Status
		}
	}
	if listStatus != "cancelled" {
		t.Errorf("list: expected 'cancelled', got %q", listStatus)
	}

	// Detail must show the checkpoint's terminal status.
	var detail MissionDetail
	if code := doJSONRequest(t, srv, "/api/missions/"+id, &detail); code != http.StatusOK {
		t.Fatalf("detail: want 200, got %d", code)
	}
	if detail.Status != "cancelled" {
		t.Errorf("detail: expected 'cancelled', got %q", detail.Status)
	}

	// List and detail must agree.
	if listStatus != detail.Status {
		t.Errorf("list status %q != detail status %q", listStatus, detail.Status)
	}

	// DAG phase overlays must still reflect live phase state.
	var dag DAGResponse
	if code := doJSONRequest(t, srv, "/api/missions/"+id+"/dag", &dag); code != http.StatusOK {
		t.Fatalf("dag: want 200, got %d", code)
	}
	statusByID := map[string]string{}
	for _, n := range dag.Nodes {
		statusByID[n.ID] = n.Status
	}
	if statusByID["p1"] != "completed" {
		t.Errorf("dag p1: expected 'completed', got %q", statusByID["p1"])
	}
	if statusByID["p2"] != "skipped" {
		t.Errorf("dag p2: expected 'skipped', got %q", statusByID["p2"])
	}
}
