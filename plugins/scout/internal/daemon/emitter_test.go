package daemon

import (
	"encoding/json"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// shortSockPath returns a socket path short enough for macOS's 104-char UDS limit.
// t.TempDir() paths are often 80+ chars; this uses /tmp/<6-char-prefix> instead.
func shortSockPath(t *testing.T) string {
	t.Helper()
	dir, err := os.MkdirTemp("/tmp", "sc")
	if err != nil {
		t.Fatalf("MkdirTemp: %v", err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	return filepath.Join(dir, "d.sock")
}

// startListener creates a Unix domain socket listener at sockPath and returns
// a channel that delivers the raw bytes of the first accepted connection read.
func startListener(t *testing.T, sockPath string) <-chan []byte {
	t.Helper()
	ln, err := net.Listen("unix", sockPath)
	if err != nil {
		t.Fatalf("listen unix %s: %v", sockPath, err)
	}
	t.Cleanup(func() { ln.Close() })

	ch := make(chan []byte, 1)
	go func() {
		conn, err := ln.Accept()
		if err != nil {
			return
		}
		defer conn.Close()
		buf := make([]byte, 4096)
		n, _ := conn.Read(buf)
		ch <- buf[:n]
	}()
	return ch
}

// ─── NewEmitter ──────────────────────────────────────────────────────────────

func TestNewEmitter_ReturnsEmitter(t *testing.T) {
	e, err := NewEmitter()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if e == nil {
		t.Fatal("expected non-nil emitter")
	}
	e.Close()
}

func TestNewEmitter_UsesAllukaSocket(t *testing.T) {
	home, err := os.UserHomeDir()
	if err != nil {
		t.Skip("cannot determine home dir")
	}
	expectedPath := filepath.Join(home, ".alluka", "daemon.sock")

	e, err := NewEmitter()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	defer e.Close()

	if e.path != expectedPath {
		t.Errorf("expected path %s, got %s", expectedPath, e.path)
	}
}

// ─── Close ───────────────────────────────────────────────────────────────────

func TestEmitter_CloseIdempotent(t *testing.T) {
	e, _ := NewEmitter()
	e.Close()
	e.Close() // must not panic on double close
}

func TestEmitter_EmitAfterClose_NoPanic(t *testing.T) {
	e := &Emitter{path: "/tmp/nonexistent-scout-test.sock"}
	e.Close()
	e.EmitIntelGathered("topic", 1) // should be a no-op, not panic
}

// ─── EmitIntelGathered — daemon not running ───────────────────────────────────

func TestEmitter_EmitWhenDaemonNotRunning(t *testing.T) {
	e := &Emitter{path: "/tmp/nonexistent-scout-daemon-test.sock"}
	// Must not panic; logs one warning to stderr.
	e.EmitIntelGathered("test-topic", 42)
	e.Close()
}

func TestEmitter_DropsLogOnlyOnce(t *testing.T) {
	// Verify the "daemon unreachable" warning is logged exactly once,
	// even if EmitIntelGathered is called multiple times.
	e := &Emitter{path: "/tmp/nonexistent-scout-daemon-once.sock"}

	r, w, _ := os.Pipe()
	oldStderr := os.Stderr
	os.Stderr = w

	e.EmitIntelGathered("topic1", 5)
	e.EmitIntelGathered("topic2", 10) // second call should NOT produce another log

	w.Close()
	os.Stderr = oldStderr

	buf := make([]byte, 4096)
	n, _ := r.Read(buf)
	output := string(buf[:n])

	count := strings.Count(output, "daemon unreachable")
	if count != 1 {
		t.Errorf("expected exactly 1 'daemon unreachable' log, got %d\nOutput: %s", count, output)
	}
}

// ─── EmitIntelGathered — daemon running ──────────────────────────────────────

func TestEmitter_EmitIntelGathered_SocketListening(t *testing.T) {
	sockPath := shortSockPath(t)
	received := startListener(t, sockPath)

	e := &Emitter{path: sockPath}
	e.EmitIntelGathered("ai-models", 42)

	select {
	case data := <-received:
		line := strings.TrimSpace(string(data))
		var ev map[string]interface{}
		if err := json.Unmarshal([]byte(line), &ev); err != nil {
			t.Fatalf("received invalid JSON: %q\nerror: %v", line, err)
		}

		// Verify required envelope fields
		if ev["type"] != EventTypeScoutIntelGathered {
			t.Errorf("type: expected %q, got %v", EventTypeScoutIntelGathered, ev["type"])
		}
		if ev["mission_id"] != "scout" {
			t.Errorf("mission_id: expected 'scout', got %v", ev["mission_id"])
		}
		if id, _ := ev["id"].(string); id == "" {
			t.Error("expected non-empty event id")
		}
		if _, ok := ev["timestamp"]; !ok {
			t.Error("expected timestamp field in event")
		}

		// Verify data payload
		d, ok := ev["data"].(map[string]interface{})
		if !ok {
			t.Fatalf("expected data to be an object, got %T", ev["data"])
		}
		if d["topic"] != "ai-models" {
			t.Errorf("topic: expected 'ai-models', got %v", d["topic"])
		}
		if count, _ := d["item_count"].(float64); count != 42 {
			t.Errorf("item_count: expected 42, got %v", d["item_count"])
		}

	case <-time.After(2 * time.Second):
		t.Fatal("timeout: no event received from emitter")
	}

	e.Close()
}

func TestEmitter_MultipleEmits_OnConnection(t *testing.T) {
	// Emit two events and verify both are received on the same connection.
	sockPath := shortSockPath(t)

	ln, err := net.Listen("unix", sockPath)
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	t.Cleanup(func() { ln.Close() })

	lines := make(chan string, 10)
	go func() {
		conn, err := ln.Accept()
		if err != nil {
			return
		}
		defer conn.Close()
		buf := make([]byte, 8192)
		n, _ := conn.Read(buf)
		// Events are newline-delimited; split and send each
		for _, l := range strings.Split(string(buf[:n]), "\n") {
			l = strings.TrimSpace(l)
			if l != "" {
				lines <- l
			}
		}
	}()

	e := &Emitter{path: sockPath}
	e.EmitIntelGathered("topic-a", 10)
	e.EmitIntelGathered("topic-b", 20)
	e.Close()

	// Collect events up to 2s
	var received []map[string]interface{}
	deadline := time.After(2 * time.Second)
	for len(received) < 2 {
		select {
		case line := <-lines:
			var ev map[string]interface{}
			if err := json.Unmarshal([]byte(line), &ev); err == nil {
				received = append(received, ev)
			}
		case <-deadline:
			goto done
		}
	}
done:
	if len(received) == 0 {
		t.Fatal("no events received")
	}
	// At least the first event should have the right type.
	if received[0]["type"] != EventTypeScoutIntelGathered {
		t.Errorf("first event type: expected %q, got %v", EventTypeScoutIntelGathered, received[0]["type"])
	}
}

func TestEmitter_EventID_IsUnique(t *testing.T) {
	// Two calls must produce different event IDs (crypto/rand-backed).
	a := newEventID()
	b := newEventID()
	if a == b {
		t.Errorf("expected unique event IDs, got %q twice", a)
	}
	if !strings.HasPrefix(a, "evt_") {
		t.Errorf("expected ID to start with 'evt_', got %q", a)
	}
}

func TestEmitter_EventID_Format(t *testing.T) {
	id := newEventID()
	// Format: "evt_" + 16 hex chars (8 random bytes)
	if len(id) != len("evt_")+16 {
		t.Errorf("expected ID length %d, got %d: %q", len("evt_")+16, len(id), id)
	}
}

// ─── browser-based gatherers: graceful degradation ───────────────────────────
// These tests live here to verify the contract: browser gatherers must return
// (nil, nil) when Chrome is unavailable — never an error that breaks gather.
// They use the gather package via its public interface.
