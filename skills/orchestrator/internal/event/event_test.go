package event_test

import (
	"bytes"
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

// allEventTypes lists all typed constants for exhaustiveness checks.
// When adding a new EventType constant to types.go, add it here too —
// TestAllEventTypesCount will catch the mismatch immediately.
var allEventTypes = []event.EventType{
	event.MissionStarted, event.MissionCompleted, event.MissionFailed, event.MissionCancelled,
	event.PhaseStarted, event.PhaseCompleted, event.PhaseFailed, event.PhaseSkipped, event.PhaseRetrying,
	event.WorkerSpawned, event.WorkerOutput, event.WorkerCompleted, event.WorkerFailed,
	event.DecomposeStarted, event.DecomposeCompleted, event.DecomposeFallback,
	event.LearningExtracted, event.LearningStored, event.LearningInjected,
	event.DAGDependencyResolved, event.DAGPhaseDispatched,
	event.RoleHandoff,
	event.ContractValidated, event.ContractViolated,
	event.PersonaContractViolation,
	event.SystemError, event.SystemCheckpointSaved,
	event.ZettelWritten, event.ZettelSkipped, event.ZettelWriteFailed,
}

func TestAllEventTypesCount(t *testing.T) {
	if len(allEventTypes) != 30 {
		t.Fatalf("expected 30 event types, got %d — add new constants to allEventTypes", len(allEventTypes))
	}
}

func TestAllEventTypesUnique(t *testing.T) {
	seen := make(map[event.EventType]bool, len(allEventTypes))
	for _, typ := range allEventTypes {
		if typ == "" {
			t.Error("found empty event type constant")
		}
		if seen[typ] {
			t.Errorf("duplicate event type: %s", typ)
		}
		seen[typ] = true
	}
}

func TestNewEventFields(t *testing.T) {
	ev := event.New(event.MissionStarted, "m1", "p1", "w1", map[string]any{"k": "v"})

	if ev.ID == "" {
		t.Error("expected non-empty ID")
	}
	if ev.Type != event.MissionStarted {
		t.Errorf("type: got %s, want %s", ev.Type, event.MissionStarted)
	}
	if ev.Timestamp.IsZero() {
		t.Error("expected non-zero timestamp")
	}
	if ev.Sequence != 0 {
		t.Error("sequence should be zero before emitting")
	}
	if ev.MissionID != "m1" {
		t.Errorf("mission_id: got %s, want m1", ev.MissionID)
	}
	if ev.PhaseID != "p1" {
		t.Errorf("phase_id: got %s, want p1", ev.PhaseID)
	}
	if ev.WorkerID != "w1" {
		t.Errorf("worker_id: got %s, want w1", ev.WorkerID)
	}
	if ev.Data["k"] != "v" {
		t.Error("data not preserved")
	}
}

func TestNewEventIDsDistinct(t *testing.T) {
	ids := make(map[string]bool, 100)
	for i := 0; i < 100; i++ {
		ev := event.New(event.MissionStarted, "m", "", "", nil)
		if ids[ev.ID] {
			t.Fatalf("duplicate event ID: %s", ev.ID)
		}
		ids[ev.ID] = true
	}
}

// --- NoOpEmitter ---

func TestNoOpEmitter(t *testing.T) {
	var em event.NoOpEmitter
	em.Emit(context.Background(), event.New(event.MissionStarted, "m1", "", "", nil))
	if err := em.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
}

// --- FileEmitter ---

func TestFileEmitterWritesJSONL(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "events.jsonl")

	fe, err := event.NewFileEmitter(path)
	if err != nil {
		t.Fatalf("NewFileEmitter: %v", err)
	}

	ctx := context.Background()
	fe.Emit(ctx, event.New(event.MissionStarted, "m1", "", "", nil))
	fe.Emit(ctx, event.New(event.PhaseStarted, "m1", "phase-1", "", nil))
	fe.Emit(ctx, event.New(event.PhaseCompleted, "m1", "phase-1", "", map[string]any{"output_len": 42}))

	if err := fe.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("ReadFile: %v", err)
	}

	lines := splitLines(data)
	if len(lines) != 3 {
		t.Fatalf("expected 3 lines, got %d", len(lines))
	}

	var evs [3]event.Event
	for i, line := range lines {
		if err := json.Unmarshal(line, &evs[i]); err != nil {
			t.Fatalf("line %d unmarshal: %v", i, err)
		}
	}

	// Sequences must be 1, 2, 3
	for i, ev := range evs {
		if ev.Sequence != int64(i+1) {
			t.Errorf("event %d: want sequence %d, got %d", i, i+1, ev.Sequence)
		}
	}

	// Types must round-trip
	wantTypes := []event.EventType{event.MissionStarted, event.PhaseStarted, event.PhaseCompleted}
	for i, ev := range evs {
		if ev.Type != wantTypes[i] {
			t.Errorf("event %d: want type %s, got %s", i, wantTypes[i], ev.Type)
		}
	}
}

func TestFileEmitterPermissions(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "events.jsonl")

	fe, err := event.NewFileEmitter(path)
	if err != nil {
		t.Fatalf("NewFileEmitter: %v", err)
	}
	fe.Emit(context.Background(), event.New(event.MissionStarted, "m1", "", "", nil))
	if err := fe.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	info, err := os.Stat(path)
	if err != nil {
		t.Fatalf("Stat: %v", err)
	}
	if perm := info.Mode().Perm(); perm != 0600 {
		t.Errorf("want 0600 permissions, got %04o", perm)
	}
}

func TestFileEmitterCreatesParentDir(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "nested", "deep", "events.jsonl")

	fe, err := event.NewFileEmitter(path)
	if err != nil {
		t.Fatalf("NewFileEmitter: %v", err)
	}
	if err := fe.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	// Parent directory should exist
	if _, err := os.Stat(filepath.Dir(path)); err != nil {
		t.Fatalf("parent dir not created: %v", err)
	}
}

func TestFileEmitterConcurrent(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "events.jsonl")

	fe, err := event.NewFileEmitter(path)
	if err != nil {
		t.Fatalf("NewFileEmitter: %v", err)
	}

	const n = 50
	done := make(chan struct{}, n)
	ctx := context.Background()
	for i := 0; i < n; i++ {
		go func() {
			fe.Emit(ctx, event.New(event.PhaseStarted, "m1", "p1", "", nil))
			done <- struct{}{}
		}()
	}
	for i := 0; i < n; i++ {
		<-done
	}

	if err := fe.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("ReadFile: %v", err)
	}

	lines := splitLines(data)
	if len(lines) != n {
		t.Errorf("want %d lines, got %d", n, len(lines))
	}

	// Every line must be valid JSON
	for i, line := range lines {
		var ev event.Event
		if err := json.Unmarshal(line, &ev); err != nil {
			t.Errorf("line %d: invalid JSON: %v", i, err)
		}
	}
}

// --- MultiEmitter ---

func TestMultiEmitterFanOut(t *testing.T) {
	dir := t.TempDir()
	path1 := filepath.Join(dir, "a.jsonl")
	path2 := filepath.Join(dir, "b.jsonl")

	fe1, err := event.NewFileEmitter(path1)
	if err != nil {
		t.Fatalf("fe1: %v", err)
	}
	fe2, err := event.NewFileEmitter(path2)
	if err != nil {
		t.Fatalf("fe2: %v", err)
	}

	multi := event.NewMultiEmitter(fe1, fe2)
	ctx := context.Background()
	multi.Emit(ctx, event.New(event.MissionStarted, "m1", "", "", nil))
	multi.Emit(ctx, event.New(event.PhaseStarted, "m1", "p1", "", nil))

	if err := multi.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	for _, path := range []string{path1, path2} {
		data, err := os.ReadFile(path)
		if err != nil {
			t.Fatalf("ReadFile %s: %v", path, err)
		}
		lines := splitLines(data)
		if len(lines) != 2 {
			t.Errorf("%s: want 2 lines, got %d", path, len(lines))
		}
	}
}

func TestMultiEmitterSequence(t *testing.T) {
	// MultiEmitter assigns its own sequence; children receive events with sequences already set.
	dir := t.TempDir()
	path := filepath.Join(dir, "events.jsonl")

	fe, err := event.NewFileEmitter(path)
	if err != nil {
		t.Fatalf("NewFileEmitter: %v", err)
	}

	multi := event.NewMultiEmitter(fe)
	ctx := context.Background()

	const n = 5
	for i := 0; i < n; i++ {
		multi.Emit(ctx, event.New(event.PhaseStarted, "m1", "p1", "", nil))
	}

	if err := multi.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("ReadFile: %v", err)
	}

	lines := splitLines(data)
	if len(lines) != n {
		t.Fatalf("want %d lines, got %d", n, len(lines))
	}

	for i, line := range lines {
		var ev event.Event
		if err := json.Unmarshal(line, &ev); err != nil {
			t.Fatalf("line %d: %v", i, err)
		}
		// File emitter's own counter should be set; sequences start from 1
		if ev.Sequence < 1 {
			t.Errorf("line %d: want sequence >= 1, got %d", i, ev.Sequence)
		}
	}
}

func TestMultiEmitterZeroEmitters(t *testing.T) {
	multi := event.NewMultiEmitter()
	multi.Emit(context.Background(), event.New(event.MissionStarted, "m1", "", "", nil))
	if err := multi.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
}

// --- Shared sequence across log + live consumers ---

// TestMultiEmitterSharedSequenceFileAndBus verifies that events written to the
// file log and delivered to bus subscribers carry the same sequence numbers —
// MultiEmitter is the sole source of truth.
func TestMultiEmitterSharedSequenceFileAndBus(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "events.jsonl")

	fe, err := event.NewFileEmitter(path)
	if err != nil {
		t.Fatalf("NewFileEmitter: %v", err)
	}

	bus := event.NewBus()
	subID, subCh := bus.Subscribe()
	defer bus.Unsubscribe(subID)

	multi := event.NewMultiEmitter(fe, event.NewBusEmitter(bus))
	ctx := context.Background()

	const n = 5
	for i := 0; i < n; i++ {
		multi.Emit(ctx, event.New(event.PhaseStarted, "m1", "p1", "", nil))
	}

	if err := multi.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	// Collect sequences received on the bus channel.
	busSeqs := make([]int64, 0, n)
	timeout := time.After(500 * time.Millisecond)
collect:
	for len(busSeqs) < n {
		select {
		case ev, ok := <-subCh:
			if !ok {
				break collect
			}
			busSeqs = append(busSeqs, ev.Sequence)
		case <-timeout:
			t.Fatalf("timeout: only received %d of %d events on bus", len(busSeqs), n)
		}
	}

	// Read sequences from the log file.
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("ReadFile: %v", err)
	}
	lines := splitLines(data)
	if len(lines) != n {
		t.Fatalf("expected %d lines in log, got %d", n, len(lines))
	}

	fileSeqs := make([]int64, n)
	for i, line := range lines {
		var ev event.Event
		if err := json.Unmarshal(line, &ev); err != nil {
			t.Fatalf("line %d unmarshal: %v", i, err)
		}
		fileSeqs[i] = ev.Sequence
	}

	// File and bus must agree on every sequence number.
	for i := 0; i < n; i++ {
		if fileSeqs[i] != busSeqs[i] {
			t.Errorf("event %d: file seq=%d, bus seq=%d (mismatch)", i, fileSeqs[i], busSeqs[i])
		}
		// Sequences must be 1, 2, ..., n.
		if fileSeqs[i] != int64(i+1) {
			t.Errorf("event %d: want sequence %d, got %d", i, i+1, fileSeqs[i])
		}
	}
}

// TestLastSequence verifies LastSequence returns the highest sequence found in
// an existing log, and 0 for a missing file.
func TestLastSequence(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "events.jsonl")

	// Missing file → 0, no error.
	seq, err := event.LastSequence(path)
	if err != nil {
		t.Fatalf("LastSequence on missing file: %v", err)
	}
	if seq != 0 {
		t.Fatalf("expected 0 for missing file, got %d", seq)
	}

	// Write 3 events via MultiEmitter.
	fe, err := event.NewFileEmitter(path)
	if err != nil {
		t.Fatalf("NewFileEmitter: %v", err)
	}
	multi := event.NewMultiEmitter(fe)
	ctx := context.Background()
	for i := 0; i < 3; i++ {
		multi.Emit(ctx, event.New(event.MissionStarted, "m1", "", "", nil))
	}
	if err := multi.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	seq, err = event.LastSequence(path)
	if err != nil {
		t.Fatalf("LastSequence: %v", err)
	}
	if seq != 3 {
		t.Fatalf("expected last sequence 3, got %d", seq)
	}
}

// TestMultiEmitterNoResetOnResume verifies that resuming a mission continues
// sequence numbering from where the previous session left off, with no gap
// or reset in the log file.
func TestMultiEmitterNoResetOnResume(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "events.jsonl")

	// --- Session 1: emit 3 events (sequences 1, 2, 3). ---
	fe1, err := event.NewFileEmitter(path)
	if err != nil {
		t.Fatalf("session1 NewFileEmitter: %v", err)
	}
	multi1 := event.NewMultiEmitter(fe1)
	ctx := context.Background()
	for i := 0; i < 3; i++ {
		multi1.Emit(ctx, event.New(event.PhaseStarted, "m1", "p1", "", nil))
	}
	if err := multi1.Close(); err != nil {
		t.Fatalf("session1 Close: %v", err)
	}

	// --- Resume: determine last sequence, open file for append. ---
	lastSeq, err := event.LastSequence(path)
	if err != nil {
		t.Fatalf("LastSequence: %v", err)
	}
	if lastSeq != 3 {
		t.Fatalf("expected lastSeq=3 after session 1, got %d", lastSeq)
	}

	// --- Session 2: emit 3 more events — sequences must be 4, 5, 6. ---
	fe2, err := event.NewFileEmitter(path)
	if err != nil {
		t.Fatalf("session2 NewFileEmitter: %v", err)
	}
	multi2 := event.NewMultiEmitterFromSeq(lastSeq, fe2)
	for i := 0; i < 3; i++ {
		multi2.Emit(ctx, event.New(event.PhaseCompleted, "m1", "p1", "", nil))
	}
	if err := multi2.Close(); err != nil {
		t.Fatalf("session2 Close: %v", err)
	}

	// --- Verify the log contains 6 events with sequences 1..6. ---
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("ReadFile: %v", err)
	}
	lines := splitLines(data)
	if len(lines) != 6 {
		t.Fatalf("expected 6 lines in log, got %d", len(lines))
	}

	for i, line := range lines {
		var ev event.Event
		if err := json.Unmarshal(line, &ev); err != nil {
			t.Fatalf("line %d unmarshal: %v", i, err)
		}
		want := int64(i + 1)
		if ev.Sequence != want {
			t.Errorf("line %d: want sequence %d, got %d (sequence reset on resume?)", i, want, ev.Sequence)
		}
	}
}

// --- Bus monotonic sequencing with pre-sequenced events ---

// TestBusPreSequencedHighWaterMark is a regression test for the case where a
// pre-sequenced event (ev.Sequence != 0) arrives before a zero-sequence event.
// TestBusAlwaysAssignsGlobalSequence verifies that the bus ignores any
// incoming Sequence and always assigns its own monotonic global counter.
// This prevents mission-local sequences from different MultiEmitters
// (each starting at 1) from colliding in the ring buffer and breaking
// SSE replay deduplication.
func TestBusAlwaysAssignsGlobalSequence(t *testing.T) {
	b := event.NewBus()

	// Publish an event whose mission-local sequence is 100 — the bus must
	// discard it and assign global seq=1 instead.
	preSeq := event.New(event.MissionStarted, "m1", "", "", nil)
	preSeq.Sequence = 100
	b.Publish(preSeq)

	// Publish a zero-sequence event — must get global seq=2, not 101.
	zeroSeq := event.New(event.PhaseStarted, "m1", "", "", nil)
	b.Publish(zeroSeq)

	events := b.EventsSince(0)
	if len(events) != 2 {
		t.Fatalf("expected 2 events, got %d", len(events))
	}
	if events[0].Sequence != 1 {
		t.Errorf("first event: want global seq 1, got %d", events[0].Sequence)
	}
	if events[1].Sequence != 2 {
		t.Errorf("second event: want global seq 2, got %d", events[1].Sequence)
	}
}

// TestBusMonotonicRegardlessOfInput verifies that the bus always produces
// strictly-increasing sequences regardless of the incoming Sequence value.
// Events with pre-assigned mission-local sequences (50, 200, …) must not
// "skip" the counter — they get 1, 2, 3, … just like zero-sequence events.
func TestBusMonotonicRegardlessOfInput(t *testing.T) {
	b := event.NewBus()

	publish := func(seq int64) {
		ev := event.New(event.PhaseStarted, "m1", "", "", nil)
		ev.Sequence = seq
		b.Publish(ev)
	}

	publish(0)
	publish(50)  // mission-local; bus ignores, assigns 2
	publish(0)
	publish(200) // mission-local; bus ignores, assigns 4
	publish(0)

	events := b.EventsSince(0)
	if len(events) != 5 {
		t.Fatalf("expected 5 events, got %d", len(events))
	}

	// All events get sequential global bus sequences 1..5.
	for i, ev := range events {
		want := int64(i + 1)
		if ev.Sequence != want {
			t.Errorf("event %d: want global seq %d, got %d", i, want, ev.Sequence)
		}
	}

	// Strict monotonic check.
	for i := 1; i < len(events); i++ {
		if events[i].Sequence <= events[i-1].Sequence {
			t.Errorf("not monotonic at index %d: seq[%d]=%d <= seq[%d]=%d",
				i, i, events[i].Sequence, i-1, events[i-1].Sequence)
		}
	}
}

// splitLines splits newline-terminated data, skipping empty lines.
func splitLines(data []byte) [][]byte {
	var lines [][]byte
	for _, line := range bytes.Split(data, []byte{'\n'}) {
		if len(bytes.TrimSpace(line)) > 0 {
			lines = append(lines, line)
		}
	}
	return lines
}
