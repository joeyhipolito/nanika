package daemon

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/persona"
)

// newTestServer returns an APIServer wired to a fresh Bus, with no auth.
// It registers a cleanup that terminates any background processes launched
// via POST /api/missions/run so the test doesn't leak goroutines.
func newTestServer(t *testing.T) (*APIServer, *event.Bus) {
	t.Helper()
	bus := event.NewBus()
	cfg := Config{} // empty = no auth
	srv := NewAPIServer(bus, event.NewBusEmitter(bus), cfg)
	t.Cleanup(srv.shutdownProcesses)
	return srv, bus
}

// ---------- health check --------------------------------------------------

func TestHealth(t *testing.T) {
	srv, _ := newTestServer(t)

	req := httptest.NewRequest(http.MethodGet, "/api/health", nil)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("health: want 200, got %d", rec.Code)
	}

	var body map[string]any
	if err := json.Unmarshal(rec.Body.Bytes(), &body); err != nil {
		t.Fatalf("health: unmarshal: %v", err)
	}
	if body["status"] != "ok" {
		t.Errorf("health: want status=ok, got %q", body["status"])
	}
	if body["subscriber_drops"] != float64(0) {
		t.Errorf("health: want subscriber_drops=0, got %v", body["subscriber_drops"])
	}
}

func TestHealth_ShapeIncludesAllDropCounters(t *testing.T) {
	srv, _ := newTestServer(t)

	req := httptest.NewRequest(http.MethodGet, "/api/health", nil)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	var body map[string]any
	if err := json.Unmarshal(rec.Body.Bytes(), &body); err != nil {
		t.Fatalf("health shape: unmarshal: %v", err)
	}
	for _, key := range []string{"status", "subscriber_drops", "file_dropped_writes", "uds_dropped_writes"} {
		if _, ok := body[key]; !ok {
			t.Errorf("health shape: missing key %q in response", key)
		}
	}
}

func TestHealth_DegradedWhenSubscriberDropsPresent(t *testing.T) {
	srv, bus := newTestServer(t)
	_, _ = bus.Subscribe() // never drained
	for i := 0; i < 100; i++ {
		bus.Publish(event.New(event.MissionStarted, "m1", "", "", nil))
	}

	req := httptest.NewRequest(http.MethodGet, "/api/health", nil)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("health degraded: want 200, got %d", rec.Code)
	}

	var body map[string]any
	if err := json.Unmarshal(rec.Body.Bytes(), &body); err != nil {
		t.Fatalf("health degraded: unmarshal: %v", err)
	}
	if body["status"] != "degraded" {
		t.Fatalf("health degraded: want status=degraded, got %v", body["status"])
	}
	if body["subscriber_drops"] == float64(0) {
		t.Fatalf("health degraded: want subscriber_drops > 0, got %v", body["subscriber_drops"])
	}
}

// ---------- list missions (empty) -----------------------------------------

func TestListMissions_Empty(t *testing.T) {
	srv, _ := newTestServer(t)

	// Use a temp dir that exists but has no JSONL files.
	req := httptest.NewRequest(http.MethodGet, "/api/missions", nil)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	// Empty dir or non-existent logs dir should return 200 + empty/null JSON.
	if rec.Code != http.StatusOK {
		t.Fatalf("list missions: want 200, got %d", rec.Code)
	}
}

// ---------- single mission not found --------------------------------------

func TestGetMission_NotFound(t *testing.T) {
	srv, _ := newTestServer(t)

	req := httptest.NewRequest(http.MethodGet, "/api/missions/nonexistent-id-xyz", nil)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusNotFound {
		t.Fatalf("get mission: want 404, got %d", rec.Code)
	}
}

// ---------- mission ID validation -----------------------------------------

func TestGetMission_TraversalRejected(t *testing.T) {
	srv, _ := newTestServer(t)

	// Go's ServeMux handles slash-containing paths before they reach our
	// handler (redirects to cleaned path, 404 for unmatched patterns).
	// Paths with ".." in a single segment reach parseMissionID and get 400.
	// In all cases the invariant is: traversal attempts must never return 200.
	cases := []string{
		"../etc/passwd",
		"foo/bar",
		"../../secret",
		"..secret..",
	}
	for _, id := range cases {
		req := httptest.NewRequest(http.MethodGet, "/api/missions/"+id, nil)
		rec := httptest.NewRecorder()
		srv.srv.Handler.ServeHTTP(rec, req)
		if rec.Code == http.StatusOK {
			t.Errorf("traversal %q: must not return 200, got %d", id, rec.Code)
		}
	}
}

func TestGetMission_DotDotRejected(t *testing.T) {
	srv, _ := newTestServer(t)

	req := httptest.NewRequest(http.MethodGet, "/api/missions/..secret..", nil)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusBadRequest {
		t.Fatalf("dotdot id: want 400, got %d", rec.Code)
	}
}

// ---------- DAG not found -------------------------------------------------

func TestGetMissionDAG_NotFound(t *testing.T) {
	srv, _ := newTestServer(t)

	req := httptest.NewRequest(http.MethodGet, "/api/missions/nonexistent-id-xyz/dag", nil)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusNotFound {
		t.Fatalf("dag not found: want 404, got %d", rec.Code)
	}
}

// ---------- buildDAG unit test --------------------------------------------

func TestBuildDAG(t *testing.T) {
	plan := testPlan()

	dag := buildDAG("m1", plan)

	if dag.MissionID != "m1" {
		t.Errorf("dag.MissionID: want m1, got %q", dag.MissionID)
	}
	if len(dag.Nodes) != 3 {
		t.Fatalf("nodes: want 3, got %d", len(dag.Nodes))
	}

	// phase-3 depends on phase-1 and phase-2 → 2 edges.
	if len(dag.Edges) != 2 {
		t.Fatalf("edges: want 2, got %d", len(dag.Edges))
	}

	// Both edges point TO phase-3.
	for _, e := range dag.Edges {
		if e.To != "phase-3" {
			t.Errorf("edge.To: want phase-3, got %q", e.To)
		}
	}

	// Nodes with no dependencies should have an empty slice, not nil.
	for _, n := range dag.Nodes {
		if n.Dependencies == nil {
			t.Errorf("node %q: dependencies should be empty slice, not nil", n.ID)
		}
	}
}

// ---------- SSE stream: Content-Type and keepalive format -----------------

func TestSSE_ContentType(t *testing.T) {
	srv, _ := newTestServer(t)

	// Use a real TCP listener so we can connect and disconnect cleanly.
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer ln.Close()

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	go srv.Serve(ctx, ln) //nolint:errcheck

	url := fmt.Sprintf("http://%s/api/events", ln.Addr())
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		t.Fatalf("new request: %v", err)
	}

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("do: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		t.Fatalf("sse: want 200, got %d", resp.StatusCode)
	}
	ct := resp.Header.Get("Content-Type")
	if !strings.HasPrefix(ct, "text/event-stream") {
		t.Errorf("Content-Type: want text/event-stream, got %q", ct)
	}
}

// TestSSE_ReplayAndLive verifies that:
//  1. Events published before the subscription are replayed via Last-Event-ID=0.
//  2. Events published after connection arrive live.
func TestSSE_ReplayAndLive(t *testing.T) {
	srv, bus := newTestServer(t)

	// Pre-publish two events to the ring buffer.
	bus.Publish(makeEvent("e1", "mission.started"))
	bus.Publish(makeEvent("e2", "phase.started"))

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer ln.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	go srv.Serve(ctx, ln) //nolint:errcheck

	url := fmt.Sprintf("http://%s/api/events", ln.Addr())
	req, _ := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	req.Header.Set("Last-Event-ID", "0") // ask for replay from beginning

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("do: %v", err)
	}
	defer resp.Body.Close()

	// Read lines until we've seen the 2 replayed events.
	scanner := bufio.NewScanner(resp.Body)
	seen := map[string]bool{}
	deadline := time.After(5 * time.Second)

	for {
		lineCh := make(chan string, 1)
		go func() {
			if scanner.Scan() {
				lineCh <- scanner.Text()
			}
		}()
		select {
		case <-deadline:
			t.Fatalf("timed out waiting for events; seen: %v", seen)
		case line := <-lineCh:
			if strings.HasPrefix(line, "event: ") {
				typ := strings.TrimPrefix(line, "event: ")
				seen[typ] = true
			}
			if seen["mission.started"] && seen["phase.started"] {
				return // success
			}
		}
	}
}

// TestSSE_LastEventID verifies reconnect skips already-seen events.
func TestSSE_LastEventID(t *testing.T) {
	srv, bus := newTestServer(t)

	// Publish 3 events; client reconnects with Last-Event-ID=2 and should
	// only receive event 3.
	e1 := makeEvent("e1", "mission.started")
	e2 := makeEvent("e2", "phase.started")
	e3 := makeEvent("e3", "phase.completed")
	bus.Publish(e1)
	bus.Publish(e2)
	bus.Publish(e3)

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer ln.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	go srv.Serve(ctx, ln) //nolint:errcheck

	url := fmt.Sprintf("http://%s/api/events", ln.Addr())
	req, _ := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	req.Header.Set("Last-Event-ID", "2") // skip e1 and e2

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("do: %v", err)
	}
	defer resp.Body.Close()

	scanner := bufio.NewScanner(resp.Body)
	deadline := time.After(5 * time.Second)

	for {
		lineCh := make(chan string, 1)
		go func() {
			if scanner.Scan() {
				lineCh <- scanner.Text()
			}
		}()
		select {
		case <-deadline:
			t.Fatal("timed out waiting for replayed event after Last-Event-ID=2")
		case line := <-lineCh:
			if strings.HasPrefix(line, "event: phase.completed") {
				return // received only the expected event
			}
			if strings.HasPrefix(line, "event: mission.started") ||
				strings.HasPrefix(line, "event: phase.started") {
				t.Error("received event that should have been skipped by Last-Event-ID")
			}
		}
	}
}

// ---------- auth middleware -----------------------------------------------

func TestAuth_NoKey_Allows(t *testing.T) {
	// No auth configured → all requests allowed.
	srv, _ := newTestServer(t)

	req := httptest.NewRequest(http.MethodGet, "/api/health", nil)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("no-auth health: want 200, got %d", rec.Code)
	}
}

func TestAuth_WithAPIKey_Rejects(t *testing.T) {
	bus := event.NewBus()
	cfg := Config{APIKey: "secret-key"}
	srv := NewAPIServer(bus, event.NewBusEmitter(bus), cfg)

	req := httptest.NewRequest(http.MethodGet, "/api/missions", nil)
	// No Authorization header.
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("missing key: want 401, got %d", rec.Code)
	}
}

func TestAuth_WithAPIKey_Accepts(t *testing.T) {
	bus := event.NewBus()
	cfg := Config{APIKey: "secret-key"}
	srv := NewAPIServer(bus, event.NewBusEmitter(bus), cfg)

	req := httptest.NewRequest(http.MethodGet, "/api/missions", nil)
	req.Header.Set("Authorization", "Bearer secret-key")
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	// 200 (empty list) — not 401.
	if rec.Code == http.StatusUnauthorized {
		t.Fatal("valid key: want non-401, got 401")
	}
}

func TestAuth_WrongKey_Rejects(t *testing.T) {
	bus := event.NewBus()
	cfg := Config{APIKey: "secret-key"}
	srv := NewAPIServer(bus, event.NewBusEmitter(bus), cfg)

	req := httptest.NewRequest(http.MethodGet, "/api/missions", nil)
	req.Header.Set("Authorization", "Bearer wrong-key")
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusUnauthorized {
		t.Fatalf("wrong key: want 401, got %d", rec.Code)
	}
}

// TestAuth_CORSPreflight ensures OPTIONS requests bypass auth.
func TestAuth_CORSPreflight(t *testing.T) {
	bus := event.NewBus()
	cfg := Config{APIKey: "secret-key"}
	srv := NewAPIServer(bus, event.NewBusEmitter(bus), cfg)

	req := httptest.NewRequest(http.MethodOptions, "/api/missions", nil)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code == http.StatusUnauthorized {
		t.Fatal("OPTIONS preflight: should not require auth")
	}
}

// ---------- CORS headers --------------------------------------------------

func TestCORSHeaders(t *testing.T) {
	srv, _ := newTestServer(t)

	req := httptest.NewRequest(http.MethodGet, "/api/health", nil)
	req.Header.Set("Origin", "http://localhost:3000")
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	origin := rec.Header().Get("Access-Control-Allow-Origin")
	if origin != "http://localhost:3000" {
		t.Errorf("CORS origin: want http://localhost:3000, got %q", origin)
	}
}

// ---------- SSE filtering ------------------------------------------------

// TestSSE_TypeFilter verifies that ?types= filters events by type.
func TestSSE_TypeFilter(t *testing.T) {
	srv, bus := newTestServer(t)

	bus.Publish(makeEvent("e1", event.MissionStarted))
	bus.Publish(makeEventWithMission("e2", event.WorkerOutput, "m1"))
	bus.Publish(makeEvent("e3", event.PhaseCompleted))
	bus.Publish(makeEventWithMission("e4", event.WorkerOutput, "m2"))

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer ln.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	go srv.Serve(ctx, ln) //nolint:errcheck

	url := fmt.Sprintf("http://%s/api/events?types=worker.output", ln.Addr())
	req, _ := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	req.Header.Set("Last-Event-ID", "0")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("do: %v", err)
	}
	defer resp.Body.Close()

	seen := collectSSEEvents(t, resp, 2, 5*time.Second)
	for _, typ := range seen {
		if typ != "worker.output" {
			t.Errorf("type filter leaked %q through worker.output filter", typ)
		}
	}
}

// TestSSE_MissionFilter verifies that ?mission_id= filters events by mission.
func TestSSE_MissionFilter(t *testing.T) {
	srv, bus := newTestServer(t)

	bus.Publish(makeEventWithMission("e1", event.WorkerOutput, "target-mission"))
	bus.Publish(makeEventWithMission("e2", event.WorkerOutput, "other-mission"))
	bus.Publish(makeEventWithMission("e3", event.PhaseCompleted, "target-mission"))

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer ln.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	go srv.Serve(ctx, ln) //nolint:errcheck

	url := fmt.Sprintf("http://%s/api/events?mission_id=target-mission", ln.Addr())
	req, _ := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	req.Header.Set("Last-Event-ID", "0")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("do: %v", err)
	}
	defer resp.Body.Close()

	seen := collectSSEEvents(t, resp, 2, 5*time.Second)
	// Should see worker.output and phase.completed, both for target-mission.
	if len(seen) != 2 {
		t.Fatalf("mission filter: want 2 events, got %d: %v", len(seen), seen)
	}
}

// ---------- per-mission stream -------------------------------------------

func TestMissionStream(t *testing.T) {
	srv, bus := newTestServer(t)

	bus.Publish(makeEventWithMission("e1", event.WorkerOutput, "stream-mission"))
	bus.Publish(makeEventWithMission("e2", event.WorkerOutput, "other-mission"))

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer ln.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	go srv.Serve(ctx, ln) //nolint:errcheck

	url := fmt.Sprintf("http://%s/api/missions/stream-mission/stream", ln.Addr())
	req, _ := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	req.Header.Set("Last-Event-ID", "0")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("do: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		t.Fatalf("mission stream: want 200, got %d", resp.StatusCode)
	}
	ct := resp.Header.Get("Content-Type")
	if !strings.HasPrefix(ct, "text/event-stream") {
		t.Errorf("Content-Type: want text/event-stream, got %q", ct)
	}

	seen := collectSSEEvents(t, resp, 1, 5*time.Second)
	if len(seen) != 1 {
		t.Fatalf("mission stream: want 1 event (filtered), got %d: %v", len(seen), seen)
	}
}

// ---------- HTTP POST ingestion ------------------------------------------

func TestIngestEvents_SingleJSON(t *testing.T) {
	srv, bus := newTestServer(t)

	ev := event.Event{
		ID:        "ingest-1",
		Type:      event.WorkerOutput,
		MissionID: "ingest-test",
		Data: map[string]any{
			"chunk":     "hello from POST",
			"streaming": true,
		},
	}
	body, _ := json.Marshal(ev)

	req := httptest.NewRequest(http.MethodPost, "/api/events", strings.NewReader(string(body)))
	req.Header.Set("Content-Type", "application/json")
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusAccepted {
		t.Fatalf("ingest single: want 202, got %d; body: %s", rec.Code, rec.Body.String())
	}

	var resp map[string]int
	json.Unmarshal(rec.Body.Bytes(), &resp)
	if resp["published"] != 1 {
		t.Errorf("ingest single: want published=1, got %d", resp["published"])
	}

	// Verify the event made it to the bus.
	events := bus.EventsSince(0)
	if len(events) != 1 {
		t.Fatalf("bus: want 1 event, got %d", len(events))
	}
	if events[0].MissionID != "ingest-test" {
		t.Errorf("bus event mission_id: want ingest-test, got %q", events[0].MissionID)
	}
}

func TestIngestEvents_NDJSON(t *testing.T) {
	srv, bus := newTestServer(t)

	lines := []string{
		`{"id":"n1","type":"worker.output","mission_id":"m1","data":{"chunk":"line1","streaming":true}}`,
		`{"id":"n2","type":"worker.completed","mission_id":"m1","data":{"output_len":100}}`,
		``, // blank line should be skipped
		`not valid json`, // malformed line should be skipped
	}
	body := strings.Join(lines, "\n")

	req := httptest.NewRequest(http.MethodPost, "/api/events", strings.NewReader(body))
	req.Header.Set("Content-Type", "application/x-ndjson")
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusAccepted {
		t.Fatalf("ingest ndjson: want 202, got %d; body: %s", rec.Code, rec.Body.String())
	}

	var resp map[string]int
	json.Unmarshal(rec.Body.Bytes(), &resp)
	if resp["published"] != 2 {
		t.Errorf("ingest ndjson: want published=2, got %d", resp["published"])
	}

	events := bus.EventsSince(0)
	if len(events) != 2 {
		t.Fatalf("bus: want 2 events, got %d", len(events))
	}
}

func TestIngestEvents_EmptyType_Rejected(t *testing.T) {
	srv, _ := newTestServer(t)

	body := `{"id":"bad","mission_id":"m1"}`
	req := httptest.NewRequest(http.MethodPost, "/api/events", strings.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusBadRequest {
		t.Fatalf("ingest empty type: want 400, got %d", rec.Code)
	}
}

func TestIngestEvents_InvalidJSON_Rejected(t *testing.T) {
	srv, _ := newTestServer(t)

	req := httptest.NewRequest(http.MethodPost, "/api/events", strings.NewReader("{not json"))
	req.Header.Set("Content-Type", "application/json")
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusBadRequest {
		t.Fatalf("ingest invalid json: want 400, got %d", rec.Code)
	}
}

// ---------- mixed-mission replay/live ordering tests ---------------------

// TestSSE_MixedMission_BusAssignsGlobalSequences verifies that the bus
// overwrites mission-local sequences with globally-unique cursors so that
// two concurrent missions whose MultiEmitters both start at seq=1 do not
// produce duplicate SSE ids.
func TestSSE_MixedMission_BusAssignsGlobalSequences(t *testing.T) {
	_, bus := newTestServer(t)

	// Simulate two missions whose MultiEmitters each start from seq=1 —
	// exactly the collision that occurred before the fix.
	evA1 := event.Event{ID: "a1", Type: event.MissionStarted, MissionID: "alpha", Sequence: 1}
	evA2 := event.Event{ID: "a2", Type: event.PhaseStarted, MissionID: "alpha", Sequence: 2}
	evB1 := event.Event{ID: "b1", Type: event.MissionStarted, MissionID: "beta", Sequence: 1} // collides with A
	evB2 := event.Event{ID: "b2", Type: event.PhaseStarted, MissionID: "beta", Sequence: 2}  // collides with A

	bus.Publish(evA1)
	bus.Publish(evA2)
	bus.Publish(evB1)
	bus.Publish(evB2)

	events := bus.EventsSince(0)
	if len(events) != 4 {
		t.Fatalf("ring: want 4 events, got %d", len(events))
	}

	// All sequences must be globally unique (1, 2, 3, 4) despite the
	// mission-local input sequences being (1, 2, 1, 2).
	seqs := make(map[int64]bool)
	for _, ev := range events {
		if seqs[ev.Sequence] {
			t.Errorf("duplicate sequence %d in ring buffer — bus did not assign global seq", ev.Sequence)
		}
		seqs[ev.Sequence] = true
	}

	// Sequences must be monotonically increasing in insertion order.
	for i := 1; i < len(events); i++ {
		if events[i].Sequence <= events[i-1].Sequence {
			t.Errorf("non-monotonic sequences at positions %d/%d: %d <= %d",
				i-1, i, events[i].Sequence, events[i-1].Sequence)
		}
	}
}

// TestSSE_MixedMission_ReplayDeliversAllEvents verifies that, when events
// from two missions share the same mission-local sequence numbers, the SSE
// replay path still delivers all events to the client without silently
// dropping any due to sequence collision.
func TestSSE_MixedMission_ReplayDeliversAllEvents(t *testing.T) {
	srv, bus := newTestServer(t)

	// Publish 2 events per mission; local seqs would collide (1,2 / 1,2).
	bus.Publish(event.Event{ID: "a1", Type: event.MissionStarted, MissionID: "alpha", Sequence: 1})
	bus.Publish(event.Event{ID: "a2", Type: event.PhaseCompleted, MissionID: "alpha", Sequence: 2})
	bus.Publish(event.Event{ID: "b1", Type: event.MissionStarted, MissionID: "beta", Sequence: 1})
	bus.Publish(event.Event{ID: "b2", Type: event.PhaseCompleted, MissionID: "beta", Sequence: 2})

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer ln.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	go srv.Serve(ctx, ln) //nolint:errcheck

	url := fmt.Sprintf("http://%s/api/events", ln.Addr())
	req, _ := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	req.Header.Set("Last-Event-ID", "0")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("do: %v", err)
	}
	defer resp.Body.Close()

	// Expect all 4 events (2 per mission); previously only 2 were delivered
	// because beta's seq=1,2 looked "already replayed" after alpha's seq=2.
	seen := collectSSEEvents(t, resp, 4, 5*time.Second)
	if len(seen) != 4 {
		t.Fatalf("want 4 events from 2 missions, got %d: %v", len(seen), seen)
	}
}

// TestSSE_MixedMission_ReconnectCursor verifies that a client reconnecting
// with the Last-Event-ID of the last event it received resumes correctly
// across mission boundaries — receiving only newer events regardless of
// their mission-local sequence numbers.
func TestSSE_MixedMission_ReconnectCursor(t *testing.T) {
	srv, bus := newTestServer(t)

	// Alpha mission publishes 2 events; beta publishes 1 event before reconnect.
	bus.Publish(event.Event{ID: "a1", Type: event.MissionStarted, MissionID: "alpha", Sequence: 1})
	bus.Publish(event.Event{ID: "a2", Type: event.PhaseCompleted, MissionID: "alpha", Sequence: 2})
	bus.Publish(event.Event{ID: "b1", Type: event.MissionStarted, MissionID: "beta", Sequence: 1})

	// After the fix the bus assigns global seqs: a1=1, a2=2, b1=3.
	// A client that received all 3 reconnects with Last-Event-ID=3 and should
	// get only the subsequent event (b2), not the earlier ones.
	bus.Publish(event.Event{ID: "b2", Type: event.PhaseCompleted, MissionID: "beta", Sequence: 2})

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer ln.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	go srv.Serve(ctx, ln) //nolint:errcheck

	url := fmt.Sprintf("http://%s/api/events", ln.Addr())
	req, _ := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	req.Header.Set("Last-Event-ID", "3") // client already saw a1,a2,b1

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("do: %v", err)
	}
	defer resp.Body.Close()

	// Only b2 (global seq=4) should arrive; a1/a2/b1 must not be replayed.
	scanner := bufio.NewScanner(resp.Body)
	deadline := time.After(5 * time.Second)
	for {
		lineCh := make(chan string, 1)
		go func() {
			if scanner.Scan() {
				lineCh <- scanner.Text()
			}
		}()
		select {
		case <-deadline:
			t.Fatal("timed out waiting for b2 after reconnect with Last-Event-ID=3")
		case line := <-lineCh:
			if strings.HasPrefix(line, "event: phase.completed") {
				return // received b2 — success
			}
			if strings.HasPrefix(line, "event: mission.started") {
				t.Error("received mission.started which should have been skipped by Last-Event-ID=3")
			}
		}
	}
}

// ---------- sseFilter unit tests -----------------------------------------

func TestSSEFilter_Match(t *testing.T) {
	cases := []struct {
		name   string
		filter sseFilter
		event  event.Event
		want   bool
	}{
		{
			name:   "zero filter matches all",
			filter: sseFilter{},
			event:  event.Event{Type: event.WorkerOutput, MissionID: "m1"},
			want:   true,
		},
		{
			name:   "type filter accepts matching type",
			filter: sseFilter{types: map[event.EventType]bool{event.WorkerOutput: true}},
			event:  event.Event{Type: event.WorkerOutput},
			want:   true,
		},
		{
			name:   "type filter rejects non-matching type",
			filter: sseFilter{types: map[event.EventType]bool{event.WorkerOutput: true}},
			event:  event.Event{Type: event.PhaseCompleted},
			want:   false,
		},
		{
			name:   "mission filter accepts matching mission",
			filter: sseFilter{missionID: "m1"},
			event:  event.Event{Type: event.WorkerOutput, MissionID: "m1"},
			want:   true,
		},
		{
			name:   "mission filter rejects non-matching mission",
			filter: sseFilter{missionID: "m1"},
			event:  event.Event{Type: event.WorkerOutput, MissionID: "m2"},
			want:   false,
		},
		{
			name: "combined filter requires both",
			filter: sseFilter{
				types:     map[event.EventType]bool{event.WorkerOutput: true},
				missionID: "m1",
			},
			event: event.Event{Type: event.WorkerOutput, MissionID: "m1"},
			want:  true,
		},
		{
			name: "combined filter rejects wrong type",
			filter: sseFilter{
				types:     map[event.EventType]bool{event.WorkerOutput: true},
				missionID: "m1",
			},
			event: event.Event{Type: event.PhaseCompleted, MissionID: "m1"},
			want:  false,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := tc.filter.match(tc.event)
			if got != tc.want {
				t.Errorf("match: got %v, want %v", got, tc.want)
			}
		})
	}
}

// ---------- helpers -------------------------------------------------------

// collectSSEEvents reads SSE event lines from resp until it has collected
// wantCount event types or the deadline expires.
func collectSSEEvents(t *testing.T, resp *http.Response, wantCount int, timeout time.Duration) []string {
	t.Helper()
	scanner := bufio.NewScanner(resp.Body)
	var seen []string
	deadline := time.After(timeout)

	for len(seen) < wantCount {
		lineCh := make(chan string, 1)
		go func() {
			if scanner.Scan() {
				lineCh <- scanner.Text()
			}
		}()
		select {
		case <-deadline:
			t.Fatalf("timed out waiting for events; seen %d of %d: %v", len(seen), wantCount, seen)
		case line := <-lineCh:
			if strings.HasPrefix(line, "event: ") {
				seen = append(seen, strings.TrimPrefix(line, "event: "))
			}
		}
	}
	return seen
}


// makeEvent creates a minimal event with a given ID and type for testing.
func makeEvent(id string, typ event.EventType) event.Event {
	return event.Event{
		ID:        id,
		Type:      typ,
		Timestamp: time.Now().UTC(),
		MissionID: "test-mission",
	}
}

// makeEventWithMission creates a minimal event with a specific mission ID.
func makeEventWithMission(id string, typ event.EventType, missionID string) event.Event {
	return event.Event{
		ID:        id,
		Type:      typ,
		Timestamp: time.Now().UTC(),
		MissionID: missionID,
	}
}

// ---------- GET /api/metrics ---------------------------------------------

// TestHandleMetrics_Shape verifies the endpoint returns 200 with the expected
// JSON shape (all required keys present and correct types) regardless of whether
// real mission data exists on disk.
func TestHandleMetrics_Shape(t *testing.T) {
	srv, _ := newTestServer(t)

	req := httptest.NewRequest(http.MethodGet, "/api/metrics", nil)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("metrics shape: want 200, got %d", rec.Code)
	}
	if ct := rec.Header().Get("Content-Type"); !strings.HasPrefix(ct, "application/json") {
		t.Errorf("metrics shape: Content-Type: want application/json, got %q", ct)
	}

	var body MetricsResponse
	if err := json.Unmarshal(rec.Body.Bytes(), &body); err != nil {
		t.Fatalf("metrics shape: unmarshal: %v", err)
	}
	// Total must be non-negative and consistent with sub-counts.
	if body.Total < 0 {
		t.Errorf("metrics shape: total < 0")
	}
	if body.Completed+body.Failed+body.Cancelled > body.Total {
		t.Errorf("metrics shape: sub-counts exceed total (%d+%d+%d > %d)",
			body.Completed, body.Failed, body.Cancelled, body.Total)
	}
	if body.ByDomain == nil {
		t.Error("metrics shape: by_domain must not be nil")
	}
	if body.ByPersona == nil {
		t.Error("metrics shape: phases_by_persona must not be nil")
	}
	if body.Recent == nil {
		t.Error("metrics shape: recent must not be nil")
	}
}

// TestAggregateMetrics_Basic exercises aggregateMetrics with a temporary
// directory tree containing a metrics.jsonl with known content.
func TestAggregateMetrics_Basic(t *testing.T) {
	dir := t.TempDir()

	// Write metrics.jsonl with 3 records: 2 success, 1 failure.
	metricsPath := dir + "/metrics.jsonl"
	lines := []string{
		`{"workspace_id":"ws1","domain":"dev","task":"build api","started_at":"2026-01-01T10:00:00Z","duration_s":120,"status":"success","phases":[{"persona":"architect","status":"completed"},{"persona":"implementer","status":"completed"}]}`,
		`{"workspace_id":"ws2","domain":"dev","task":"fix bug","started_at":"2026-01-02T10:00:00Z","duration_s":60,"status":"failure","phases":[{"persona":"qa-engineer","status":"failed"}]}`,
		`{"workspace_id":"ws3","domain":"personal","task":"plan trip","started_at":"2026-01-03T10:00:00Z","duration_s":90,"status":"success","phases":[{"persona":"researcher","status":"completed"}]}`,
	}
	if err := os.WriteFile(metricsPath, []byte(strings.Join(lines, "\n")+"\n"), 0600); err != nil {
		t.Fatalf("write metrics.jsonl: %v", err)
	}

	wsBase := dir + "/workspaces"
	eventsBase := dir + "/events"

	resp, err := aggregateMetrics(metricsPath, wsBase, eventsBase, 10)
	if err != nil {
		t.Fatalf("aggregateMetrics: %v", err)
	}

	if resp.Total != 3 {
		t.Errorf("total: want 3, got %d", resp.Total)
	}
	if resp.Completed != 2 {
		t.Errorf("completed: want 2, got %d", resp.Completed)
	}
	if resp.Failed != 1 {
		t.Errorf("failed: want 1, got %d", resp.Failed)
	}
	if resp.Cancelled != 0 {
		t.Errorf("cancelled: want 0, got %d", resp.Cancelled)
	}

	// avg = (120+60+90)/3 = 90
	if resp.AvgDurationS != 90 {
		t.Errorf("avg_duration_s: want 90, got %d", resp.AvgDurationS)
	}

	// by_domain
	devStats, ok := resp.ByDomain["dev"]
	if !ok {
		t.Fatal("by_domain: missing 'dev'")
	}
	if devStats.Total != 2 {
		t.Errorf("by_domain[dev].total: want 2, got %d", devStats.Total)
	}
	if devStats.Completed != 1 {
		t.Errorf("by_domain[dev].completed: want 1, got %d", devStats.Completed)
	}
	if devStats.Failed != 1 {
		t.Errorf("by_domain[dev].failed: want 1, got %d", devStats.Failed)
	}

	personalStats, ok := resp.ByDomain["personal"]
	if !ok {
		t.Fatal("by_domain: missing 'personal'")
	}
	if personalStats.Completed != 1 {
		t.Errorf("by_domain[personal].completed: want 1, got %d", personalStats.Completed)
	}

	// by_persona
	archStats, ok := resp.ByPersona["architect"]
	if !ok {
		t.Fatal("by_persona: missing 'architect'")
	}
	if archStats.Phases != 1 {
		t.Errorf("by_persona[architect].phases: want 1, got %d", archStats.Phases)
	}
	if archStats.Completed != 1 {
		t.Errorf("by_persona[architect].completed: want 1, got %d", archStats.Completed)
	}

	qaStats, ok := resp.ByPersona["qa-engineer"]
	if !ok {
		t.Fatal("by_persona: missing 'qa-engineer'")
	}
	if qaStats.Failed != 1 {
		t.Errorf("by_persona[qa-engineer].failed: want 1, got %d", qaStats.Failed)
	}

	// recent: newest first — ws3, ws2, ws1
	if len(resp.Recent) != 3 {
		t.Fatalf("recent: want 3, got %d", len(resp.Recent))
	}
	if resp.Recent[0].WorkspaceID != "ws3" {
		t.Errorf("recent[0]: want ws3, got %q", resp.Recent[0].WorkspaceID)
	}
	if resp.Recent[0].Status != "completed" {
		t.Errorf("recent[0].status: want completed, got %q", resp.Recent[0].Status)
	}
}

// TestAggregateMetrics_Cancelled verifies that workspaces with a
// mission.cancelled event in their event log (but no metrics.jsonl entry)
// are counted as cancelled.
func TestAggregateMetrics_Cancelled(t *testing.T) {
	dir := t.TempDir()

	// metrics.jsonl: one completed mission.
	metricsPath := dir + "/metrics.jsonl"
	if err := os.WriteFile(metricsPath,
		[]byte(`{"workspace_id":"ws1","domain":"dev","task":"done","started_at":"2026-01-01T10:00:00Z","duration_s":60,"status":"success"}`+"\n"),
		0600,
	); err != nil {
		t.Fatalf("write metrics.jsonl: %v", err)
	}

	wsBase := dir + "/workspaces"
	eventsBase := dir + "/events"

	// Create a workspace + checkpoint for the cancelled mission.
	wsPath := wsBase + "/ws2"
	if err := os.MkdirAll(wsPath, 0700); err != nil {
		t.Fatalf("mkdir ws2: %v", err)
	}
	cpData := `{"version":1,"workspace_id":"ws2","domain":"personal","status":"in_progress"}`
	if err := os.WriteFile(wsPath+"/checkpoint.json", []byte(cpData), 0600); err != nil {
		t.Fatalf("write checkpoint: %v", err)
	}

	// Write an event log with a mission.cancelled event.
	if err := os.MkdirAll(eventsBase, 0700); err != nil {
		t.Fatalf("mkdir events: %v", err)
	}
	eventLine := `{"id":"e1","type":"mission.cancelled","timestamp":"2026-01-02T10:00:00Z","sequence":1,"mission_id":"ws2"}`
	if err := os.WriteFile(eventsBase+"/ws2.jsonl", []byte(eventLine+"\n"), 0600); err != nil {
		t.Fatalf("write event log: %v", err)
	}

	resp, err := aggregateMetrics(metricsPath, wsBase, eventsBase, 10)
	if err != nil {
		t.Fatalf("aggregateMetrics: %v", err)
	}

	if resp.Total != 2 {
		t.Errorf("total: want 2, got %d", resp.Total)
	}
	if resp.Completed != 1 {
		t.Errorf("completed: want 1, got %d", resp.Completed)
	}
	if resp.Cancelled != 1 {
		t.Errorf("cancelled: want 1, got %d", resp.Cancelled)
	}

	personalStats, ok := resp.ByDomain["personal"]
	if !ok {
		t.Fatal("by_domain: missing 'personal'")
	}
	if personalStats.Cancelled != 1 {
		t.Errorf("by_domain[personal].cancelled: want 1, got %d", personalStats.Cancelled)
	}
}

// TestAggregateMetrics_LastParam verifies that ?last=N limits the recent list.
func TestAggregateMetrics_LastParam(t *testing.T) {
	dir := t.TempDir()

	// Write 5 records.
	var lines []string
	for i := 1; i <= 5; i++ {
		lines = append(lines, fmt.Sprintf(
			`{"workspace_id":"ws%d","domain":"dev","task":"task %d","started_at":"2026-01-0%dT10:00:00Z","duration_s":60,"status":"success"}`,
			i, i, i,
		))
	}
	metricsPath := dir + "/metrics.jsonl"
	if err := os.WriteFile(metricsPath, []byte(strings.Join(lines, "\n")+"\n"), 0600); err != nil {
		t.Fatalf("write metrics.jsonl: %v", err)
	}

	resp, err := aggregateMetrics(metricsPath, dir+"/workspaces", dir+"/events", 3)
	if err != nil {
		t.Fatalf("aggregateMetrics: %v", err)
	}

	if len(resp.Recent) != 3 {
		t.Errorf("recent: want 3, got %d", len(resp.Recent))
	}
	// Newest first: ws5, ws4, ws3
	if resp.Recent[0].WorkspaceID != "ws5" {
		t.Errorf("recent[0]: want ws5, got %q", resp.Recent[0].WorkspaceID)
	}
}

// TestHandleMetrics_LastQueryParam verifies the ?last query param is wired through.
func TestHandleMetrics_LastQueryParam(t *testing.T) {
	srv, _ := newTestServer(t)

	req := httptest.NewRequest(http.MethodGet, "/api/metrics?last=5", nil)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("metrics ?last: want 200, got %d", rec.Code)
	}
}

// ---------- GET /api/personas --------------------------------------------

// TestListPersonas_Returns200 verifies the endpoint responds with 200 and
// a JSON array regardless of whether personas are loaded on this machine.
func TestListPersonas_Returns200(t *testing.T) {
	srv, _ := newTestServer(t)

	req := httptest.NewRequest(http.MethodGet, "/api/personas", nil)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("list personas: want 200, got %d", rec.Code)
	}
	ct := rec.Header().Get("Content-Type")
	if !strings.HasPrefix(ct, "application/json") {
		t.Errorf("list personas: Content-Type want application/json, got %q", ct)
	}
	// Body must be a valid JSON array.
	var result []json.RawMessage
	if err := json.Unmarshal(rec.Body.Bytes(), &result); err != nil {
		t.Fatalf("list personas: body is not a JSON array: %v\nbody: %s", err, rec.Body.String())
	}
}

// TestListPersonas_FieldShape verifies that every returned object carries all
// required fields with sensible zero values when no usage data exists.
func TestListPersonas_FieldShape(t *testing.T) {
	srv, _ := newTestServer(t)

	req := httptest.NewRequest(http.MethodGet, "/api/personas", nil)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	var items []PersonaResponse
	if err := json.Unmarshal(rec.Body.Bytes(), &items); err != nil {
		t.Fatalf("list personas: unmarshal: %v", err)
	}
	for _, p := range items {
		if p.Name == "" {
			t.Errorf("persona missing name: %+v", p)
		}
		if p.Color == "" {
			t.Errorf("persona %q missing color", p.Name)
		}
		if p.WhenToUse == nil {
			t.Errorf("persona %q: when_to_use must not be nil", p.Name)
		}
		if p.MissionsAssigned < 0 {
			t.Errorf("persona %q: missions_assigned < 0", p.Name)
		}
		if p.SuccessRate < 0 || p.SuccessRate > 1 {
			t.Errorf("persona %q: success_rate %f out of [0,1]", p.Name, p.SuccessRate)
		}
		if p.AvgDurationSeconds < 0 {
			t.Errorf("persona %q: avg_duration_seconds < 0", p.Name)
		}
	}
}

// TestListPersonas_SortedByName verifies alphabetical ordering.
func TestListPersonas_SortedByName(t *testing.T) {
	srv, _ := newTestServer(t)

	req := httptest.NewRequest(http.MethodGet, "/api/personas", nil)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	var items []PersonaResponse
	if err := json.Unmarshal(rec.Body.Bytes(), &items); err != nil {
		t.Fatalf("list personas: unmarshal: %v", err)
	}
	for i := 1; i < len(items); i++ {
		if items[i].Name < items[i-1].Name {
			t.Errorf("personas not sorted: %q before %q", items[i-1].Name, items[i].Name)
		}
	}
}

// ---------- aggregatePersonaStats unit tests ------------------------------

func TestAggregatePersonaStats_MissingDir(t *testing.T) {
	stats, err := aggregatePersonaStats("/nonexistent/path/that/cannot/exist/xyz987")
	if err != nil {
		t.Fatalf("aggregatePersonaStats: want nil err for missing dir, got %v", err)
	}
	if len(stats) != 0 {
		t.Errorf("aggregatePersonaStats: want empty map for missing dir, got %d entries", len(stats))
	}
}

func TestAggregatePersonaStats_EmptyDir(t *testing.T) {
	stats, err := aggregatePersonaStats(t.TempDir())
	if err != nil {
		t.Fatalf("aggregatePersonaStats empty dir: %v", err)
	}
	if len(stats) != 0 {
		t.Errorf("aggregatePersonaStats empty dir: want 0 entries, got %d", len(stats))
	}
}

func TestAggregatePersonaStats_MultipleFiles(t *testing.T) {
	dir := t.TempDir()

	// Two missions sharing one persona; a non-JSONL file must be silently skipped.
	writePersonaJSONL(t, dir, "m1.jsonl", []event.Event{
		{Type: event.PhaseStarted, Data: map[string]any{"persona": "architect"}},
		{Type: event.MissionCompleted, Data: map[string]any{"duration": "1m"}},
	})
	writePersonaJSONL(t, dir, "m2.jsonl", []event.Event{
		{Type: event.PhaseStarted, Data: map[string]any{"persona": "architect"}},
		{Type: event.MissionFailed},
	})
	os.WriteFile(filepath.Join(dir, "notes.txt"), []byte("ignore me"), 0600) //nolint:errcheck

	stats, err := aggregatePersonaStats(dir)
	if err != nil {
		t.Fatalf("aggregatePersonaStats: %v", err)
	}
	u := stats["architect"]
	if u == nil {
		t.Fatal("architect missing from aggregated stats")
	}
	if u.missionsAssigned != 2 {
		t.Errorf("missionsAssigned: want 2, got %d", u.missionsAssigned)
	}
	if u.succeeded != 1 {
		t.Errorf("succeeded: want 1, got %d", u.succeeded)
	}
	if u.total != 2 {
		t.Errorf("total: want 2, got %d", u.total)
	}
}

// ---------- processMissionLog unit tests ----------------------------------

func TestProcessMissionLog_CompletedMission(t *testing.T) {
	path := writePersonaJSONL(t, t.TempDir(), "m.jsonl", []event.Event{
		{Type: event.PhaseStarted, Data: map[string]any{"persona": "architect"}},
		{Type: event.PhaseStarted, Data: map[string]any{"persona": "backend-engineer"}},
		{Type: event.MissionCompleted, Data: map[string]any{"duration": "2m30s"}},
	})

	stats := make(map[string]*personaUsage)
	if err := processMissionLog(path, stats); err != nil {
		t.Fatalf("processMissionLog: %v", err)
	}

	arch := stats["architect"]
	if arch == nil {
		t.Fatal("architect missing from stats")
	}
	if arch.missionsAssigned != 1 {
		t.Errorf("architect.missionsAssigned: want 1, got %d", arch.missionsAssigned)
	}
	if arch.succeeded != 1 {
		t.Errorf("architect.succeeded: want 1, got %d", arch.succeeded)
	}
	if arch.total != 1 {
		t.Errorf("architect.total: want 1, got %d", arch.total)
	}
	if arch.active {
		t.Error("architect.active: want false for completed mission")
	}
	if len(arch.durations) != 1 || arch.durations[0] != 150*time.Second {
		t.Errorf("architect.durations: want [2m30s], got %v", arch.durations)
	}

	be := stats["backend-engineer"]
	if be == nil {
		t.Fatal("backend-engineer missing from stats")
	}
	if be.missionsAssigned != 1 || be.succeeded != 1 {
		t.Errorf("backend-engineer: want 1 assigned/1 succeeded, got %d/%d", be.missionsAssigned, be.succeeded)
	}
}

func TestProcessMissionLog_FailedMission(t *testing.T) {
	path := writePersonaJSONL(t, t.TempDir(), "m.jsonl", []event.Event{
		{Type: event.PhaseStarted, Data: map[string]any{"persona": "qa-engineer"}},
		{Type: event.MissionFailed},
	})

	stats := make(map[string]*personaUsage)
	if err := processMissionLog(path, stats); err != nil {
		t.Fatalf("processMissionLog: %v", err)
	}

	u := stats["qa-engineer"]
	if u == nil {
		t.Fatal("qa-engineer missing from stats")
	}
	if u.missionsAssigned != 1 {
		t.Errorf("missionsAssigned: want 1, got %d", u.missionsAssigned)
	}
	if u.succeeded != 0 {
		t.Errorf("succeeded: want 0 for failed mission, got %d", u.succeeded)
	}
	if u.total != 1 {
		t.Errorf("total: want 1, got %d", u.total)
	}
	if u.active {
		t.Error("active: want false for failed mission")
	}
}

func TestProcessMissionLog_ActiveMission(t *testing.T) {
	// DAGPhaseDispatched also registers a persona; no terminal event → active.
	path := writePersonaJSONL(t, t.TempDir(), "m.jsonl", []event.Event{
		{Type: event.DAGPhaseDispatched, Data: map[string]any{"persona": "researcher"}},
	})

	stats := make(map[string]*personaUsage)
	if err := processMissionLog(path, stats); err != nil {
		t.Fatalf("processMissionLog: %v", err)
	}

	u := stats["researcher"]
	if u == nil {
		t.Fatal("researcher missing from stats")
	}
	if !u.active {
		t.Error("active: want true for in-progress mission")
	}
	if u.total != 0 {
		t.Errorf("total: want 0 for active mission (no terminal event), got %d", u.total)
	}
}

func TestProcessMissionLog_NoPersonaEvents(t *testing.T) {
	path := writePersonaJSONL(t, t.TempDir(), "m.jsonl", []event.Event{
		{Type: event.MissionStarted},
		{Type: event.MissionCompleted, Data: map[string]any{"duration": "1m"}},
	})

	stats := make(map[string]*personaUsage)
	if err := processMissionLog(path, stats); err != nil {
		t.Fatalf("processMissionLog: %v", err)
	}
	if len(stats) != 0 {
		t.Errorf("stats: want empty (no persona events), got %d entries", len(stats))
	}
}

func TestProcessMissionLog_DurationNotRecordedOnFail(t *testing.T) {
	path := writePersonaJSONL(t, t.TempDir(), "m.jsonl", []event.Event{
		{Type: event.PhaseStarted, Data: map[string]any{"persona": "devops-engineer"}},
		{Type: event.MissionFailed},
	})

	stats := make(map[string]*personaUsage)
	if err := processMissionLog(path, stats); err != nil {
		t.Fatalf("processMissionLog: %v", err)
	}
	u := stats["devops-engineer"]
	if u == nil {
		t.Fatal("devops-engineer missing from stats")
	}
	if len(u.durations) != 0 {
		t.Errorf("durations: want none recorded on failure, got %v", u.durations)
	}
}

// ---------- personaColor unit tests --------------------------------------

func TestPersonaColor_KnownPersonas(t *testing.T) {
	cases := []struct {
		name  string
		color string
	}{
		{"architect", "#6366f1"},
		{"backend-engineer", "#10b981"},
		{"frontend-engineer", "#06b6d4"},
		{"qa-engineer", "#ef4444"},
		{"researcher", "#0ea5e9"},
		{"security-auditor", "#dc2626"},
		{"devops-engineer", "#8b5cf6"},
	}
	for _, tc := range cases {
		got := personaColor(tc.name)
		if got != tc.color {
			t.Errorf("personaColor(%q): want %q, got %q", tc.name, tc.color, got)
		}
	}
}

func TestPersonaColor_UnknownFallback(t *testing.T) {
	const gray = "#6b7280"
	got := personaColor("unknown-persona-xyz")
	if got != gray {
		t.Errorf("personaColor(unknown): want gray %s, got %q", gray, got)
	}
}

// ---------- POST /api/personas/reload ------------------------------------

// TestReloadPersonas_Endpoint verifies that POST /api/personas/reload returns
// {"reloaded":true,"count":N} and picks up new persona files added after Load.
func TestReloadPersonas_Endpoint(t *testing.T) {
	dir := t.TempDir()
	names := []string{"alpha", "beta"}
	for _, name := range names {
		content := fmt.Sprintf(
			"# %s\n\n## Identity\nX\n\n## Goal\nX\n\n## When to Use\n- %s tasks\n\n## When NOT to Use\n- nothing\n\n## Self-Check\n- ok\n",
			name, name,
		)
		if err := os.WriteFile(filepath.Join(dir, name+".md"), []byte(content), 0644); err != nil {
			t.Fatalf("write persona %s: %v", name, err)
		}
	}

	persona.SetDir(dir)
	t.Cleanup(func() { persona.SetDir("") })

	if err := persona.Load(); err != nil {
		t.Fatalf("Load: %v", err)
	}

	srv, _ := newTestServer(t)

	req := httptest.NewRequest(http.MethodPost, "/api/personas/reload", nil)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("reload personas: want 200, got %d; body: %s", rec.Code, rec.Body.String())
	}
	ct := rec.Header().Get("Content-Type")
	if !strings.HasPrefix(ct, "application/json") {
		t.Errorf("reload personas: Content-Type want application/json, got %q", ct)
	}

	var resp map[string]any
	if err := json.Unmarshal(rec.Body.Bytes(), &resp); err != nil {
		t.Fatalf("reload personas: unmarshal: %v", err)
	}
	reloaded, _ := resp["reloaded"].(bool)
	if !reloaded {
		t.Errorf("reload personas: want reloaded=true, got %v", resp["reloaded"])
	}
	count, _ := resp["count"].(float64)
	if int(count) != len(names) {
		t.Errorf("reload personas: want count=%d, got %v", len(names), resp["count"])
	}
}

// TestReloadPersonas_PicksUpNewFile verifies that a persona added after Load is
// visible after calling POST /api/personas/reload.
func TestReloadPersonas_PicksUpNewFile(t *testing.T) {
	dir := t.TempDir()
	writePersona := func(name, content string) {
		t.Helper()
		if err := os.WriteFile(filepath.Join(dir, name+".md"), []byte(content), 0644); err != nil {
			t.Fatalf("write persona %s: %v", name, err)
		}
	}
	writePersona("delta", "# Delta\n\n## Identity\nX\n\n## Goal\nX\n\n## When to Use\n- delta tasks\n\n## When NOT to Use\n- nothing\n\n## Self-Check\n- ok\n")

	persona.SetDir(dir)
	t.Cleanup(func() { persona.SetDir("") })

	if err := persona.Load(); err != nil {
		t.Fatalf("Load: %v", err)
	}

	// Add a second persona after initial load.
	writePersona("epsilon", "# Epsilon\n\n## Identity\nX\n\n## Goal\nX\n\n## When to Use\n- epsilon tasks\n\n## When NOT to Use\n- nothing\n\n## Self-Check\n- ok\n")

	srv, _ := newTestServer(t)

	req := httptest.NewRequest(http.MethodPost, "/api/personas/reload", nil)
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusOK {
		t.Fatalf("reload: want 200, got %d", rec.Code)
	}

	var resp map[string]any
	if err := json.Unmarshal(rec.Body.Bytes(), &resp); err != nil {
		t.Fatalf("reload: unmarshal: %v", err)
	}
	count, _ := resp["count"].(float64)
	if int(count) != 2 {
		t.Errorf("reload: want count=2 (delta+epsilon), got %v", resp["count"])
	}
}

// writePersonaJSONL writes events as JSONL to dir/name and returns the path.
func writePersonaJSONL(t *testing.T, dir, name string, events []event.Event) string {
	t.Helper()
	path := filepath.Join(dir, name)
	f, err := os.Create(path)
	if err != nil {
		t.Fatalf("create %s: %v", path, err)
	}
	defer f.Close()
	enc := json.NewEncoder(f)
	for _, ev := range events {
		if err := enc.Encode(ev); err != nil {
			t.Fatalf("encoding event: %v", err)
		}
	}
	return path
}

// testPlan builds a small Plan for DAG tests:
//
//	phase-1 ──┐
//	           ▼
//	phase-2 ──▶ phase-3
func testPlan() *core.Plan {
	return &core.Plan{
		ID:   "test-plan",
		Task: "test task",
		Phases: []*core.Phase{
			{ID: "phase-1", Name: "Setup", Persona: "architect", Status: core.StatusCompleted},
			{ID: "phase-2", Name: "Build", Persona: "implementer", Status: core.StatusCompleted},
			{
				ID:           "phase-3",
				Name:         "Review",
				Persona:      "qa",
				Status:       core.StatusPending,
				Dependencies: []string{"phase-1", "phase-2"},
			},
		},
	}
}

// ---------- POST /api/missions/run ----------------------------------------

// TestRunMission_AcceptedShape verifies that the endpoint returns 202 with the
// correct JSON shape: status=accepted, request_id (non-empty), task echoed back.
func TestRunMission_AcceptedShape(t *testing.T) {
	srv, _ := newTestServer(t)

	body := `{"task":"build a feature"}`
	req := httptest.NewRequest(http.MethodPost, "/api/missions/run", strings.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusAccepted {
		t.Fatalf("run mission: want 202, got %d (body: %s)", rec.Code, rec.Body)
	}

	var result map[string]string
	if err := json.Unmarshal(rec.Body.Bytes(), &result); err != nil {
		t.Fatalf("run mission: unmarshal: %v", err)
	}
	if result["status"] != "accepted" {
		t.Errorf("run mission: status: want 'accepted', got %q", result["status"])
	}
	if result["request_id"] == "" {
		t.Error("run mission: request_id must not be empty")
	}
	if result["task"] != "build a feature" {
		t.Errorf("run mission: task echo: want 'build a feature', got %q", result["task"])
	}
	// The old fake field must not be present.
	if _, ok := result["mission_id"]; ok {
		t.Error("run mission: response must not contain 'mission_id' (truthfulness regression)")
	}
}

// TestRunMission_EmptyTask_Rejected verifies that a missing or empty task returns 400.
func TestRunMission_EmptyTask_Rejected(t *testing.T) {
	srv, _ := newTestServer(t)

	cases := []struct {
		name string
		body string
	}{
		{"empty task field", `{"task":""}`},
		{"missing task field", `{}`},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			req := httptest.NewRequest(http.MethodPost, "/api/missions/run", strings.NewReader(tc.body))
			req.Header.Set("Content-Type", "application/json")
			rec := httptest.NewRecorder()
			srv.srv.Handler.ServeHTTP(rec, req)

			if rec.Code != http.StatusBadRequest {
				t.Errorf("%s: want 400, got %d", tc.name, rec.Code)
			}
		})
	}
}

// TestRunMission_InvalidJSON_Rejected verifies that malformed JSON returns 400.
func TestRunMission_InvalidJSON_Rejected(t *testing.T) {
	srv, _ := newTestServer(t)

	req := httptest.NewRequest(http.MethodPost, "/api/missions/run", strings.NewReader("{not json"))
	req.Header.Set("Content-Type", "application/json")
	rec := httptest.NewRecorder()
	srv.srv.Handler.ServeHTTP(rec, req)

	if rec.Code != http.StatusBadRequest {
		t.Fatalf("run mission invalid json: want 400, got %d", rec.Code)
	}
}

// TestRunMission_RequestIDsAreUnique verifies that two sequential launches
// produce different request_ids.
func TestRunMission_RequestIDsAreUnique(t *testing.T) {
	srv, _ := newTestServer(t)

	ids := make(map[string]bool)
	for i := 0; i < 5; i++ {
		body := `{"task":"test task"}`
		req := httptest.NewRequest(http.MethodPost, "/api/missions/run", strings.NewReader(body))
		req.Header.Set("Content-Type", "application/json")
		rec := httptest.NewRecorder()
		srv.srv.Handler.ServeHTTP(rec, req)

		if rec.Code != http.StatusAccepted {
			t.Fatalf("launch %d: want 202, got %d", i, rec.Code)
		}
		var result map[string]string
		if err := json.Unmarshal(rec.Body.Bytes(), &result); err != nil {
			t.Fatalf("launch %d: unmarshal: %v", i, err)
		}
		id := result["request_id"]
		if ids[id] {
			t.Fatalf("launch %d: duplicate request_id %q", i, id)
		}
		ids[id] = true
	}
}

// ---------- isTerminalMissionEvent ----------------------------------------

// TestIsTerminalMissionEvent verifies the helper correctly identifies terminal
// and non-terminal event types.
func TestIsTerminalMissionEvent(t *testing.T) {
	cases := []struct {
		typ      event.EventType
		terminal bool
	}{
		{event.MissionCompleted, true},
		{event.MissionFailed, true},
		{event.MissionCancelled, true},
		{event.MissionStarted, false},
		{event.PhaseCompleted, false},
		{event.PhaseStarted, false},
		{event.WorkerOutput, false},
	}
	for _, tc := range cases {
		got := isTerminalMissionEvent(tc.typ)
		if got != tc.terminal {
			t.Errorf("isTerminalMissionEvent(%q): want %v, got %v", tc.typ, tc.terminal, got)
		}
	}
}

// ---------- SSE per-mission stream: stream:done on terminal event ----------

// TestMissionStream_ClosesAfterCompleted verifies that a per-mission SSE stream
// sends a final stream:done event and closes after a mission.completed event.
func TestMissionStream_ClosesAfterCompleted(t *testing.T) {
	testMissionStreamClosesOnTerminal(t, event.MissionCompleted)
}

func TestMissionStream_ClosesAfterFailed(t *testing.T) {
	testMissionStreamClosesOnTerminal(t, event.MissionFailed)
}

func TestMissionStream_ClosesAfterCancelled(t *testing.T) {
	testMissionStreamClosesOnTerminal(t, event.MissionCancelled)
}

// sseReadResult carries a scanner result and an EOF flag to avoid treating
// blank SSE separator lines (empty text, ok=true) as end-of-stream.
type sseReadResult struct {
	text string
	eof  bool
}

func testMissionStreamClosesOnTerminal(t *testing.T, terminalType event.EventType) {
	t.Helper()
	srv, bus := newTestServer(t)

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer ln.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	go srv.Serve(ctx, ln) //nolint:errcheck

	url := fmt.Sprintf("http://%s/api/missions/test-mission/stream", ln.Addr())
	req, _ := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	req.Header.Set("Last-Event-ID", "0")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		t.Fatalf("mission stream: want 200, got %d", resp.StatusCode)
	}

	// Publish the terminal event after we're connected.
	go func() {
		time.Sleep(50 * time.Millisecond)
		bus.Publish(makeEventWithMission("term", terminalType, "test-mission"))
	}()

	// Read SSE lines until we see stream:done or the connection closes.
	scanner := bufio.NewScanner(resp.Body)
	deadline := time.After(5 * time.Second)
	seenStreamDone := false

	for {
		lineCh := make(chan sseReadResult, 1)
		go func() {
			if scanner.Scan() {
				lineCh <- sseReadResult{text: scanner.Text()}
			} else {
				lineCh <- sseReadResult{eof: true}
			}
		}()
		select {
		case <-deadline:
			t.Fatalf("timed out; seenStreamDone=%v", seenStreamDone)
		case res := <-lineCh:
			if res.eof {
				// Connection closed by server.
				if !seenStreamDone {
					t.Error("stream closed without stream:done event")
				}
				return
			}
			if res.text == "event: stream:done" {
				seenStreamDone = true
			}
			if seenStreamDone {
				return // success
			}
		}
	}
}

// TestGlobalStream_DoesNotCloseAfterTerminalEvent verifies that the global
// /api/events stream is NOT closed after a terminal mission event — it is a
// firehose and must remain open.
func TestGlobalStream_DoesNotCloseAfterTerminalEvent(t *testing.T) {
	srv, bus := newTestServer(t)

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer ln.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	go srv.Serve(ctx, ln) //nolint:errcheck

	url := fmt.Sprintf("http://%s/api/events", ln.Addr())
	req, _ := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	req.Header.Set("Last-Event-ID", "0")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	defer resp.Body.Close()

	// Publish a terminal event, then a non-terminal event after a short delay.
	// If the global stream incorrectly closes, we won't receive the second event.
	go func() {
		time.Sleep(50 * time.Millisecond)
		bus.Publish(makeEventWithMission("done", event.MissionCompleted, "m1"))
		time.Sleep(50 * time.Millisecond)
		bus.Publish(makeEventWithMission("after", event.WorkerOutput, "m2"))
	}()

	// We expect to see both events — the stream should not have closed.
	seen := collectSSEEvents(t, resp, 2, 5*time.Second)
	if len(seen) < 2 {
		t.Errorf("global stream: want ≥2 events after terminal, got %d: %v", len(seen), seen)
	}
}

// ---------- shutdownProcesses: no panic on empty table --------------------

// TestShutdownProcesses_EmptyTable verifies that shutdownProcesses does not
// panic or deadlock when no processes are tracked.
func TestShutdownProcesses_EmptyTable(t *testing.T) {
	srv, _ := newTestServer(t)
	// Should not panic or block.
	srv.shutdownProcesses()
}
