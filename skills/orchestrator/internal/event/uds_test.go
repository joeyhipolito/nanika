package event

import (
	"context"
	"net"
	"os"
	"path/filepath"
	"testing"
)

// shortTempSock returns a Unix socket path short enough for macOS (≤104 chars).
func shortTempSock(t *testing.T, name string) string {
	t.Helper()
	dir, err := os.MkdirTemp("/tmp", "evt")
	if err != nil {
		t.Fatalf("MkdirTemp: %v", err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	return filepath.Join(dir, name)
}

// TestUDSEmitter_DroppedWritesZeroOnSuccess verifies DroppedWrites stays zero
// when all events are delivered to a reachable daemon.
func TestUDSEmitter_DroppedWritesZeroOnSuccess(t *testing.T) {
	sockPath := shortTempSock(t, "d.sock")

	// Start a minimal Unix domain socket listener.
	ln, err := net.Listen("unix", sockPath)
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer ln.Close()

	// Accept and discard data in background.
	go func() {
		conn, err := ln.Accept()
		if err != nil {
			return
		}
		defer conn.Close()
		buf := make([]byte, 4096)
		for {
			if _, err := conn.Read(buf); err != nil {
				return
			}
		}
	}()

	u := NewUDSEmitter(sockPath)
	defer u.Close()

	u.Emit(context.Background(), New(MissionStarted, "m1", "", "", nil))
	u.Emit(context.Background(), New(MissionCompleted, "m1", "", "", nil))

	if got := u.DroppedWrites(); got != 0 {
		t.Fatalf("DroppedWrites() = %d after successful delivery; want 0", got)
	}
}

// TestUDSEmitter_DroppedWritesOnDialFailure verifies DroppedWrites increments
// for each event emitted when the daemon socket does not exist.
func TestUDSEmitter_DroppedWritesOnDialFailure(t *testing.T) {
	sockPath := filepath.Join(t.TempDir(), "nonexistent.sock") // long path is fine; no bind
	// Ensure path does not exist.
	os.Remove(sockPath)

	u := NewUDSEmitter(sockPath)
	defer u.Close()

	const n = 3
	for i := 0; i < n; i++ {
		u.Emit(context.Background(), New(MissionStarted, "m1", "", "", nil))
	}

	if got := u.DroppedWrites(); got != int64(n) {
		t.Fatalf("DroppedWrites() = %d after %d dial failures; want %d", got, n, n)
	}
}

// TestUDSEmitter_DroppedWritesOnWriteFailure verifies DroppedWrites increments
// when the listener closes mid-stream and subsequent writes fail.
func TestUDSEmitter_DroppedWritesOnWriteFailure(t *testing.T) {
	sockPath := shortTempSock(t, "d.sock")

	ln, err := net.Listen("unix", sockPath)
	if err != nil {
		t.Fatalf("listen: %v", err)
	}

	// Accept once, then immediately close the connection.
	accepted := make(chan struct{})
	go func() {
		conn, err := ln.Accept()
		if err != nil {
			return
		}
		conn.Close() // close right after accepting — next write will fail
		close(accepted)
	}()

	u := NewUDSEmitter(sockPath)
	defer u.Close()

	// First Emit establishes the connection (no drop yet).
	u.Emit(context.Background(), New(MissionStarted, "m1", "", "", nil))

	// Wait for the server side to close the connection.
	<-accepted
	ln.Close()

	// Subsequent emit should fail the write and increment the counter.
	u.Emit(context.Background(), New(MissionCompleted, "m1", "", "", nil))

	if got := u.DroppedWrites(); got == 0 {
		t.Fatal("DroppedWrites() = 0; expected > 0 after write to closed connection")
	}
}

// TestUDSEmitter_DroppedWritesAccumulate verifies the counter accumulates
// over multiple failed emits.
func TestUDSEmitter_DroppedWritesAccumulate(t *testing.T) {
	sockPath := filepath.Join(t.TempDir(), "nonexistent.sock") // long path is fine; no bind
	os.Remove(sockPath)

	u := NewUDSEmitter(sockPath)
	defer u.Close()

	const n = 10
	for i := 0; i < n; i++ {
		u.Emit(context.Background(), New(MissionStarted, "m1", "", "", nil))
	}

	if got := u.DroppedWrites(); got < int64(n) {
		t.Fatalf("DroppedWrites() = %d after %d failed emits; want >= %d", got, n, n)
	}
}
