package engine

// Tests for file-overlap detection between parallel phases.
//
// detectFileOverlaps is a pure function over phase.ChangedFiles — no real git
// repository or worker execution is needed. Tests pre-populate ChangedFiles
// on phases and assert that the correct file_overlap.detected events are emitted.

import (
	"context"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

// makeOverlapEngine builds a minimal Engine with a captureEmitter suitable
// for calling detectFileOverlaps.
func makeOverlapEngine(ws *core.Workspace, phases []*core.Phase) (*Engine, *captureEmitter) {
	em := &captureEmitter{}
	e := &Engine{
		workspace: ws,
		config:    &core.OrchestratorConfig{},
		emitter:   em,
		phases:    make(map[string]*core.Phase, len(phases)),
	}
	for _, p := range phases {
		e.phases[p.ID] = p
	}
	return e, em
}

// overlapEvents filters captured events to only file_overlap.detected events.
func overlapEvents(em *captureEmitter) []event.Event {
	var out []event.Event
	for _, ev := range em.collected() {
		if ev.Type == event.FileOverlapDetected {
			out = append(out, ev)
		}
	}
	return out
}

// ---------------------------------------------------------------------------
// No-overlap cases
// ---------------------------------------------------------------------------

func TestDetectFileOverlaps_NoOverlap(t *testing.T) {
	phases := []*core.Phase{
		{
			ID:           "phase-a",
			Name:         "impl-a",
			Status:       core.StatusCompleted,
			ChangedFiles: []string{"foo/bar.go", "foo/baz.go"},
		},
		{
			ID:           "phase-b",
			Name:         "impl-b",
			Status:       core.StatusCompleted,
			ChangedFiles: []string{"other/qux.go"},
		},
	}
	ws := &core.Workspace{ID: "ws-1"} // no BaseBranch → statusMap stays nil
	e, em := makeOverlapEngine(ws, phases)

	e.detectFileOverlaps(context.Background(), phases)

	if got := overlapEvents(em); len(got) != 0 {
		t.Errorf("expected 0 overlap events, got %d", len(got))
	}
}

func TestDetectFileOverlaps_SinglePhase(t *testing.T) {
	phases := []*core.Phase{
		{
			ID:           "phase-a",
			Name:         "impl-a",
			Status:       core.StatusCompleted,
			ChangedFiles: []string{"foo/bar.go"},
		},
	}
	ws := &core.Workspace{ID: "ws-1"}
	e, em := makeOverlapEngine(ws, phases)

	e.detectFileOverlaps(context.Background(), phases)

	if got := overlapEvents(em); len(got) != 0 {
		t.Errorf("expected 0 overlap events for a single phase, got %d", len(got))
	}
}

func TestDetectFileOverlaps_EmptyChangedFiles(t *testing.T) {
	phases := []*core.Phase{
		{ID: "phase-a", Name: "impl-a", Status: core.StatusCompleted, ChangedFiles: nil},
		{ID: "phase-b", Name: "impl-b", Status: core.StatusCompleted, ChangedFiles: nil},
	}
	ws := &core.Workspace{ID: "ws-1"}
	e, em := makeOverlapEngine(ws, phases)

	e.detectFileOverlaps(context.Background(), phases)

	if got := overlapEvents(em); len(got) != 0 {
		t.Errorf("expected 0 events when ChangedFiles is empty, got %d", len(got))
	}
}

func TestDetectFileOverlaps_FailedPhasesIgnored(t *testing.T) {
	phases := []*core.Phase{
		{
			ID:           "phase-a",
			Name:         "impl-a",
			Status:       core.StatusCompleted,
			ChangedFiles: []string{"shared/config.go"},
		},
		{
			// Failed phase — must not contribute to overlap detection.
			ID:           "phase-b",
			Name:         "impl-b",
			Status:       core.StatusFailed,
			ChangedFiles: []string{"shared/config.go"},
		},
	}
	ws := &core.Workspace{ID: "ws-1"}
	e, em := makeOverlapEngine(ws, phases)

	e.detectFileOverlaps(context.Background(), phases)

	if got := overlapEvents(em); len(got) != 0 {
		t.Errorf("expected 0 events when overlap is only with a failed phase, got %d", len(got))
	}
}

// ---------------------------------------------------------------------------
// Overlap detected cases
// ---------------------------------------------------------------------------

func TestDetectFileOverlaps_SingleOverlap(t *testing.T) {
	phases := []*core.Phase{
		{
			ID:           "phase-a",
			Name:         "impl-a",
			Status:       core.StatusCompleted,
			ChangedFiles: []string{"internal/engine/engine.go", "internal/engine/engine_test.go"},
		},
		{
			ID:           "phase-b",
			Name:         "impl-b",
			Status:       core.StatusCompleted,
			ChangedFiles: []string{"internal/engine/engine.go", "internal/core/types.go"},
		},
	}
	ws := &core.Workspace{ID: "ws-1"}
	e, em := makeOverlapEngine(ws, phases)

	e.detectFileOverlaps(context.Background(), phases)

	events := overlapEvents(em)
	if len(events) != 1 {
		t.Fatalf("expected 1 overlap event, got %d", len(events))
	}

	ev := events[0]
	if got := ev.Data["file"]; got != "internal/engine/engine.go" {
		t.Errorf("overlap file = %q, want %q", got, "internal/engine/engine.go")
	}
	phases_, ok := ev.Data["phases"].([]string)
	if !ok {
		t.Fatalf("phases field is not []string: %T", ev.Data["phases"])
	}
	if len(phases_) != 2 {
		t.Errorf("expected 2 phases in overlap event, got %d", len(phases_))
	}
}

func TestDetectFileOverlaps_MultipleOverlappingFiles(t *testing.T) {
	phases := []*core.Phase{
		{
			ID:           "phase-a",
			Name:         "backend",
			Status:       core.StatusCompleted,
			ChangedFiles: []string{"api/handler.go", "api/types.go", "api/middleware.go"},
		},
		{
			ID:           "phase-b",
			Name:         "frontend",
			Status:       core.StatusCompleted,
			ChangedFiles: []string{"api/handler.go", "api/types.go", "ui/app.tsx"},
		},
	}
	ws := &core.Workspace{ID: "ws-1"}
	e, em := makeOverlapEngine(ws, phases)

	e.detectFileOverlaps(context.Background(), phases)

	events := overlapEvents(em)
	if len(events) != 2 {
		t.Fatalf("expected 2 overlap events (one per conflicting file), got %d", len(events))
	}

	// Collect the reported files.
	reported := make(map[string]bool)
	for _, ev := range events {
		if f, ok := ev.Data["file"].(string); ok {
			reported[f] = true
		}
	}
	for _, wantFile := range []string{"api/handler.go", "api/types.go"} {
		if !reported[wantFile] {
			t.Errorf("expected overlap event for %q, but it was not reported", wantFile)
		}
	}
}

func TestDetectFileOverlaps_ThreePhaseOverlap(t *testing.T) {
	// Three phases all touch the same file.
	phases := []*core.Phase{
		{
			ID:           "phase-a",
			Name:         "impl-a",
			Status:       core.StatusCompleted,
			ChangedFiles: []string{"go.mod"},
		},
		{
			ID:           "phase-b",
			Name:         "impl-b",
			Status:       core.StatusCompleted,
			ChangedFiles: []string{"go.mod"},
		},
		{
			ID:           "phase-c",
			Name:         "impl-c",
			Status:       core.StatusCompleted,
			ChangedFiles: []string{"go.mod"},
		},
	}
	ws := &core.Workspace{ID: "ws-1"}
	e, em := makeOverlapEngine(ws, phases)

	e.detectFileOverlaps(context.Background(), phases)

	events := overlapEvents(em)
	if len(events) != 1 {
		t.Fatalf("expected 1 overlap event for a file touched by 3 phases, got %d", len(events))
	}

	phases_, ok := events[0].Data["phases"].([]string)
	if !ok {
		t.Fatalf("phases field is not []string: %T", events[0].Data["phases"])
	}
	if len(phases_) != 3 {
		t.Errorf("expected 3 phases in overlap event, got %d: %v", len(phases_), phases_)
	}
}

// ---------------------------------------------------------------------------
// Severity
// ---------------------------------------------------------------------------

func TestDetectFileOverlaps_SeverityDefaultsMedium(t *testing.T) {
	// When no BaseBranch is set, statusMap is nil and severity defaults to "medium".
	phases := []*core.Phase{
		{
			ID:           "phase-a",
			Status:       core.StatusCompleted,
			ChangedFiles: []string{"pkg/store.go"},
		},
		{
			ID:           "phase-b",
			Status:       core.StatusCompleted,
			ChangedFiles: []string{"pkg/store.go"},
		},
	}
	ws := &core.Workspace{ID: "ws-1"} // no BaseBranch
	e, em := makeOverlapEngine(ws, phases)

	e.detectFileOverlaps(context.Background(), phases)

	events := overlapEvents(em)
	if len(events) != 1 {
		t.Fatalf("expected 1 event, got %d", len(events))
	}
	if got := events[0].Data["severity"]; got != "medium" {
		t.Errorf("severity = %q, want %q when no base branch is configured", got, "medium")
	}
}

func TestDetectFileOverlaps_EventContainsRequiredFields(t *testing.T) {
	phases := []*core.Phase{
		{
			ID:           "phase-x",
			Name:         "worker-x",
			Status:       core.StatusCompleted,
			ChangedFiles: []string{"cmd/main.go"},
		},
		{
			ID:           "phase-y",
			Name:         "worker-y",
			Status:       core.StatusCompleted,
			ChangedFiles: []string{"cmd/main.go"},
		},
	}
	ws := &core.Workspace{ID: "ws-2"}
	e, em := makeOverlapEngine(ws, phases)

	e.detectFileOverlaps(context.Background(), phases)

	events := overlapEvents(em)
	if len(events) != 1 {
		t.Fatalf("expected 1 event, got %d", len(events))
	}

	ev := events[0]

	if _, ok := ev.Data["file"]; !ok {
		t.Error("overlap event missing 'file' field")
	}
	if _, ok := ev.Data["phases"]; !ok {
		t.Error("overlap event missing 'phases' field")
	}
	if _, ok := ev.Data["severity"]; !ok {
		t.Error("overlap event missing 'severity' field")
	}
	if ev.Type != event.FileOverlapDetected {
		t.Errorf("event type = %q, want %q", ev.Type, event.FileOverlapDetected)
	}
}

// ---------------------------------------------------------------------------
// recordChangedFiles — unit test with a real git repo
// ---------------------------------------------------------------------------

func TestRecordChangedFiles_NoTargetDir(t *testing.T) {
	// When TargetDir is empty, recordChangedFiles must be a no-op.
	phase := &core.Phase{ID: "p1", Name: "impl", TargetDir: ""}
	ws := &core.Workspace{BaseBranch: "main"}
	e, _ := makeOverlapEngine(ws, []*core.Phase{phase})

	e.recordChangedFiles(phase)

	if len(phase.ChangedFiles) != 0 {
		t.Errorf("expected no ChangedFiles when TargetDir is empty, got %v", phase.ChangedFiles)
	}
}

func TestRecordChangedFiles_NoBaseBranch(t *testing.T) {
	// When BaseBranch is empty, recordChangedFiles must be a no-op.
	phase := &core.Phase{ID: "p1", Name: "impl", TargetDir: t.TempDir()}
	ws := &core.Workspace{BaseBranch: ""}
	e, _ := makeOverlapEngine(ws, []*core.Phase{phase})

	e.recordChangedFiles(phase)

	if len(phase.ChangedFiles) != 0 {
		t.Errorf("expected no ChangedFiles when BaseBranch is empty, got %v", phase.ChangedFiles)
	}
}
