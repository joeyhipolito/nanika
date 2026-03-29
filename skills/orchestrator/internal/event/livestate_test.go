package event

import (
	"testing"
	"time"
)

func TestLiveState_MissionStarted(t *testing.T) {
	b := NewBus()
	ls := NewLiveState(b)
	defer ls.Close()

	b.Publish(New(MissionStarted, "m1", "", "", map[string]any{"task": "do stuff"}))
	waitForSnap(t, ls, "m1")

	snap := ls.Mission("m1")
	if snap == nil {
		t.Fatal("expected snap, got nil")
	}
	if snap.Status != "in_progress" {
		t.Fatalf("expected in_progress, got %q", snap.Status)
	}
	if snap.MissionID != "m1" {
		t.Fatalf("expected mission_id m1, got %q", snap.MissionID)
	}
}

func TestLiveState_MissionCompleted(t *testing.T) {
	b := NewBus()
	ls := NewLiveState(b)
	defer ls.Close()

	b.Publish(New(MissionStarted, "m1", "", "", nil))
	waitForSnap(t, ls, "m1")

	b.Publish(New(MissionCompleted, "m1", "", "", nil))
	waitForStatus(t, ls, "m1", "completed")

	snap := ls.Mission("m1")
	if snap.Status != "completed" {
		t.Fatalf("expected completed, got %q", snap.Status)
	}
	if snap.EndedAt.IsZero() {
		t.Fatal("expected non-zero EndedAt on completed mission")
	}
}

func TestLiveState_MissionFailed(t *testing.T) {
	b := NewBus()
	ls := NewLiveState(b)
	defer ls.Close()

	b.Publish(New(MissionStarted, "m1", "", "", nil))
	waitForSnap(t, ls, "m1")
	b.Publish(New(MissionFailed, "m1", "", "", nil))
	waitForStatus(t, ls, "m1", "failed")

	if snap := ls.Mission("m1"); snap.Status != "failed" {
		t.Fatalf("expected failed, got %q", snap.Status)
	}
}

func TestLiveState_MissionCancelled(t *testing.T) {
	b := NewBus()
	ls := NewLiveState(b)
	defer ls.Close()

	b.Publish(New(MissionStarted, "m1", "", "", nil))
	waitForSnap(t, ls, "m1")
	b.Publish(New(MissionCancelled, "m1", "", "", nil))
	waitForStatus(t, ls, "m1", "cancelled")

	if snap := ls.Mission("m1"); snap.Status != "cancelled" {
		t.Fatalf("expected cancelled, got %q", snap.Status)
	}
}

func TestLiveState_PhaseLifecycle(t *testing.T) {
	b := NewBus()
	ls := NewLiveState(b)
	defer ls.Close()

	b.Publish(New(MissionStarted, "m1", "", "", nil))
	waitForSnap(t, ls, "m1")

	b.Publish(New(PhaseStarted, "m1", "p1", "", map[string]any{"name": "build"}))
	waitForPhase(t, ls, "m1", "p1")

	snap := ls.Mission("m1")
	p := snap.Phases["p1"]
	if p == nil {
		t.Fatal("expected phase p1, got nil")
	}
	if p.Status != "running" {
		t.Fatalf("expected running, got %q", p.Status)
	}
	if p.Name != "build" {
		t.Fatalf("expected name build, got %q", p.Name)
	}

	b.Publish(New(PhaseCompleted, "m1", "p1", "", map[string]any{"output_len": 42}))
	waitForPhaseStatus(t, ls, "m1", "p1", "completed")

	if p2 := ls.Mission("m1").Phases["p1"]; p2.Status != "completed" {
		t.Fatalf("expected completed, got %q", p2.Status)
	}
}

func TestLiveState_PhaseFailed(t *testing.T) {
	b := NewBus()
	ls := NewLiveState(b)
	defer ls.Close()

	b.Publish(New(MissionStarted, "m1", "", "", nil))
	waitForSnap(t, ls, "m1")
	b.Publish(New(PhaseStarted, "m1", "p1", "", map[string]any{"name": "compile"}))
	waitForPhase(t, ls, "m1", "p1")
	b.Publish(New(PhaseFailed, "m1", "p1", "", map[string]any{"error": "exit 1"}))
	waitForPhaseStatus(t, ls, "m1", "p1", "failed")

	if p := ls.Mission("m1").Phases["p1"]; p.Status != "failed" {
		t.Fatalf("expected failed, got %q", p.Status)
	}
}

func TestLiveState_PhaseSkipped(t *testing.T) {
	b := NewBus()
	ls := NewLiveState(b)
	defer ls.Close()

	b.Publish(New(MissionStarted, "m1", "", "", nil))
	waitForSnap(t, ls, "m1")
	b.Publish(New(PhaseSkipped, "m1", "p2", "", map[string]any{"reason": "dep failed"}))
	waitForPhase(t, ls, "m1", "p2")

	if p := ls.Mission("m1").Phases["p2"]; p.Status != "skipped" {
		t.Fatalf("expected skipped, got %q", p.Status)
	}
}

func TestLiveState_UnknownMission(t *testing.T) {
	b := NewBus()
	ls := NewLiveState(b)
	defer ls.Close()

	if snap := ls.Mission("nope"); snap != nil {
		t.Fatalf("expected nil for unknown mission, got %+v", snap)
	}
}

func TestLiveState_SnapshotIsCopy(t *testing.T) {
	b := NewBus()
	ls := NewLiveState(b)
	defer ls.Close()

	b.Publish(New(MissionStarted, "m1", "", "", nil))
	waitForSnap(t, ls, "m1")

	snap1 := ls.Mission("m1")
	snap1.Status = "mutated"

	snap2 := ls.Mission("m1")
	if snap2.Status != "in_progress" {
		t.Fatalf("expected in_progress after external mutation, got %q", snap2.Status)
	}
}

func TestLiveState_LateJoin(t *testing.T) {
	// Phase events arrive before mission.started (late-join / out-of-order).
	b := NewBus()
	ls := NewLiveState(b)
	defer ls.Close()

	b.Publish(New(PhaseStarted, "m1", "p1", "", map[string]any{"name": "deploy"}))
	waitForPhase(t, ls, "m1", "p1")

	snap := ls.Mission("m1")
	if snap == nil {
		t.Fatal("expected synthesised mission snap after late-join phase event")
	}
	if snap.Status != "in_progress" {
		t.Fatalf("expected in_progress for synthesised mission, got %q", snap.Status)
	}
}

func TestLiveState_EmptyPhaseIDIgnored(t *testing.T) {
	b := NewBus()
	ls := NewLiveState(b)
	defer ls.Close()

	b.Publish(New(MissionStarted, "m1", "", "", nil))
	waitForSnap(t, ls, "m1")

	// PhaseStarted with no PhaseID must not panic or add a nil/empty key.
	b.Publish(New(PhaseStarted, "m1", "", "", map[string]any{"name": "orphan"}))

	// Drain: publish a sentinel event and wait for it — guarantees the
	// consumer goroutine has processed the preceding (bad) event.
	b.Publish(New(MissionCompleted, "m1", "", "", nil))
	waitForStatus(t, ls, "m1", "completed")

	snap := ls.Mission("m1")
	if _, ok := snap.Phases[""]; ok {
		t.Fatal("empty phase ID must not be stored in Phases map")
	}
}

func TestLiveState_Close(t *testing.T) {
	b := NewBus()
	ls := NewLiveState(b)
	ls.Close()

	// After Close, the subscription must be gone — Publish must not panic.
	b.Publish(New(MissionStarted, "m1", "", "", nil))
}

func TestLiveState_CloseIdempotent(t *testing.T) {
	b := NewBus()
	ls := NewLiveState(b)
	ls.Close()
	// Second call must not panic.
	ls.Close()
}

func TestLiveState_PhaseRetrying(t *testing.T) {
	b := NewBus()
	ls := NewLiveState(b)
	defer ls.Close()

	b.Publish(New(MissionStarted, "m1", "", "", nil))
	waitForSnap(t, ls, "m1")
	b.Publish(New(PhaseStarted, "m1", "p1", "", map[string]any{"name": "build"}))
	waitForPhase(t, ls, "m1", "p1")

	b.Publish(New(PhaseRetrying, "m1", "p1", "", map[string]any{"attempt": 2}))
	waitForPhaseStatus(t, ls, "m1", "p1", "retrying")

	p := ls.Mission("m1").Phases["p1"]
	if p.Status != "retrying" {
		t.Fatalf("expected retrying, got %q", p.Status)
	}

	// Phase can still transition to completed after retrying.
	b.Publish(New(PhaseCompleted, "m1", "p1", "", nil))
	waitForPhaseStatus(t, ls, "m1", "p1", "completed")
}

func TestLiveState_PhaseRetryingLateJoin(t *testing.T) {
	// PhaseRetrying arrives without a prior PhaseStarted — must synthesise mission.
	b := NewBus()
	ls := NewLiveState(b)
	defer ls.Close()

	b.Publish(New(PhaseRetrying, "m2", "p1", "", nil))
	waitForPhase(t, ls, "m2", "p1")

	p := ls.Mission("m2").Phases["p1"]
	if p.Status != "retrying" {
		t.Fatalf("expected retrying for late-join, got %q", p.Status)
	}
}

func TestLiveState_DuplicateMissionStartedPreservesPhases(t *testing.T) {
	// Regression: a second mission.started (at-least-once delivery) must not
	// overwrite the snap with an empty Phases map.
	b := NewBus()
	ls := NewLiveState(b)
	defer ls.Close()

	b.Publish(New(MissionStarted, "m1", "", "", nil))
	waitForSnap(t, ls, "m1")

	b.Publish(New(PhaseStarted, "m1", "p1", "", map[string]any{"name": "build"}))
	waitForPhase(t, ls, "m1", "p1")

	// Second mission.started — duplicate due to at-least-once delivery.
	b.Publish(New(MissionStarted, "m1", "", "", nil))

	// Drain: a sentinel phase lets us confirm the duplicate was consumed.
	b.Publish(New(PhaseStarted, "m1", "p2", "", map[string]any{"name": "test"}))
	waitForPhase(t, ls, "m1", "p2")

	snap := ls.Mission("m1")
	if _, ok := snap.Phases["p1"]; !ok {
		t.Fatal("duplicate mission.started must not wipe previously-projected phase p1")
	}
	if snap.Status != "in_progress" {
		t.Fatalf("expected in_progress after duplicate started, got %q", snap.Status)
	}
}

func TestLiveState_PhaseCompletedAfterMissionCompleted(t *testing.T) {
	// Regression: a phase.completed event that arrives after mission.completed
	// (out-of-order / late delivery) must update the phase snapshot without
	// corrupting the mission's terminal status.
	b := NewBus()
	ls := NewLiveState(b)
	defer ls.Close()

	b.Publish(New(MissionStarted, "m1", "", "", nil))
	waitForSnap(t, ls, "m1")
	b.Publish(New(PhaseStarted, "m1", "p1", "", map[string]any{"name": "build"}))
	waitForPhase(t, ls, "m1", "p1")

	b.Publish(New(MissionCompleted, "m1", "", "", nil))
	waitForStatus(t, ls, "m1", "completed")

	// Phase terminal event arrives late (background goroutine still flushing).
	b.Publish(New(PhaseCompleted, "m1", "p1", "", nil))
	waitForPhaseStatus(t, ls, "m1", "p1", "completed")

	snap := ls.Mission("m1")
	if snap.Status != "completed" {
		t.Fatalf("mission status must remain completed after late phase event; got %q", snap.Status)
	}
	if p := snap.Phases["p1"]; p == nil || p.Status != "completed" {
		t.Fatalf("phase p1 must be completed after late phase.completed event; got %+v", p)
	}
}

func TestLiveState_PhaseFailedAfterMissionCancelled(t *testing.T) {
	// A phase.failed event arriving after mission.cancelled (typical cancellation
	// flow: event loop exits, running goroutines emit PhaseFailed in background)
	// must update the phase without overwriting the mission's cancelled status.
	b := NewBus()
	ls := NewLiveState(b)
	defer ls.Close()

	b.Publish(New(MissionStarted, "m1", "", "", nil))
	waitForSnap(t, ls, "m1")
	b.Publish(New(PhaseStarted, "m1", "p1", "", map[string]any{"name": "deploy"}))
	waitForPhase(t, ls, "m1", "p1")

	b.Publish(New(MissionCancelled, "m1", "", "", nil))
	waitForStatus(t, ls, "m1", "cancelled")

	// Background goroutine emits PhaseFailed after the event loop has exited.
	b.Publish(New(PhaseFailed, "m1", "p1", "", map[string]any{"error": "context cancelled"}))
	waitForPhaseStatus(t, ls, "m1", "p1", "failed")

	snap := ls.Mission("m1")
	if snap.Status != "cancelled" {
		t.Fatalf("mission status must remain cancelled after late phase.failed; got %q", snap.Status)
	}
	if p := snap.Phases["p1"]; p == nil || p.Status != "failed" {
		t.Fatalf("phase p1 must be failed after late phase.failed event; got %+v", p)
	}
}

func TestLiveState_PhaseSkippedAfterMissionCancelled(t *testing.T) {
	// phase.skipped events emitted by the cancellation path (for never-dispatched
	// phases) arrive after mission.cancelled. All must be reflected correctly.
	b := NewBus()
	ls := NewLiveState(b)
	defer ls.Close()

	b.Publish(New(MissionStarted, "m1", "", "", nil))
	waitForSnap(t, ls, "m1")

	b.Publish(New(MissionCancelled, "m1", "", "", nil))
	waitForStatus(t, ls, "m1", "cancelled")

	// Never-dispatched phases emit skipped events after mission terminal.
	b.Publish(New(PhaseSkipped, "m1", "p1", "", map[string]any{"reason": "mission cancelled"}))
	b.Publish(New(PhaseSkipped, "m1", "p2", "", map[string]any{"reason": "mission cancelled"}))

	// Drain by waiting for both phases.
	waitForPhase(t, ls, "m1", "p1")
	waitForPhase(t, ls, "m1", "p2")

	snap := ls.Mission("m1")
	if snap.Status != "cancelled" {
		t.Fatalf("mission status must remain cancelled; got %q", snap.Status)
	}
	for _, id := range []string{"p1", "p2"} {
		p := snap.Phases[id]
		if p == nil || p.Status != "skipped" {
			t.Errorf("phase %q must be skipped; got %+v", id, p)
		}
	}
}

// ---- helpers ---------------------------------------------------------------

func waitForSnap(t *testing.T, ls *LiveState, missionID string) {
	t.Helper()
	deadline := time.Now().Add(200 * time.Millisecond)
	for time.Now().Before(deadline) {
		if ls.Mission(missionID) != nil {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for mission snap %q", missionID)
}

func waitForStatus(t *testing.T, ls *LiveState, missionID, want string) {
	t.Helper()
	deadline := time.Now().Add(200 * time.Millisecond)
	for time.Now().Before(deadline) {
		if snap := ls.Mission(missionID); snap != nil && snap.Status == want {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for mission %q status %q", missionID, want)
}

func waitForPhase(t *testing.T, ls *LiveState, missionID, phaseID string) {
	t.Helper()
	deadline := time.Now().Add(200 * time.Millisecond)
	for time.Now().Before(deadline) {
		if snap := ls.Mission(missionID); snap != nil {
			if _, ok := snap.Phases[phaseID]; ok {
				return
			}
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for phase %q in mission %q", phaseID, missionID)
}

func waitForPhaseStatus(t *testing.T, ls *LiveState, missionID, phaseID, want string) {
	t.Helper()
	deadline := time.Now().Add(200 * time.Millisecond)
	for time.Now().Before(deadline) {
		if snap := ls.Mission(missionID); snap != nil {
			if p, ok := snap.Phases[phaseID]; ok && p.Status == want {
				return
			}
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("timed out waiting for phase %q status %q in mission %q", phaseID, want, missionID)
}
