package event

import (
	"context"
	"path/filepath"
	"testing"
)

func TestFileEmitter_DroppedWritesZeroOnSuccess(t *testing.T) {
	path := filepath.Join(t.TempDir(), "events.jsonl")
	fe, err := NewFileEmitter(path)
	if err != nil {
		t.Fatalf("NewFileEmitter: %v", err)
	}
	defer fe.Close()

	fe.Emit(context.Background(), New(MissionStarted, "m1", "", "", nil))
	fe.Emit(context.Background(), New(PhaseStarted, "m1", "p1", "", map[string]any{"name": "build"}))
	fe.Emit(context.Background(), New(PhaseCompleted, "m1", "p1", "", nil))
	fe.Emit(context.Background(), New(MissionCompleted, "m1", "", "", nil))

	if got := fe.DroppedWrites(); got != 0 {
		t.Fatalf("DroppedWrites() = %d after successful writes; want 0", got)
	}
}

func TestFileEmitter_DroppedWritesIncrementAfterClose(t *testing.T) {
	// Close the underlying file to trigger write failures, then verify that
	// DroppedWrites() increments for each event that cannot be persisted.
	path := filepath.Join(t.TempDir(), "events.jsonl")
	fe, err := NewFileEmitter(path)
	if err != nil {
		t.Fatalf("NewFileEmitter: %v", err)
	}

	// One successful write before close.
	fe.Emit(context.Background(), New(MissionStarted, "m1", "", "", nil))
	if got := fe.DroppedWrites(); got != 0 {
		t.Fatalf("before close: DroppedWrites() = %d; want 0", got)
	}

	// Close the file; subsequent writes must fail.
	if err := fe.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	fe.Emit(context.Background(), New(MissionCompleted, "m1", "", "", nil))
	if got := fe.DroppedWrites(); got == 0 {
		t.Fatal("DroppedWrites() should be > 0 after write to closed file")
	}
}

func TestFileEmitter_DroppedWritesAccumulate(t *testing.T) {
	path := filepath.Join(t.TempDir(), "events.jsonl")
	fe, err := NewFileEmitter(path)
	if err != nil {
		t.Fatalf("NewFileEmitter: %v", err)
	}

	if err := fe.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	const n = 5
	for i := 0; i < n; i++ {
		fe.Emit(context.Background(), New(MissionStarted, "m1", "", "", nil))
	}

	if got := fe.DroppedWrites(); got < int64(n) {
		t.Fatalf("DroppedWrites() = %d after %d failed emits; want >= %d", got, n, n)
	}
}
