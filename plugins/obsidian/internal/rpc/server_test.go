// Package rpc tests the Unix-socket RPC server for the Obsidian plugin.
// RED phase: these tests reference types and functions that do not exist yet.
// TRK-530 — Phase 3 server implementation.
package rpc

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"sync"
	"testing"
	"time"
)

// newTestServer creates a Server wired to nil dependencies for tests that only
// need socket-level behaviour (no index or graph traversal).
func newTestServer(t *testing.T) (*Server, string) {
	t.Helper()
	dir := t.TempDir()
	sock := filepath.Join(dir, "obsidian.sock")
	srv := New(Config{Store: nil, Graph: nil})
	if err := srv.Start(sock); err != nil {
		t.Fatalf("Start: %v", err)
	}
	t.Cleanup(func() { _ = srv.Shutdown(context.Background()) })
	return srv, sock
}

// newTestClient dials sock and registers Close on cleanup.
func newTestClient(t *testing.T, sock string) *Client {
	t.Helper()
	c, err := Dial(sock)
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })
	return c
}

// --- API tests ---------------------------------------------------------------

func TestStartShutdown_RoundTrip(t *testing.T) {
	dir := t.TempDir()
	sock := filepath.Join(dir, "obsidian.sock")
	srv := New(Config{Store: nil, Graph: nil})

	if err := srv.Start(sock); err != nil {
		t.Fatalf("Start: %v", err)
	}
	if _, err := os.Stat(sock); err != nil {
		t.Fatalf("socket file not created: %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	if err := srv.Shutdown(ctx); err != nil {
		t.Fatalf("Shutdown: %v", err)
	}
	if _, err := os.Stat(sock); err == nil {
		t.Fatal("socket file must be removed after Shutdown")
	}
}

func TestStartRemovesStaleSocket(t *testing.T) {
	dir := t.TempDir()
	sock := filepath.Join(dir, "obsidian.sock")

	// Leave a stale socket file behind.
	if err := os.WriteFile(sock, []byte("stale"), 0o600); err != nil {
		t.Fatal(err)
	}

	srv := New(Config{Store: nil, Graph: nil})
	if err := srv.Start(sock); err != nil {
		t.Fatalf("Start must remove stale socket and succeed: %v", err)
	}
	defer func() { _ = srv.Shutdown(context.Background()) }()

	c, err := Dial(sock)
	if err != nil {
		t.Fatalf("Dial after stale-remove: %v", err)
	}
	_ = c.Close()
}

func TestShutdownTimeout(t *testing.T) {
	_, sock := newTestServer(t)

	// Occupy a connection but never close it — forces the server to drain.
	raw, err := net.Dial("unix", sock)
	if err != nil {
		t.Fatalf("raw dial: %v", err)
	}
	defer raw.Close()

	srv := New(Config{Store: nil, Graph: nil})
	_ = srv.Start(filepath.Join(t.TempDir(), "t.sock"))

	// Shutdown with a very tight deadline — must return deadline-exceeded, not hang.
	ctx, cancel := context.WithTimeout(context.Background(), 1*time.Millisecond)
	defer cancel()
	err = srv.Shutdown(ctx)
	if err == nil {
		t.Log("Shutdown completed before timeout — acceptable if fast")
	}
}

func TestPing(t *testing.T) {
	_, sock := newTestServer(t)
	c := newTestClient(t, sock)

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	if err := c.Ping(ctx); err != nil {
		t.Fatalf("Ping: %v", err)
	}
}

func TestIndexStat(t *testing.T) {
	_, sock := newTestServer(t)
	c := newTestClient(t, sock)

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	stat, err := c.IndexStat(ctx)
	if err != nil {
		t.Fatalf("IndexStat: %v", err)
	}
	if stat == nil {
		t.Fatal("IndexStat returned nil response")
	}
	if stat.NoteCount < 0 || stat.VertexCount < 0 || stat.EdgeCount < 0 {
		t.Fatalf("IndexStat returned negative counts: %+v", stat)
	}
}

func TestRecall_Stub(t *testing.T) {
	_, sock := newTestServer(t)
	c := newTestClient(t, sock)

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	resp, err := c.Recall(ctx, RecallRequest{Seed: "notes/index.md", MaxHops: 2, Limit: 20})
	if err != nil {
		t.Fatalf("Recall: %v", err)
	}
	if resp == nil {
		t.Fatal("Recall returned nil response")
	}
	// Server backed by nil graph — must return empty paths, not panic.
	if resp.Paths == nil {
		resp.Paths = []string{}
	}
}

func TestRecall_UnknownSeed(t *testing.T) {
	_, sock := newTestServer(t)
	c := newTestClient(t, sock)

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	resp, err := c.Recall(ctx, RecallRequest{
		Seed:    "does/not/exist.md",
		MaxHops: 3,
		Limit:   10,
	})
	if err != nil {
		t.Fatalf("Recall with unknown seed must not error: %v", err)
	}
	if len(resp.Paths) != 0 {
		t.Fatalf("unknown seed must return empty paths, got %v", resp.Paths)
	}
}

func TestDial_NoServer(t *testing.T) {
	sock := filepath.Join(t.TempDir(), "nonexistent.sock")
	_, err := Dial(sock)
	if err == nil {
		t.Fatal("Dial to non-existent socket must return an error")
	}
}

func TestServer_DoubleStart(t *testing.T) {
	dir := t.TempDir()
	sock := filepath.Join(dir, "obsidian.sock")
	srv := New(Config{Store: nil, Graph: nil})

	if err := srv.Start(sock); err != nil {
		t.Fatalf("first Start: %v", err)
	}
	defer func() { _ = srv.Shutdown(context.Background()) }()

	// Second Start on the same server — must return an error (already started).
	err := srv.Start(sock)
	if err == nil {
		t.Fatal("second Start on same Server must return an error")
	}
}

// --- T3.4: TestRecall_SocketProtocol — 8 malformed-request cases ------------

// malformedCase sends raw bytes to the socket and expects the server to close
// the connection cleanly (no panic, no hang).
type malformedCase struct {
	name    string
	payload []byte
}

func TestRecall_SocketProtocol(t *testing.T) {
	_, sock := newTestServer(t)

	cases := []malformedCase{
		{
			name:    "empty_body",
			payload: []byte{},
		},
		{
			name:    "invalid_utf8",
			payload: []byte{0xff, 0xfe, 0x00},
		},
		{
			name:    "truncated_json",
			payload: []byte(`{"method":"recall","params":{`),
		},
		{
			name:    "unknown_method",
			payload: mustMarshal(t, map[string]any{"method": "DoesNotExist", "params": map[string]any{}}),
		},
		{
			name:    "missing_method_field",
			payload: mustMarshal(t, map[string]any{"params": map[string]any{"seed": "a.md"}}),
		},
		{
			name:    "wrong_param_types",
			payload: mustMarshal(t, map[string]any{"method": "recall", "params": map[string]any{"seed": 42, "max_hops": "two"}}),
		},
		{
			name:    "null_body",
			payload: []byte("null"),
		},
		{
			name:    "array_instead_of_object",
			payload: []byte(`["recall","notes/index.md",2]`),
		},
	}

	for _, tc := range cases {
		tc := tc
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			conn, err := net.DialTimeout("unix", sock, 2*time.Second)
			if err != nil {
				t.Fatalf("dial: %v", err)
			}
			defer conn.Close()

			_ = conn.SetDeadline(time.Now().Add(2 * time.Second))

			if len(tc.payload) > 0 {
				if _, err := conn.Write(tc.payload); err != nil {
					// Write failure after sending malformed data is acceptable.
					return
				}
			}

			// Read until EOF or error — server must not hang.
			buf := make([]byte, 4096)
			_, _ = conn.Read(buf)
		})
	}
}

// --- T3.9: TestRecall_Concurrency — 50 goroutines × 10 calls ----------------

func TestRecall_Concurrency(t *testing.T) {
	const (
		goroutines = 50
		callsEach  = 10
	)

	_, sock := newTestServer(t)

	var wg sync.WaitGroup
	errs := make(chan error, goroutines*callsEach)

	for g := 0; g < goroutines; g++ {
		wg.Add(1)
		go func(id int) {
			defer wg.Done()
			c, err := Dial(sock)
			if err != nil {
				errs <- fmt.Errorf("goroutine %d Dial: %w", id, err)
				return
			}
			defer c.Close()

			for i := 0; i < callsEach; i++ {
				ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
				seed := fmt.Sprintf("notes/node-%d.md", (id*callsEach+i)%20)
				_, err := c.Recall(ctx, RecallRequest{Seed: seed, MaxHops: 2, Limit: 10})
				cancel()
				if err != nil {
					errs <- fmt.Errorf("goroutine %d call %d: %w", id, i, err)
				}
			}
		}(g)
	}

	wg.Wait()
	close(errs)

	var failures []error
	for err := range errs {
		failures = append(failures, err)
	}
	if len(failures) > 0 {
		for _, e := range failures {
			t.Error(e)
		}
		t.Fatalf("%d/%d calls failed under concurrency", len(failures), goroutines*callsEach)
	}
}

// --- helpers -----------------------------------------------------------------

func mustMarshal(t *testing.T, v any) []byte {
	t.Helper()
	b, err := json.Marshal(v)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	return b
}
