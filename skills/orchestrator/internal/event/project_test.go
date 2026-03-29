package event

import (
	"bufio"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/config"
)

// writeLog writes events as JSONL to a temp file and returns the path.
func writeLog(t *testing.T, events []Event) string {
	t.Helper()
	f, err := os.CreateTemp(t.TempDir(), "events-*.jsonl")
	if err != nil {
		t.Fatalf("creating temp log: %v", err)
	}
	defer f.Close()
	for _, ev := range events {
		b, err := json.Marshal(ev)
		if err != nil {
			t.Fatalf("marshalling event: %v", err)
		}
		f.Write(b)
		f.Write([]byte{'\n'})
	}
	return f.Name()
}

// replayLog replays the JSONL file at path through a Projection and returns
// the mission snapshot for missionID. Uses line-by-line scanning (matching
// ProjectFromLog) so corrupt lines are skipped without blocking.
func replayLog(t *testing.T, path, missionID string) *MissionSnap {
	t.Helper()
	f, err := os.Open(path)
	if err != nil {
		t.Fatalf("opening log: %v", err)
	}
	defer f.Close()

	proj := newProjection()
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 1<<20), 1<<20)
	for scanner.Scan() {
		var ev Event
		if err := json.Unmarshal(scanner.Bytes(), &ev); err != nil {
			continue
		}
		proj.apply(ev)
	}
	return proj.Mission(missionID)
}

// ---- Projection unit tests -------------------------------------------------

func TestProjection_MissionCompleted(t *testing.T) {
	ts := time.Now().UTC()
	events := []Event{
		{Type: MissionStarted, MissionID: "m1", Timestamp: ts},
		{Type: MissionCompleted, MissionID: "m1", Timestamp: ts.Add(time.Second)},
	}
	path := writeLog(t, events)
	snap := replayLog(t, path, "m1")
	if snap == nil {
		t.Fatal("expected snap, got nil")
	}
	if snap.Status != "completed" {
		t.Fatalf("want completed, got %q", snap.Status)
	}
	if snap.EndedAt.IsZero() {
		t.Fatal("EndedAt should be set on completed mission")
	}
}

func TestProjection_MissionFailed(t *testing.T) {
	ts := time.Now().UTC()
	events := []Event{
		{Type: MissionStarted, MissionID: "m1", Timestamp: ts},
		{Type: MissionFailed, MissionID: "m1", Timestamp: ts.Add(time.Second)},
	}
	snap := replayLog(t, writeLog(t, events), "m1")
	if snap.Status != "failed" {
		t.Fatalf("want failed, got %q", snap.Status)
	}
}

func TestProjection_MissionCancelled(t *testing.T) {
	ts := time.Now().UTC()
	events := []Event{
		{Type: MissionStarted, MissionID: "m1", Timestamp: ts},
		{Type: MissionCancelled, MissionID: "m1", Timestamp: ts.Add(time.Second)},
	}
	snap := replayLog(t, writeLog(t, events), "m1")
	if snap.Status != "cancelled" {
		t.Fatalf("want cancelled, got %q", snap.Status)
	}
}

func TestProjection_InProgressWhenNoTerminalEvent(t *testing.T) {
	// Only mission.started received — simulates a mission killed mid-run.
	events := []Event{
		{Type: MissionStarted, MissionID: "m1", Timestamp: time.Now().UTC()},
	}
	snap := replayLog(t, writeLog(t, events), "m1")
	if snap.Status != "in_progress" {
		t.Fatalf("want in_progress, got %q", snap.Status)
	}
}

func TestProjection_PhaseStatuses(t *testing.T) {
	ts := time.Now().UTC()
	events := []Event{
		{Type: MissionStarted, MissionID: "m1", Timestamp: ts},
		{Type: PhaseStarted, MissionID: "m1", PhaseID: "p1", Timestamp: ts.Add(time.Second), Data: map[string]any{"name": "setup"}},
		{Type: PhaseCompleted, MissionID: "m1", PhaseID: "p1", Timestamp: ts.Add(2 * time.Second)},
		{Type: PhaseStarted, MissionID: "m1", PhaseID: "p2", Timestamp: ts.Add(3 * time.Second), Data: map[string]any{"name": "build"}},
		{Type: PhaseFailed, MissionID: "m1", PhaseID: "p2", Timestamp: ts.Add(4 * time.Second)},
		{Type: PhaseStarted, MissionID: "m1", PhaseID: "p3", Timestamp: ts.Add(5 * time.Second), Data: map[string]any{"name": "verify"}},
		{Type: PhaseSkipped, MissionID: "m1", PhaseID: "p3", Timestamp: ts.Add(6 * time.Second)},
	}
	snap := replayLog(t, writeLog(t, events), "m1")
	if snap == nil {
		t.Fatal("expected snap, got nil")
	}
	cases := []struct{ id, want string }{
		{"p1", "completed"},
		{"p2", "failed"},
		{"p3", "skipped"},
	}
	for _, c := range cases {
		ph := snap.Phases[c.id]
		if ph == nil {
			t.Errorf("phase %s not found", c.id)
			continue
		}
		if ph.Status != c.want {
			t.Errorf("phase %s: want %q, got %q", c.id, c.want, ph.Status)
		}
	}
}

func TestProjection_PhaseRetrying(t *testing.T) {
	ts := time.Now().UTC()
	events := []Event{
		{Type: MissionStarted, MissionID: "m1", Timestamp: ts},
		{Type: PhaseStarted, MissionID: "m1", PhaseID: "p1", Timestamp: ts.Add(time.Second)},
		{Type: PhaseRetrying, MissionID: "m1", PhaseID: "p1", Timestamp: ts.Add(2 * time.Second)},
	}
	snap := replayLog(t, writeLog(t, events), "m1")
	ph := snap.Phases["p1"]
	if ph == nil {
		t.Fatal("expected phase p1")
	}
	if ph.Status != "retrying" {
		t.Fatalf("want retrying, got %q", ph.Status)
	}
}

func TestProjection_LateJoinPhase(t *testing.T) {
	// Phase event arrives without prior mission.started — Projection should
	// synthesise a mission snap with "in_progress".
	ts := time.Now().UTC()
	events := []Event{
		{Type: PhaseStarted, MissionID: "m1", PhaseID: "p1", Timestamp: ts},
	}
	snap := replayLog(t, writeLog(t, events), "m1")
	if snap == nil {
		t.Fatal("expected synthesised snap, got nil")
	}
	if snap.Status != "in_progress" {
		t.Fatalf("want in_progress for late-join, got %q", snap.Status)
	}
}

func TestProjection_CorruptLinesSkipped(t *testing.T) {
	// Write one good event sandwiched by two corrupt lines.
	f, err := os.CreateTemp(t.TempDir(), "events-*.jsonl")
	if err != nil {
		t.Fatalf("creating temp log: %v", err)
	}
	f.WriteString("not json\n")
	good, _ := json.Marshal(Event{Type: MissionStarted, MissionID: "m1", Timestamp: time.Now().UTC()})
	f.Write(good)
	f.WriteString("\n{bad}\n")
	f.Close()

	snap := replayLog(t, f.Name(), "m1")
	if snap == nil {
		t.Fatal("expected snap despite corrupt lines")
	}
	if snap.Status != "in_progress" {
		t.Fatalf("want in_progress, got %q", snap.Status)
	}
}

func TestProjection_UnknownMissionReturnsNil(t *testing.T) {
	events := []Event{
		{Type: MissionStarted, MissionID: "m1", Timestamp: time.Now().UTC()},
	}
	snap := replayLog(t, writeLog(t, events), "other-mission")
	if snap != nil {
		t.Fatal("expected nil for unseen mission ID")
	}
}

func TestProjection_SnapshotIsCopy(t *testing.T) {
	// Mutations on the returned snap must not affect future calls.
	ts := time.Now().UTC()
	events := []Event{
		{Type: MissionStarted, MissionID: "m1", Timestamp: ts},
		{Type: PhaseStarted, MissionID: "m1", PhaseID: "p1", Timestamp: ts.Add(time.Second)},
	}
	path := writeLog(t, events)

	proj := newProjection()
	f, _ := os.Open(path)
	defer f.Close()
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		var ev Event
		if json.Unmarshal(sc.Bytes(), &ev) == nil {
			proj.apply(ev)
		}
	}

	snap1 := proj.Mission("m1")
	snap1.Status = "mutated"
	snap1.Phases["p1"].Status = "mutated"

	snap2 := proj.Mission("m1")
	if snap2.Status != "in_progress" {
		t.Fatalf("external mutation leaked: got %q", snap2.Status)
	}
	if snap2.Phases["p1"].Status != "running" {
		t.Fatalf("external phase mutation leaked: got %q", snap2.Phases["p1"].Status)
	}
}

// ---- ProjectFromLog integration tests -------------------------------------

func TestProjectFromLog_NoLogReturnsNil(t *testing.T) {
	t.Setenv(config.EnvVar, t.TempDir())
	snap, err := ProjectFromLog("nonexistent")
	if err != nil {
		t.Fatalf("ProjectFromLog returned error: %v", err)
	}
	if snap != nil {
		t.Fatal("expected nil snap for missing log")
	}
}

func TestProjectFromLog_UsesConfiguredEventsDir(t *testing.T) {
	tmpDir := t.TempDir()
	t.Setenv(config.EnvVar, tmpDir)
	logPath := filepath.Join(tmpDir, "events", "m1.jsonl")
	if err := os.MkdirAll(filepath.Dir(logPath), 0700); err != nil {
		t.Fatalf("mkdir events dir: %v", err)
	}
	events := []Event{
		{Type: MissionStarted, MissionID: "m1", Timestamp: time.Now().UTC()},
		{Type: MissionCompleted, MissionID: "m1", Timestamp: time.Now().UTC().Add(time.Second)},
	}
	f, err := os.Create(logPath)
	if err != nil {
		t.Fatalf("create log: %v", err)
	}
	for _, ev := range events {
		b, err := json.Marshal(ev)
		if err != nil {
			t.Fatalf("marshal event: %v", err)
		}
		if _, err := f.Write(b); err != nil {
			t.Fatalf("write event: %v", err)
		}
		if _, err := f.Write([]byte{'\n'}); err != nil {
			t.Fatalf("write newline: %v", err)
		}
	}
	if err := f.Close(); err != nil {
		t.Fatalf("close log: %v", err)
	}

	snap, err := ProjectFromLog("m1")
	if err != nil {
		t.Fatalf("ProjectFromLog returned error: %v", err)
	}
	if snap != nil {
		if snap.Status != "completed" {
			t.Fatalf("want completed, got %q", snap.Status)
		}
		return
	}

	t.Fatal("expected snapshot from configured events dir")
}

func TestProjectFromLog_PhaseCountMatchesEvents(t *testing.T) {
	tmpDir := t.TempDir()
	t.Setenv(config.EnvVar, tmpDir)
	logPath := filepath.Join(tmpDir, "events", "m1.jsonl")
	if err := os.MkdirAll(filepath.Dir(logPath), 0700); err != nil {
		t.Fatalf("mkdir events dir: %v", err)
	}
	ts := time.Now().UTC()
	events := []Event{
		{Type: MissionStarted, MissionID: "m1", Timestamp: ts},
		{Type: PhaseStarted, MissionID: "m1", PhaseID: "p1", Timestamp: ts.Add(time.Second)},
		{Type: PhaseCompleted, MissionID: "m1", PhaseID: "p1", Timestamp: ts.Add(2 * time.Second)},
		{Type: PhaseStarted, MissionID: "m1", PhaseID: "p2", Timestamp: ts.Add(3 * time.Second)},
		{Type: PhaseCompleted, MissionID: "m1", PhaseID: "p2", Timestamp: ts.Add(4 * time.Second)},
		{Type: MissionCompleted, MissionID: "m1", Timestamp: ts.Add(5 * time.Second)},
	}
	f, err := os.Create(logPath)
	if err != nil {
		t.Fatalf("create log: %v", err)
	}
	for _, ev := range events {
		b, err := json.Marshal(ev)
		if err != nil {
			t.Fatalf("marshal event: %v", err)
		}
		if _, err := f.Write(b); err != nil {
			t.Fatalf("write event: %v", err)
		}
		if _, err := f.Write([]byte{'\n'}); err != nil {
			t.Fatalf("write newline: %v", err)
		}
	}
	if err := f.Close(); err != nil {
		t.Fatalf("close log: %v", err)
	}

	snap, err := ProjectFromLog("m1")
	if err != nil {
		t.Fatalf("ProjectFromLog returned error: %v", err)
	}
	if snap.Status != "completed" {
		t.Fatalf("want completed, got %q", snap.Status)
	}
	completedCount := 0
	for _, ph := range snap.Phases {
		if ph.Status == "completed" {
			completedCount++
		}
	}
	if completedCount != 2 {
		t.Fatalf("want 2 completed phases, got %d", completedCount)
	}
}
