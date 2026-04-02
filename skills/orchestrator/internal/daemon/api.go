package daemon

import (
	"bufio"
	"bytes"
	"context"
	"crypto/rand"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/config"
	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/metrics"
	"github.com/joeyhipolito/orchestrator-cli/internal/persona"
	_ "modernc.org/sqlite"
)

// runningMission tracks a background orchestrator process launched via POST /api/missions/run.
// done is closed by the goroutine that owns cmd.Wait(); shutdownProcesses waits on it
// instead of calling cmd.Wait() again to avoid a data race.
type runningMission struct {
	cmd       *exec.Cmd
	requestID string
	task      string
	startedAt time.Time
	done      chan struct{} // closed when cmd.Wait returns
}

// ChannelStatus is the JSON-serializable health record for one notification channel.
type ChannelStatus struct {
	Name          string    `json:"name"`
	Platform      string    `json:"platform"`
	Configured    bool      `json:"configured"`
	Active        bool      `json:"active"`
	LastEventSent time.Time `json:"last_event_sent,omitempty"`
	ErrorCount    int       `json:"error_count"`
	LastError     string    `json:"last_error,omitempty"`
}

// channelTracker wraps ChannelStatus with a mutex for concurrent updates from
// the notifier goroutines.
type channelTracker struct {
	mu     sync.Mutex
	status ChannelStatus
}

func (t *channelTracker) recordEvent() {
	t.mu.Lock()
	t.status.LastEventSent = time.Now()
	t.mu.Unlock()
}

func (t *channelTracker) recordError(err error) {
	t.mu.Lock()
	t.status.ErrorCount++
	t.status.LastError = err.Error()
	t.mu.Unlock()
}

func (t *channelTracker) snapshot() ChannelStatus {
	t.mu.Lock()
	defer t.mu.Unlock()
	return t.status
}

// APIServer serves the SSE event stream and a small REST API.
type APIServer struct {
	bus       *event.Bus
	dropper   event.DropReporter
	cfg       Config
	srv       *http.Server
	liveState *event.LiveState
	chat      *chatStore
	metricsDB *metrics.DB // nil if metrics.db is unavailable

	procMu    sync.Mutex
	procTable map[string]*runningMission // keyed by requestID

	channelMu sync.RWMutex
	channels  []*channelTracker
}

// NewAPIServer constructs an APIServer backed by bus with the given config.
func NewAPIServer(bus *event.Bus, dropper event.DropReporter, cfg Config) *APIServer {
	s := &APIServer{
		bus:       bus,
		dropper:   dropper,
		cfg:       cfg,
		liveState: event.NewLiveState(bus),
		chat:      newChatStore(),
		procTable: make(map[string]*runningMission),
	}

	// Open metrics.db for richer persona stats. Non-fatal: personas still work
	// without it, falling back to event-log aggregation.
	if mdb, err := metrics.InitDB(""); err == nil {
		s.metricsDB = mdb
	}

	mux := http.NewServeMux()
	auth := newAuthMiddleware(cfg, nil)

	// SSE stream — primary consumer endpoint.
	// Supports Last-Event-ID reconnect and sends a ": keepalive" comment
	// every 15 seconds to prevent proxy/LB from closing idle connections.
	mux.Handle("GET /api/events", auth(http.HandlerFunc(s.handleSSE)))

	// REST — list known missions with event-log metadata.
	mux.Handle("GET /api/missions", auth(http.HandlerFunc(s.handleListMissions)))

	// REST — single mission detail (metadata + plan overview from checkpoint).
	mux.Handle("GET /api/missions/{id}", auth(http.HandlerFunc(s.handleMission)))

	// REST — execution DAG for a mission (nodes=phases, edges=dependencies).
	mux.Handle("GET /api/missions/{id}/dag", auth(http.HandlerFunc(s.handleMissionDAG)))

	// SSE — per-mission live stream, filtered to a single mission's events.
	mux.Handle("GET /api/missions/{id}/stream", auth(http.HandlerFunc(s.handleMissionStream)))

	// SSE — global worker output stream: all worker.output events across all missions,
	// projected to a compact payload {mission_id, phase, persona, tool_name, file_path,
	// chunk, streaming, duration}. Supports optional ?mission_id= filter.
	mux.Handle("GET /api/missions/live", auth(http.HandlerFunc(s.handleGlobalLive)))

	// SSE — per-mission worker output stream: only worker.output events, projected
	// to a compact payload {mission_id, phase, persona, tool_name, file_path, chunk, streaming, duration}.
	mux.Handle("GET /api/missions/{id}/live", auth(http.HandlerFunc(s.handleMissionLive)))

	// REST — replay a mission's sanitized events from its JSONL log.
	mux.Handle("GET /api/missions/{id}/events", auth(http.HandlerFunc(s.handleReplayMission)))

	// REST — cancel a running mission (sentinel + SIGTERM or manual cleanup).
	mux.Handle("POST /api/missions/{id}/cancel", auth(http.HandlerFunc(s.handleCancelMission)))

	// REST — force-update a mission's checkpoint status (stalled/failed/completed).
	mux.Handle("PATCH /api/missions/{id}/status", auth(http.HandlerFunc(s.handleUpdateMissionStatus)))

	// Ingestion — accept events via HTTP POST as an alternative to UDS.
	mux.Handle("POST /api/events", auth(http.HandlerFunc(s.handleIngestEvents)))

	// REST — persona catalog with aggregated usage stats.
	mux.Handle("GET /api/personas", auth(http.HandlerFunc(s.handleListPersonas)))

	// REST — single persona detail with recent missions and success trend.
	mux.Handle("GET /api/personas/{name}", auth(http.HandlerFunc(s.handleGetPersona)))

	// REST — manually trigger a persona catalog hot-reload.
	mux.Handle("POST /api/personas/reload", auth(http.HandlerFunc(s.handleReloadPersonas)))

	// REST — aggregate mission statistics (totals, breakdowns, recent list).
	mux.Handle("GET /api/metrics", auth(http.HandlerFunc(s.handleMetrics)))

	// REST — decomposition findings from learnings.db (counts by type, recent, trends).
	mux.Handle("GET /api/decomposition-findings", auth(http.HandlerFunc(s.handleDecompositionFindings)))

	// REST — nen ability findings from ~/.alluka/nen/findings.db.
	mux.Handle("GET /api/findings", auth(http.HandlerFunc(s.handleFindings)))

	// REST — spawn a new mission via orchestrator run in a background process.
	mux.Handle("POST /api/missions/run", auth(http.HandlerFunc(s.handleRunMission)))

	// Chat — conversation management and streaming.
	mux.Handle("GET /api/chat", auth(http.HandlerFunc(s.handleListConversations)))
	mux.Handle("POST /api/chat", auth(http.HandlerFunc(s.handleChat)))
	mux.Handle("GET /api/chat/{id}", auth(http.HandlerFunc(s.handleGetConversation)))
	mux.Handle("DELETE /api/chat/{id}", auth(http.HandlerFunc(s.handleDeleteConversation)))
	mux.Handle("GET /api/chat/{id}/stream", auth(http.HandlerFunc(s.handleChatStream)))

	// REST — discover plugins by scanning plugins/*/plugin.json.
	mux.Handle("GET /api/plugins", auth(http.HandlerFunc(s.handleListPlugins)))
	mux.Handle("GET /api/plugins/{name}", auth(http.HandlerFunc(s.handlePluginInfo)))
	mux.Handle("GET /api/plugins/{name}/status", auth(http.HandlerFunc(s.handlePluginQuery("status"))))
	mux.Handle("GET /api/plugins/{name}/items", auth(http.HandlerFunc(s.handlePluginQuery("items"))))
	mux.Handle("POST /api/plugins/{name}/action", auth(http.HandlerFunc(s.handlePluginAction)))

	// REST — cleanup orphaned worktrees via orchestrator cleanup --worktrees.
	mux.Handle("POST /api/cleanup", auth(http.HandlerFunc(s.handleCleanup)))

	// REST — per-channel notification health (configured, active, last event, errors).
	mux.Handle("GET /api/channels", auth(http.HandlerFunc(s.handleChannels)))

	// Health check — unauthenticated.
	mux.HandleFunc("GET /api/health", s.handleHealth)

	s.srv = &http.Server{
		Handler:      corsMiddleware(mux),
		ReadTimeout:  10 * time.Second,
		WriteTimeout: 0, // SSE streams are unbounded; disable write timeout.
		IdleTimeout:  120 * time.Second,
	}
	return s
}

// Close releases the LiveState bus subscription.
func (s *APIServer) Close() {
	s.liveState.Close()
}

// Serve runs the HTTP server using the pre-bound listener until ctx is cancelled.
func (s *APIServer) Serve(ctx context.Context, ln net.Listener) error {
	go func() {
		<-ctx.Done()
		s.shutdownProcesses()
		shutCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		s.srv.Shutdown(shutCtx) //nolint:errcheck
	}()

	if err := s.srv.Serve(ln); err != nil && err != http.ErrServerClosed {
		return fmt.Errorf("serving: %w", err)
	}
	return nil
}

// shutdownProcesses sends SIGTERM to all tracked child processes and waits up
// to 10 seconds before sending SIGKILL to any survivors.
func (s *APIServer) shutdownProcesses() {
	s.procMu.Lock()
	procs := make([]*runningMission, 0, len(s.procTable))
	for _, m := range s.procTable {
		procs = append(procs, m)
	}
	s.procMu.Unlock()

	if len(procs) == 0 {
		return
	}

	for _, m := range procs {
		if m.cmd.Process != nil {
			// Kill the whole process group so orchestrator children are also terminated.
			syscall.Kill(-m.cmd.Process.Pid, syscall.SIGTERM) //nolint:errcheck
		}
	}

	done := make(chan struct{})
	go func() {
		defer close(done)
		for _, m := range procs {
			<-m.done
		}
	}()

	select {
	case <-done:
	case <-time.After(10 * time.Second):
		for _, m := range procs {
			if m.cmd.Process != nil {
				syscall.Kill(-m.cmd.Process.Pid, syscall.SIGKILL) //nolint:errcheck
			}
		}
		<-done
	}
}

const sseKeepaliveInterval = 15 * time.Second

// sseFilter holds optional query-param filters for SSE streams.
// Zero value passes all events.
type sseFilter struct {
	types     map[event.EventType]bool // nil = accept all types
	missionID string                   // empty = accept all missions
}

// match returns true if ev passes the filter criteria.
func (f sseFilter) match(ev event.Event) bool {
	if f.missionID != "" && ev.MissionID != f.missionID {
		return false
	}
	if f.types != nil && !f.types[ev.Type] {
		return false
	}
	return true
}

// parseSSEFilter extracts optional type and mission_id filters from query params.
//
//	?types=worker.output,worker.completed  — comma-separated event types
//	?mission_id=20260222-abc123            — single mission ID
func parseSSEFilter(r *http.Request) sseFilter {
	var f sseFilter
	if raw := r.URL.Query().Get("types"); raw != "" {
		f.types = make(map[event.EventType]bool)
		for _, t := range strings.Split(raw, ",") {
			t = strings.TrimSpace(t)
			if t != "" {
				f.types[event.EventType(t)] = true
			}
		}
	}
	if mid := r.URL.Query().Get("mission_id"); mid != "" {
		f.missionID = mid
	}
	return f
}

// handleSSE streams sanitized events as SSE to the client.
//
// Reconnect support: Last-Event-ID header carries the sequence number of the
// last event received; any buffered events with a higher sequence are replayed
// before switching to live streaming.
//
// Keepalive: a ": keepalive" comment is sent every 15 seconds so that proxies
// and load balancers do not close idle connections.
//
// Optional query params:
//
//	?types=worker.output,phase.completed  — only receive these event types
//	?mission_id=<id>                      — only receive events for this mission
func (s *APIServer) handleSSE(w http.ResponseWriter, r *http.Request) {
	filter := parseSSEFilter(r)
	s.streamSSE(w, r, filter)
}

// handleMissionStream streams sanitized events for a single mission.
// Equivalent to GET /api/events?mission_id={id} but scoped via the URL path
// so per-mission dashboards can subscribe without knowing the query-param API.
func (s *APIServer) handleMissionStream(w http.ResponseWriter, r *http.Request) {
	id, ok := parseMissionID(w, r)
	if !ok {
		return
	}
	filter := parseSSEFilter(r)
	filter.missionID = id // path param overrides any query param
	s.streamSSE(w, r, filter)
}

// liveOutputEvent is the projected payload emitted by the /live SSE endpoints.
// It extracts only the fields a dashboard needs from worker.output events.
type liveOutputEvent struct {
	MissionID string `json:"mission_id,omitempty"`
	Phase     string `json:"phase"`
	Persona   string `json:"persona"`
	ToolName  string `json:"tool_name,omitempty"`
	FilePath  string `json:"file_path,omitempty"`
	Chunk     string `json:"chunk,omitempty"`
	Streaming bool   `json:"streaming"`
	Duration  string `json:"duration,omitempty"`
}

// handleMissionLive streams worker.output events for a single mission as SSE.
// Each event is projected to a compact payload; non-worker-output events are
// silently dropped. The stream closes after a terminal mission event.
func (s *APIServer) handleMissionLive(w http.ResponseWriter, r *http.Request) {
	id, ok := parseMissionID(w, r)
	if !ok {
		return
	}

	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	w.Header().Set("Connection", "keep-alive")
	w.Header().Set("X-Accel-Buffering", "no")
	applyCORS(w, r)

	flusher, ok := w.(http.Flusher)
	if !ok {
		http.Error(w, "streaming not supported", http.StatusInternalServerError)
		return
	}

	subID, ch := s.bus.Subscribe()
	defer s.bus.Unsubscribe(subID)

	// Replay buffered worker.output events the client may have missed.
	for _, ev := range s.bus.EventsSince(0) {
		if ev.MissionID == id && ev.Type == event.WorkerOutput {
			writeLiveEvent(w, ev)
		}
	}
	flusher.Flush()

	ticker := time.NewTicker(sseKeepaliveInterval)
	defer ticker.Stop()

	ctx := r.Context()
	for {
		select {
		case <-ctx.Done():
			return

		case <-ticker.C:
			fmt.Fprintf(w, ": keepalive\n\n")
			flusher.Flush()

		case ev, ok := <-ch:
			if !ok {
				return
			}
			if ev.MissionID != id {
				continue
			}
			if ev.Type == event.WorkerOutput {
				writeLiveEvent(w, ev)
				flusher.Flush()
			}
			if isTerminalMissionEvent(ev.Type) {
				fmt.Fprintf(w, "event: stream:done\ndata: {}\n\n")
				flusher.Flush()
				return
			}
		}
	}
}

// writeLiveEvent projects a worker.output event into a liveOutputEvent and
// writes it as an SSE message.
func writeLiveEvent(w http.ResponseWriter, ev event.Event) {
	out := liveOutputEvent{
		MissionID: ev.MissionID,
		Phase:     ev.PhaseID,
		Persona:   ev.WorkerID,
	}
	if v, _ := ev.Data["tool_name"].(string); v != "" {
		out.ToolName = v
	}
	if v, _ := ev.Data["file_path"].(string); v != "" {
		out.FilePath = v
	}
	if v, _ := ev.Data["chunk"].(string); v != "" {
		out.Chunk = v
	}
	if v, _ := ev.Data["streaming"].(bool); v {
		out.Streaming = v
	}
	if v, _ := ev.Data["duration"].(string); v != "" {
		out.Duration = v
	}
	data, err := json.Marshal(out)
	if err != nil {
		return
	}
	fmt.Fprintf(w, "id: %d\nevent: worker.output\ndata: %s\n\n", ev.Sequence, data)
}

// handleGlobalLive streams worker.output events for all missions as SSE.
// Events are projected to the compact liveOutputEvent payload (includes mission_id).
// Supports optional ?mission_id= query param to filter to a specific mission.
// The stream stays open until the client disconnects — no terminal event closes it.
func (s *APIServer) handleGlobalLive(w http.ResponseWriter, r *http.Request) {
	missionFilter := r.URL.Query().Get("mission_id")

	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	w.Header().Set("Connection", "keep-alive")
	w.Header().Set("X-Accel-Buffering", "no")
	applyCORS(w, r)

	flusher, ok := w.(http.Flusher)
	if !ok {
		http.Error(w, "streaming not supported", http.StatusInternalServerError)
		return
	}

	subID, ch := s.bus.Subscribe()
	defer s.bus.Unsubscribe(subID)

	// Replay buffered worker.output events the client may have missed.
	for _, ev := range s.bus.EventsSince(0) {
		if ev.Type == event.WorkerOutput {
			if missionFilter == "" || ev.MissionID == missionFilter {
				writeLiveEvent(w, ev)
			}
		}
	}
	flusher.Flush()

	ticker := time.NewTicker(sseKeepaliveInterval)
	defer ticker.Stop()

	ctx := r.Context()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			fmt.Fprintf(w, ": keepalive\n\n")
			flusher.Flush()
		case ev, ok := <-ch:
			if !ok {
				return
			}
			if ev.Type != event.WorkerOutput {
				continue
			}
			if missionFilter != "" && ev.MissionID != missionFilter {
				continue
			}
			writeLiveEvent(w, ev)
			flusher.Flush()
		}
	}
}

// streamSSE is the shared SSE implementation used by handleSSE and handleMissionStream.
func (s *APIServer) streamSSE(w http.ResponseWriter, r *http.Request, filter sseFilter) {
	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	w.Header().Set("Connection", "keep-alive")
	w.Header().Set("X-Accel-Buffering", "no") // disable nginx buffering
	applyCORS(w, r)

	flusher, ok := w.(http.Flusher)
	if !ok {
		http.Error(w, "streaming not supported", http.StatusInternalServerError)
		return
	}

	// Subscribe BEFORE replaying so we don't miss events between replay and
	// live subscription.
	subID, ch := s.bus.Subscribe()
	defer s.bus.Unsubscribe(subID)

	// Parse Last-Event-ID for reconnect support.
	lastSeq := int64(0)
	if idStr := r.Header.Get("Last-Event-ID"); idStr != "" {
		if n, err := strconv.ParseInt(idStr, 10, 64); err == nil {
			lastSeq = n
		}
	}

	// Replay buffered events the client missed.
	replayed := lastSeq
	for _, ev := range s.bus.EventsSince(lastSeq) {
		if filter.match(ev) {
			writeSSEEvent(w, event.Sanitize(ev))
		}
		if ev.Sequence > replayed {
			replayed = ev.Sequence
		}
	}
	flusher.Flush()

	// keepalive ticker: prevents proxies/LBs from closing idle SSE connections.
	ticker := time.NewTicker(sseKeepaliveInterval)
	defer ticker.Stop()

	ctx := r.Context()
	for {
		select {
		case <-ctx.Done():
			return

		case <-ticker.C:
			// SSE comment — ignored as data by clients, keeps the connection alive.
			fmt.Fprintf(w, ": keepalive\n\n")
			flusher.Flush()

		case ev, ok := <-ch:
			if !ok {
				return
			}
			if ev.Sequence <= replayed {
				continue // already sent during replay
			}
			if !filter.match(ev) {
				continue
			}
			writeSSEEvent(w, event.Sanitize(ev))
			flusher.Flush()
			// For per-mission streams, close after a terminal event so the
			// client and server don't hold the connection open indefinitely.
			if filter.missionID != "" && isTerminalMissionEvent(ev.Type) {
				fmt.Fprintf(w, "event: stream:done\ndata: {}\n\n")
				flusher.Flush()
				return
			}
		}
	}
}

// isTerminalMissionEvent reports whether ev is a mission lifecycle event that
// signals no further events will be emitted for that mission.
func isTerminalMissionEvent(t event.EventType) bool {
	return t == event.MissionCompleted || t == event.MissionFailed || t == event.MissionCancelled
}

// writeSSEEvent writes one Server-Sent Event in the canonical wire format:
//
//	id: <sequence>
//	event: <type>
//	data: <json>
//	(blank line)
func writeSSEEvent(w http.ResponseWriter, ev event.Event) {
	data, err := json.Marshal(ev)
	if err != nil {
		return // drop unserializable events
	}
	fmt.Fprintf(w, "id: %d\nevent: %s\ndata: %s\n\n", ev.Sequence, ev.Type, data)
}

// handleListMissions returns metadata about all available mission event logs.
func (s *APIServer) handleListMissions(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)

	logsDir, err := event.EventLogsDir()
	if err != nil {
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}

	missions, err := listMissionLogs(logsDir)
	if err != nil {
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}

	// Overlay live status for missions the event projection has seen.
	// Same rule as mission detail: only replace when checkpoint says
	// in_progress (or is absent), so manual overrides remain visible.
	for i := range missions {
		if snap := s.liveState.Mission(missions[i].MissionID); snap != nil &&
			(missions[i].Status == "" || missions[i].Status == "in_progress") {
			missions[i].Status = snap.Status
		}
	}

	writeJSON(w, missions)
}

// MissionDetail is the REST representation of a single mission.
type MissionDetail struct {
	MissionID  string    `json:"mission_id"`
	Status     string    `json:"status,omitempty"`
	Task       string    `json:"task,omitempty"`
	Phases     int       `json:"phases,omitempty"`
	EventCount int       `json:"event_count"`
	SizeBytes  int64     `json:"size_bytes"`
	ModifiedAt time.Time `json:"modified_at"`
}

// handleMission returns detail for a single mission by ID.
// Event-log metadata is combined with plan info from the checkpoint when present.
func (s *APIServer) handleMission(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)

	id, ok := parseMissionID(w, r)
	if !ok {
		return
	}

	logPath, err := event.EventLogPath(id)
	if err != nil {
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}

	info, err := os.Stat(logPath)
	if err != nil {
		http.Error(w, "not found", http.StatusNotFound)
		return
	}

	detail := MissionDetail{
		MissionID:  id,
		EventCount: countMissionEvents(logPath),
		SizeBytes:  info.Size(),
		ModifiedAt: info.ModTime(),
	}

	// Enrich with plan data from checkpoint when available.
	if cp := loadCheckpoint(id); cp != nil {
		detail.Status = cp.Status
		if cp.Plan != nil {
			detail.Task = cp.Plan.Task
			detail.Phases = len(cp.Plan.Phases)
		}
	}

	// Overlay live status from the event projection when available — more
	// current than checkpoint for actively-running missions.
	// Skip the overlay when the checkpoint already carries a manually-set
	// terminal status (completed/failed/stalled/cancelled): that override
	// must win because the live projection has no knowledge of PATCH writes.
	if snap := s.liveState.Mission(id); snap != nil &&
		(detail.Status == "" || detail.Status == "in_progress") {
		detail.Status = snap.Status
	}

	writeJSON(w, detail)
}

// updateStatusRequest is the request body for PATCH /api/missions/{id}/status.
type updateStatusRequest struct {
	Status string `json:"status"` // "completed", "failed", "stalled"
}

// handleUpdateMissionStatus force-updates a mission's checkpoint status.
// This is used to manually resolve stalled or stuck missions.
func (s *APIServer) handleUpdateMissionStatus(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)

	id, ok := parseMissionID(w, r)
	if !ok {
		return
	}

	var req updateStatusRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		http.Error(w, fmt.Sprintf("invalid request body: %v", err), http.StatusBadRequest)
		return
	}

	allowed := map[string]bool{"completed": true, "failed": true, "stalled": true}
	if !allowed[req.Status] {
		http.Error(w, fmt.Sprintf("invalid status %q: must be completed, failed, or stalled", req.Status), http.StatusBadRequest)
		return
	}

	base, err := config.Dir()
	if err != nil {
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}

	wsPath := filepath.Join(base, "workspaces", id)
	cp, err := core.LoadCheckpoint(wsPath)
	if err != nil {
		http.Error(w, "mission not found", http.StatusNotFound)
		return
	}

	oldStatus := cp.Status
	cp.Status = req.Status
	if err := core.SaveCheckpoint(wsPath, cp.Plan, cp.Domain, cp.Status, cp.StartedAt); err != nil {
		http.Error(w, fmt.Sprintf("failed to save checkpoint: %v", err), http.StatusInternalServerError)
		return
	}

	writeJSON(w, map[string]string{
		"mission_id": id,
		"old_status": oldStatus,
		"new_status": req.Status,
	})
}

// handleCancelMission cancels a running mission by writing a cancel sentinel
// file to its workspace and signalling the orchestrator process via its PID file.
// If the process is already dead (stale PID or no PID file), performs manual
// cleanup: marks pending phases as skipped, emits mission.cancelled, and saves
// the checkpoint as cancelled.
func (s *APIServer) handleCancelMission(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)

	id, ok := parseMissionID(w, r)
	if !ok {
		return
	}

	base, err := config.Dir()
	if err != nil {
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}

	wsPath := filepath.Join(base, "workspaces", id)
	cp, err := core.LoadCheckpoint(wsPath)
	if err != nil {
		http.Error(w, "mission not found", http.StatusNotFound)
		return
	}

	// Already terminal — 409 Conflict with current status.
	switch cp.Status {
	case "completed", "failed", "cancelled":
		w.WriteHeader(http.StatusConflict)
		writeJSON(w, map[string]string{
			"mission_id": id,
			"status":     cp.Status,
			"message":    fmt.Sprintf("mission already %s", cp.Status),
		})
		return
	}

	// Write cancel sentinel first so the engine picks it up between phases
	// regardless of whether the signal succeeds (belt-and-suspenders).
	if err := core.WriteCancelSentinel(wsPath); err != nil {
		http.Error(w, fmt.Sprintf("writing cancel sentinel: %v", err), http.StatusInternalServerError)
		return
	}

	// Try to signal the running process via PID file.
	pid, err := core.ReadPID(wsPath)
	if err != nil {
		http.Error(w, fmt.Sprintf("reading pid file: %v", err), http.StatusInternalServerError)
		return
	}

	if pid > 0 {
		if proc, findErr := os.FindProcess(pid); findErr == nil {
			// Signal 0 probes liveness without side effects.
			if proc.Signal(syscall.Signal(0)) == nil {
				// Process is alive; send SIGTERM for graceful shutdown.
				// The engine's teardown logic will handle checkpoint and event emission.
				_ = proc.Signal(syscall.SIGTERM)
				writeJSON(w, map[string]any{
					"mission_id": id,
					"action":     "signalled",
					"pid":        pid,
					"signal":     "SIGTERM",
				})
				return
			}
		}
	}

	// Process is dead or no PID file — manual cleanup path.
	if err := cancelMissionManually(wsPath, id, cp); err != nil {
		http.Error(w, fmt.Sprintf("manual cancel: %v", err), http.StatusInternalServerError)
		return
	}

	writeJSON(w, map[string]any{
		"mission_id": id,
		"action":     "cancelled",
	})
}

// cancelMissionManually performs cleanup when the orchestrator process is no
// longer running. Mirrors manualCancel in cmd/cancel.go: marks pending/running
// phases as skipped, emits coherent events, and saves the checkpoint as cancelled.
func cancelMissionManually(wsPath, missionID string, cp *core.Checkpoint) error {
	plan := cp.Plan
	if plan == nil {
		return fmt.Errorf("checkpoint has no plan")
	}

	logPath, err := event.EventLogPath(missionID)
	if err != nil {
		return fmt.Errorf("resolving event log path: %w", err)
	}

	var emitter event.Emitter
	if fe, emitErr := event.NewFileEmitter(logPath); emitErr == nil {
		emitter = fe
	} else {
		emitter = event.NoOpEmitter{}
	}
	defer emitter.Close() //nolint:errcheck

	ctx := context.Background()
	now := time.Now()
	skipped := 0

	for _, phase := range plan.Phases {
		if phase.Status.IsTerminal() {
			continue
		}
		phase.Status = core.StatusSkipped
		phase.Error = "skipped: mission cancelled"
		phase.EndTime = &now
		skipped++
		emitter.Emit(ctx, event.New(event.PhaseSkipped, missionID, phase.ID, "", map[string]any{
			"reason": "mission cancelled (manual cleanup)",
		}))
	}

	emitter.Emit(ctx, event.New(event.MissionCancelled, missionID, "", "", map[string]any{
		"source":         "orchestrator cancel",
		"skipped_phases": skipped,
	}))

	return core.SaveCheckpoint(wsPath, plan, cp.Domain, "cancelled", cp.StartedAt)
}

// DAGResponse is the response body for GET /api/missions/{id}/dag.
type DAGResponse struct {
	MissionID string    `json:"mission_id"`
	Nodes     []DAGNode `json:"nodes"`
	Edges     []DAGEdge `json:"edges"`
}

// DAGNode is a phase vertex in the execution DAG.
type DAGNode struct {
	ID           string   `json:"id"`
	Name         string   `json:"name"`
	Persona      string   `json:"persona"`
	Skills       []string `json:"skills"`
	Status       string   `json:"status"`
	Dependencies []string `json:"dependencies"`
}

// DAGEdge is a directed dependency edge: From must complete before To can start.
type DAGEdge struct {
	From string `json:"from"` // dependency phase ID
	To   string `json:"to"`   // dependent phase ID
}

// handleMissionDAG returns the execution DAG for a mission.
// Requires a checkpoint.json in the mission workspace.
func (s *APIServer) handleMissionDAG(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)

	id, ok := parseMissionID(w, r)
	if !ok {
		return
	}

	cp := loadCheckpoint(id)
	if cp == nil || cp.Plan == nil {
		http.Error(w, "not found", http.StatusNotFound)
		return
	}

	dag := buildDAG(id, cp.Plan)

	// Overlay live phase statuses from the event projection when available.
	if snap := s.liveState.Mission(id); snap != nil {
		for i := range dag.Nodes {
			if ps, ok := snap.Phases[dag.Nodes[i].ID]; ok {
				dag.Nodes[i].Status = ps.Status
			}
		}
	}

	writeJSON(w, dag)
}

// buildDAG constructs a DAGResponse from a plan.
// Each phase becomes a node; each dependency relationship becomes a directed edge.
func buildDAG(missionID string, plan *core.Plan) DAGResponse {
	nodes := make([]DAGNode, 0, len(plan.Phases))
	var edges []DAGEdge

	for _, p := range plan.Phases {
		deps := p.Dependencies
		if deps == nil {
			deps = []string{}
		}
		skills := p.Skills
		if skills == nil {
			skills = []string{}
		}
		nodes = append(nodes, DAGNode{
			ID:           p.ID,
			Name:         p.Name,
			Persona:      p.Persona,
			Skills:       skills,
			Status:       string(p.Status),
			Dependencies: deps,
		})
		// Emit one edge per dependency: dep → this phase.
		for _, depID := range p.Dependencies {
			edges = append(edges, DAGEdge{From: depID, To: p.ID})
		}
	}
	if edges == nil {
		edges = []DAGEdge{}
	}

	return DAGResponse{
		MissionID: missionID,
		Nodes:     nodes,
		Edges:     edges,
	}
}

// handleReplayMission returns sanitized events from a mission's JSONL log.
func (s *APIServer) handleReplayMission(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)

	id, ok := parseMissionID(w, r)
	if !ok {
		return
	}

	logPath, err := event.EventLogPath(id)
	if err != nil {
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}

	events, err := readMissionEvents(logPath)
	if err != nil {
		http.Error(w, "not found", http.StatusNotFound)
		return
	}

	sanitized := make([]event.Event, len(events))
	for i, ev := range events {
		sanitized[i] = event.Sanitize(ev)
	}
	writeJSON(w, sanitized)
}

// handleIngestEvents accepts events via HTTP POST and publishes them to the bus.
// This is the HTTP alternative to the UDS socket ingestion path, useful for
// remote orchestrators or tooling that can't connect to the Unix domain socket.
//
// Accepts two content formats:
//   - application/json: a single Event object.
//   - application/x-ndjson (or text/plain): newline-delimited JSON, one Event per line.
//
// Returns 202 Accepted with a count of published events.
func (s *APIServer) handleIngestEvents(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)

	ct := r.Header.Get("Content-Type")

	// Single JSON event.
	if strings.HasPrefix(ct, "application/json") {
		var ev event.Event
		if err := json.NewDecoder(r.Body).Decode(&ev); err != nil {
			http.Error(w, fmt.Sprintf("invalid event JSON: %v", err), http.StatusBadRequest)
			return
		}
		if ev.Type == "" {
			http.Error(w, "event type is required", http.StatusBadRequest)
			return
		}
		s.bus.Publish(ev)
		w.WriteHeader(http.StatusAccepted)
		writeJSON(w, map[string]int{"published": 1})
		return
	}

	// NDJSON (default for non-JSON content types).
	sc := bufio.NewScanner(r.Body)
	sc.Buffer(make([]byte, 64*1024), 64*1024)
	published := 0
	for sc.Scan() {
		line := sc.Text()
		if line == "" {
			continue
		}
		var ev event.Event
		if err := json.Unmarshal([]byte(line), &ev); err != nil {
			continue // skip malformed lines, same as UDS handler
		}
		if ev.Type == "" {
			continue
		}
		s.bus.Publish(ev)
		published++
	}
	if err := sc.Err(); err != nil {
		http.Error(w, fmt.Sprintf("reading request body: %v", err), http.StatusBadRequest)
		return
	}
	w.WriteHeader(http.StatusAccepted)
	writeJSON(w, map[string]int{"published": published})
}

// addChannel registers a notification channel and returns its tracker so the
// daemon can wire up the health hook before starting the notifier goroutine.
func (s *APIServer) addChannel(status ChannelStatus) *channelTracker {
	t := &channelTracker{status: status}
	s.channelMu.Lock()
	s.channels = append(s.channels, t)
	s.channelMu.Unlock()
	return t
}

// handleChannels returns the health status of all registered notification channels.
func (s *APIServer) handleChannels(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)
	s.channelMu.RLock()
	trackers := make([]*channelTracker, len(s.channels))
	copy(trackers, s.channels)
	s.channelMu.RUnlock()

	out := make([]ChannelStatus, len(trackers))
	for i, t := range trackers {
		out[i] = t.snapshot()
	}
	writeJSON(w, out)
}

func (s *APIServer) handleHealth(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)
	var stats event.DropStats
	if s.dropper != nil {
		stats = s.dropper.DropStats()
	}
	status := "ok"
	if stats.Any() {
		status = "degraded"
	}
	writeJSON(w, map[string]any{
		"status":               status,
		"subscriber_drops":     stats.SubscriberDrops,
		"file_dropped_writes":  stats.FileDroppedWrites,
		"uds_dropped_writes":   stats.UDSDroppedWrites,
	})
}

func writeJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json")
	json.NewEncoder(w).Encode(v) //nolint:errcheck
}

// parseMissionID extracts and validates the {id} path value.
// Writes a 400 response and returns false if the id is empty or contains
// path traversal sequences.
func parseMissionID(w http.ResponseWriter, r *http.Request) (string, bool) {
	id := r.PathValue("id")
	if id == "" {
		http.Error(w, "mission id required", http.StatusBadRequest)
		return "", false
	}
	if strings.Contains(id, "/") || strings.Contains(id, "..") {
		http.Error(w, "invalid mission id", http.StatusBadRequest)
		return "", false
	}
	return id, true
}

// loadCheckpoint attempts to load the checkpoint for a mission workspace.
// Returns nil if the checkpoint doesn't exist or cannot be parsed.
func loadCheckpoint(missionID string) *core.Checkpoint {
	base, err := config.Dir()
	if err != nil {
		return nil
	}
	wsPath := filepath.Join(base, "workspaces", missionID)
	cp, err := core.LoadCheckpoint(wsPath)
	if err != nil {
		return nil
	}
	return cp
}

// ---- mission log helpers -----------------------------------------------

// MissionLogInfo is the REST representation of a mission event log.
type MissionLogInfo struct {
	MissionID  string    `json:"mission_id"`
	Status     string    `json:"status,omitempty"`
	Task       string    `json:"task,omitempty"`
	EventCount int       `json:"event_count"`
	SizeBytes  int64     `json:"size_bytes"`
	ModifiedAt time.Time `json:"modified_at"`
}

func listMissionLogs(logsDir string) ([]MissionLogInfo, error) {
	entries, err := os.ReadDir(logsDir)
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("reading logs dir: %w", err)
	}

	var result []MissionLogInfo
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".jsonl") {
			continue
		}
		info, err := e.Info()
		if err != nil {
			continue
		}
		mid := strings.TrimSuffix(e.Name(), ".jsonl")
		path := filepath.Join(logsDir, e.Name())
		entry := MissionLogInfo{
			MissionID:  mid,
			EventCount: countMissionEvents(path),
			SizeBytes:  info.Size(),
			ModifiedAt: info.ModTime(),
		}
		if cp := loadCheckpoint(mid); cp != nil {
			entry.Status = cp.Status
			if cp.Plan != nil {
				entry.Task = cp.Plan.Task
			}
		}
		result = append(result, entry)
	}
	return result, nil
}

func countMissionEvents(path string) int {
	f, err := os.Open(path)
	if err != nil {
		return 0
	}
	defer f.Close()
	n := 0
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		if sc.Text() != "" {
			n++
		}
	}
	return n
}

func readMissionEvents(logPath string) ([]event.Event, error) {
	f, err := os.Open(logPath)
	if err != nil {
		return nil, fmt.Errorf("opening event log: %w", err)
	}
	defer f.Close()

	var events []event.Event
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 64*1024), 64*1024)
	for sc.Scan() {
		line := sc.Text()
		if line == "" {
			continue
		}
		var ev event.Event
		if err := json.Unmarshal([]byte(line), &ev); err != nil {
			continue // skip malformed lines
		}
		events = append(events, ev)
	}
	if err := sc.Err(); err != nil {
		return nil, fmt.Errorf("scanning events: %w", err)
	}
	return events, nil
}

// ---- persona handlers and helpers -----------------------------------------------

// PersonaResponse is the REST representation of a persona with aggregated usage stats.
type PersonaResponse struct {
	Name               string   `json:"name"`
	WhenToUse          []string `json:"when_to_use"`
	Expertise          []string `json:"expertise"`
	Color              string   `json:"color"`
	MissionsAssigned   int      `json:"missions_assigned"`
	SuccessRate        float64  `json:"success_rate"`
	AvgDurationSeconds float64  `json:"avg_duration_seconds"`
	CurrentlyActive    bool     `json:"currently_active"`
}

// PersonaRecentMission is one mission entry in the persona detail view.
type PersonaRecentMission struct {
	WorkspaceID string    `json:"workspace_id"`
	Domain      string    `json:"domain"`
	Task        string    `json:"task"`
	Status      string    `json:"status"`
	StartedAt   time.Time `json:"started_at"`
	DurationSec int       `json:"duration_s"`
}

// PersonaSuccessTrendPoint is one week in the persona success rate trend.
type PersonaSuccessTrendPoint struct {
	Week        string  `json:"week"` // "2026-W12"
	Total       int     `json:"total"`
	Succeeded   int     `json:"succeeded"`
	SuccessRate float64 `json:"success_rate"` // 0–1
}

// PersonaDetailResponse is the response body for GET /api/personas/{name}.
type PersonaDetailResponse struct {
	PersonaResponse
	RecentMissions []PersonaRecentMission     `json:"recent_missions"`
	SuccessTrend   []PersonaSuccessTrendPoint `json:"success_trend"`
}

// handleListPersonas returns all personas with aggregated usage stats from event logs.
func (s *APIServer) handleListPersonas(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)

	personas := persona.All()

	logsDir, err := event.EventLogsDir()
	if err != nil {
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}

	stats, err := aggregatePersonaStats(logsDir)
	if err != nil {
		// Non-fatal: return personas with zero stats if events dir is unreadable.
		stats = make(map[string]*personaUsage)
	}

	// Prefer metrics.db for stats when available; fall back to event-log aggregation.
	dbStats := s.queryPersonaStatsFromDB(r.Context())

	result := make([]PersonaResponse, 0, len(personas))
	for name, p := range personas {
		expertise := p.Expertise
		if len(expertise) == 0 {
			expertise = []string{}
		}
		whenToUse := p.WhenToUse
		if len(whenToUse) == 0 {
			whenToUse = []string{}
		}
		resp := PersonaResponse{
			Name:      name,
			WhenToUse: whenToUse,
			Expertise: expertise,
			Color:     personaColor(name),
		}

		if ds, ok := dbStats[name]; ok {
			resp.MissionsAssigned = ds.missionsTotal
			resp.SuccessRate = ds.successRate
			resp.AvgDurationSeconds = ds.avgDurationSec
		} else if u := stats[name]; u != nil {
			// Event-log fallback.
			resp.MissionsAssigned = u.missionsAssigned
			if u.total > 0 {
				resp.SuccessRate = float64(u.succeeded) / float64(u.total)
			}
			if len(u.durations) > 0 {
				var sum float64
				for _, d := range u.durations {
					sum += d.Seconds()
				}
				resp.AvgDurationSeconds = sum / float64(len(u.durations))
			}
		}

		// Active status still comes from live event logs (metrics.db only has completed missions).
		if u := stats[name]; u != nil {
			resp.CurrentlyActive = u.active
		}

		result = append(result, resp)
	}

	sort.Slice(result, func(i, j int) bool {
		return result[i].Name < result[j].Name
	})

	writeJSON(w, result)
}

// handleReloadPersonas reloads the persona catalog from disk immediately.
func (s *APIServer) handleReloadPersonas(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)
	if err := persona.Reload(); err != nil {
		http.Error(w, fmt.Sprintf("reload failed: %v", err), http.StatusInternalServerError)
		return
	}
	writeJSON(w, map[string]any{
		"reloaded": true,
		"count":    len(persona.All()),
	})
}

// personaDBStats holds per-persona aggregated stats from metrics.db.
type personaDBStats struct {
	missionsTotal  int
	successRate    float64
	avgDurationSec float64
}

// queryPersonaStatsFromDB queries metrics.db for per-persona mission stats.
// Returns nil map when metricsDB is unavailable so callers can fall back.
func (s *APIServer) queryPersonaStatsFromDB(ctx context.Context) map[string]personaDBStats {
	if s.metricsDB == nil {
		return nil
	}
	rows, err := s.metricsDB.RawDB().QueryContext(ctx, `
		SELECT
			p.persona,
			COUNT(DISTINCT p.mission_id)                                     AS missions_total,
			SUM(CASE WHEN m.status = 'success' THEN 1 ELSE 0 END) * 1.0
				/ NULLIF(COUNT(DISTINCT p.mission_id), 0)                    AS success_rate,
			AVG(CAST(m.duration_s AS REAL))                                  AS avg_duration_s
		FROM phases p
		JOIN missions m ON m.id = p.mission_id
		WHERE p.persona != ''
		GROUP BY p.persona
	`)
	if err != nil {
		return nil
	}
	defer rows.Close()

	out := make(map[string]personaDBStats)
	for rows.Next() {
		var name string
		var ds personaDBStats
		if err := rows.Scan(&name, &ds.missionsTotal, &ds.successRate, &ds.avgDurationSec); err != nil {
			continue
		}
		out[name] = ds
	}
	return out
}

// handleGetPersona returns detail for a single persona: stats, recent missions, and weekly trend.
func (s *APIServer) handleGetPersona(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)

	name := r.PathValue("name")
	if name == "" {
		http.Error(w, "name required", http.StatusBadRequest)
		return
	}

	personas := persona.All()
	p, ok := personas[name]
	if !ok {
		http.Error(w, "persona not found", http.StatusNotFound)
		return
	}

	expertise := p.Expertise
	if len(expertise) == 0 {
		expertise = []string{}
	}
	whenToUse := p.WhenToUse
	if len(whenToUse) == 0 {
		whenToUse = []string{}
	}

	base := PersonaResponse{
		Name:      name,
		WhenToUse: whenToUse,
		Expertise: expertise,
		Color:     personaColor(name),
	}

	detail := PersonaDetailResponse{
		PersonaResponse: base,
		RecentMissions:  []PersonaRecentMission{},
		SuccessTrend:    []PersonaSuccessTrendPoint{},
	}

	// Populate stats and detail from metrics.db when available.
	if s.metricsDB != nil {
		if stats, ok := s.queryPersonaStatsFromDB(r.Context())[name]; ok {
			detail.MissionsAssigned = stats.missionsTotal
			detail.SuccessRate = stats.successRate
			detail.AvgDurationSeconds = stats.avgDurationSec
		}
		detail.RecentMissions = s.queryPersonaRecentMissions(r.Context(), name, 20)
		detail.SuccessTrend = s.queryPersonaSuccessTrend(r.Context(), name, 8)
	}

	// Active status from live event logs.
	if logsDir, err := event.EventLogsDir(); err == nil {
		if eventStats, err := aggregatePersonaStats(logsDir); err == nil {
			if u, ok := eventStats[name]; ok {
				detail.CurrentlyActive = u.active
				// Fall back to event-log stats when metrics.db had nothing.
				if detail.MissionsAssigned == 0 {
					detail.MissionsAssigned = u.missionsAssigned
					if u.total > 0 {
						detail.SuccessRate = float64(u.succeeded) / float64(u.total)
					}
					if len(u.durations) > 0 {
						var sum float64
						for _, d := range u.durations {
							sum += d.Seconds()
						}
						detail.AvgDurationSeconds = sum / float64(len(u.durations))
					}
				}
			}
		}
	}

	writeJSON(w, detail)
}

// queryPersonaRecentMissions returns the N most recent missions that included this persona.
func (s *APIServer) queryPersonaRecentMissions(ctx context.Context, personaName string, limit int) []PersonaRecentMission {
	rows, err := s.metricsDB.RawDB().QueryContext(ctx, `
		SELECT DISTINCT m.id, m.domain, m.task, m.status, m.started_at, m.duration_s
		FROM missions m
		JOIN phases p ON p.mission_id = m.id
		WHERE p.persona = ?
		ORDER BY m.started_at DESC
		LIMIT ?
	`, personaName, limit)
	if err != nil {
		return []PersonaRecentMission{}
	}
	defer rows.Close()

	var out []PersonaRecentMission
	for rows.Next() {
		var rm PersonaRecentMission
		var startedAt string
		if err := rows.Scan(&rm.WorkspaceID, &rm.Domain, &rm.Task, &rm.Status, &startedAt, &rm.DurationSec); err != nil {
			continue
		}
		if t, err := time.Parse(time.RFC3339, startedAt); err == nil {
			rm.StartedAt = t
		}
		out = append(out, rm)
	}
	if out == nil {
		return []PersonaRecentMission{}
	}
	return out
}

// queryPersonaSuccessTrend returns weekly success rate for a persona over the last N weeks.
func (s *APIServer) queryPersonaSuccessTrend(ctx context.Context, personaName string, weeks int) []PersonaSuccessTrendPoint {
	rows, err := s.metricsDB.RawDB().QueryContext(ctx, `
		SELECT
			strftime('%Y-W%W', m.started_at) AS week,
			COUNT(DISTINCT m.id)             AS total,
			SUM(CASE WHEN m.status = 'success' THEN 1 ELSE 0 END) AS succeeded
		FROM missions m
		JOIN phases p ON p.mission_id = m.id
		WHERE p.persona = ?
		  AND m.started_at >= datetime('now', '-' || ? || ' days')
		GROUP BY week
		ORDER BY week ASC
	`, personaName, weeks*7)
	if err != nil {
		return []PersonaSuccessTrendPoint{}
	}
	defer rows.Close()

	var out []PersonaSuccessTrendPoint
	for rows.Next() {
		var pt PersonaSuccessTrendPoint
		if err := rows.Scan(&pt.Week, &pt.Total, &pt.Succeeded); err != nil {
			continue
		}
		if pt.Total > 0 {
			pt.SuccessRate = float64(pt.Succeeded) / float64(pt.Total)
		}
		out = append(out, pt)
	}
	if out == nil {
		return []PersonaSuccessTrendPoint{}
	}
	return out
}

// personaUsage accumulates stats for one persona across all missions.
type personaUsage struct {
	missionsAssigned int
	succeeded        int
	total            int           // missions with a terminal event (completed or failed)
	durations        []time.Duration
	active           bool
}

// aggregatePersonaStats reads all JSONL files in logsDir and returns per-persona stats.
func aggregatePersonaStats(logsDir string) (map[string]*personaUsage, error) {
	entries, err := os.ReadDir(logsDir)
	if os.IsNotExist(err) {
		return make(map[string]*personaUsage), nil
	}
	if err != nil {
		return nil, fmt.Errorf("reading events dir: %w", err)
	}

	stats := make(map[string]*personaUsage)
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".jsonl") {
			continue
		}
		path := filepath.Join(logsDir, e.Name())
		if err := processMissionLog(path, stats); err != nil {
			continue // skip unreadable or malformed logs
		}
	}
	return stats, nil
}

// processMissionLog reads one mission JSONL and updates stats for each persona that appeared.
func processMissionLog(logPath string, stats map[string]*personaUsage) error {
	events, err := readMissionEvents(logPath)
	if err != nil {
		return err
	}

	personasInMission := make(map[string]bool)
	var completed, failed bool
	var duration time.Duration

	for _, ev := range events {
		switch ev.Type {
		case event.PhaseStarted, event.DAGPhaseDispatched:
			if p, ok := ev.Data["persona"].(string); ok && p != "" {
				personasInMission[p] = true
			}
		case event.MissionCompleted:
			completed = true
			if d, ok := ev.Data["duration"].(string); ok {
				if dur, err := time.ParseDuration(d); err == nil {
					duration = dur
				}
			}
		case event.MissionFailed:
			failed = true
		}
	}

	if len(personasInMission) == 0 {
		return nil
	}

	isActive := !completed && !failed

	for p := range personasInMission {
		u := stats[p]
		if u == nil {
			u = &personaUsage{}
			stats[p] = u
		}
		u.missionsAssigned++
		if completed || failed {
			u.total++
		}
		if completed {
			u.succeeded++
			if duration > 0 {
				u.durations = append(u.durations, duration)
			}
		}
		if isActive {
			u.active = true
		}
	}

	return nil
}

// personaColor returns a stable display color for a named persona.
// Unknown personas fall back to gray.
func personaColor(name string) string {
	colors := map[string]string{
		// Live 10-persona catalog.
		"academic-researcher":      "#0ea5e9", // sky
		"architect":                "#6366f1", // indigo
		"data-analyst":             "#3b82f6", // blue
		"devops-engineer":          "#8b5cf6", // violet
		"qa-engineer":              "#ef4444", // red
		"security-auditor":         "#dc2626", // red-600
		"senior-backend-engineer":  "#10b981", // emerald
		"senior-frontend-engineer": "#06b6d4", // cyan
		"staff-code-reviewer":      "#f59e0b", // amber
		"technical-writer":         "#64748b", // slate

		// Legacy names (historical event log data)
		"academic-reviewer":         "#0284c7",
		"academic-writer":           "#2563eb",
		"artist":                    "#ec4899",
		"backend-engineer":          "#10b981",
		"cartographer":              "#14b8a6",
		"cinematographer":           "#7c3aed",
		"code-reviewer":             "#f59e0b",
		"conversion-writer":         "#c026d3",
		"frontend-engineer":         "#06b6d4",
		"indie-coach":               "#f97316",
		"journaler":                 "#84cc16",
		"methodologist":             "#a855f7",
		"narrator":                  "#059669",
		"principal-systems-reviewer": "#6366f1",
		"researcher":                "#0ea5e9",
		"senior-golang-engineer":    "#34d399",
		"senior-rust-engineer":      "#ea580c",
		"storyteller":               "#d946ef",
	}
	if c, ok := colors[name]; ok {
		return c
	}
	return "#6b7280" // gray
}

// ---- metrics endpoint ---------------------------------------------------

// MetricsResponse is the response body for GET /api/metrics.
type MetricsResponse struct {
	Total        int                      `json:"total"`
	Completed    int                      `json:"completed"`
	Failed       int                      `json:"failed"`
	Cancelled    int                      `json:"cancelled"`
	AvgDurationS int                      `json:"avg_duration_s"`
	ByDomain     map[string]*DomainStats  `json:"by_domain"`
	ByPersona    map[string]*PersonaStats `json:"by_persona"`
	Recent       []RecentMission          `json:"recent"`
}

// DomainStats holds per-domain mission counts.
type DomainStats struct {
	Total     int `json:"total"`
	Completed int `json:"completed"`
	Failed    int `json:"failed"`
	Cancelled int `json:"cancelled"`
}

// PersonaStats holds per-persona phase execution counts across all missions.
type PersonaStats struct {
	Phases    int `json:"phases"`
	Completed int `json:"completed"`
	Failed    int `json:"failed"`
}

// RecentMission is one row in the recent missions list.
type RecentMission struct {
	WorkspaceID string    `json:"workspace_id"`
	Domain      string    `json:"domain"`
	Task        string    `json:"task,omitempty"`
	Status      string    `json:"status"`
	StartedAt   time.Time `json:"started_at,omitempty"`
	DurationS   int       `json:"duration_s,omitempty"`
}

// metricsRecord matches the JSONL shape written by engine.RecordMetrics.
type metricsRecord struct {
	WorkspaceID string    `json:"workspace_id"`
	Domain      string    `json:"domain"`
	Task        string    `json:"task"`
	StartedAt   time.Time `json:"started_at"`
	DurationSec int       `json:"duration_s"`
	Status      string    `json:"status"` // "success", "failure", "partial"
	Phases      []struct {
		Persona string `json:"persona"`
		Status  string `json:"status"`
	} `json:"phases,omitempty"`
}

// cancelledWorkspace holds minimal info about a cancelled workspace.
type cancelledWorkspace struct {
	WorkspaceID string
	Domain      string
}

// handleMetrics aggregates mission statistics from ~/.alluka/metrics.jsonl and
// workspace checkpoint/event-log files, returning totals, breakdowns, and a
// recent missions list.
//
// Optional query param:
//
//	?last=N — number of recent missions to include (default 10)
func (s *APIServer) handleMetrics(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)

	last := 10
	if n := r.URL.Query().Get("last"); n != "" {
		if v, err := strconv.Atoi(n); err == nil && v > 0 {
			last = v
		}
	}

	result, err := gatherMetrics(last)
	if err != nil {
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}
	writeJSON(w, result)
}

// gatherMetrics resolves paths from the real home directory and delegates to
// aggregateMetrics. Returns a zero-value response when no data exists yet.
func gatherMetrics(recentLimit int) (*MetricsResponse, error) {
	base, err := config.Dir()
	if err != nil {
		return nil, fmt.Errorf("getting config dir: %w", err)
	}
	return aggregateMetrics(
		filepath.Join(base, "metrics.jsonl"),
		filepath.Join(base, "workspaces"),
		filepath.Join(base, "events"),
		recentLimit,
	)
}

// aggregateMetrics is the testable aggregation core that takes explicit paths.
//
// Data sources:
//   - metricsPath (~/.alluka/metrics.jsonl): one JSONL record per completed/failed
//     mission written by engine.RecordMetrics. Authoritative for timing and
//     per-phase persona data.
//   - wsBase (~/.alluka/workspaces/): workspace directories with checkpoint.json.
//     Consulted for workspaces absent from metrics.jsonl — those whose event
//     log contains mission.cancelled are counted as cancelled.
//   - eventsBase (~/.alluka/events/): per-mission event JSONL logs used to detect
//     mission.cancelled events.
func aggregateMetrics(metricsPath, wsBase, eventsBase string, recentLimit int) (*MetricsResponse, error) {
	records, seenIDs, err := readMetricsRecords(metricsPath)
	if err != nil && !os.IsNotExist(err) {
		return nil, fmt.Errorf("reading metrics.jsonl: %w", err)
	}

	cancelled := scanCancelledWorkspaces(wsBase, eventsBase, seenIDs)

	resp := &MetricsResponse{
		ByDomain:  make(map[string]*DomainStats),
		ByPersona: make(map[string]*PersonaStats),
		Recent:    []RecentMission{},
	}

	var totalDurSec, durationCount int

	for _, rec := range records {
		resp.Total++

		apiStatus := engineToAPIStatus(rec.Status)
		if apiStatus == "completed" {
			resp.Completed++
		} else {
			resp.Failed++
		}

		if rec.DurationSec > 0 {
			totalDurSec += rec.DurationSec
			durationCount++
		}

		if rec.Domain != "" {
			ds := ensureDomainStats(resp.ByDomain, rec.Domain)
			ds.Total++
			if apiStatus == "completed" {
				ds.Completed++
			} else {
				ds.Failed++
			}
		}

		for _, p := range rec.Phases {
			if p.Persona == "" {
				continue
			}
			ps := ensurePersonaStats(resp.ByPersona, p.Persona)
			ps.Phases++
			switch p.Status {
			case "completed":
				ps.Completed++
			case "failed":
				ps.Failed++
			}
		}
	}

	for _, c := range cancelled {
		resp.Total++
		resp.Cancelled++
		if c.Domain != "" {
			ds := ensureDomainStats(resp.ByDomain, c.Domain)
			ds.Total++
			ds.Cancelled++
		}
	}

	if durationCount > 0 {
		resp.AvgDurationS = totalDurSec / durationCount
	}

	resp.Recent = buildRecentMissions(records, cancelled, recentLimit)

	return resp, nil
}

// engineToAPIStatus maps engine status strings to API status strings.
// "success" → "completed"; everything else ("failure", "partial") → "failed".
func engineToAPIStatus(s string) string {
	if s == "success" {
		return "completed"
	}
	return "failed"
}

// readMetricsRecords parses a metrics JSONL file and returns all records plus
// a set of workspace IDs already accounted for.
func readMetricsRecords(path string) ([]metricsRecord, map[string]bool, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, nil, err
	}
	defer f.Close()

	var records []metricsRecord
	seen := make(map[string]bool)

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 64*1024), 64*1024)
	for sc.Scan() {
		line := sc.Text()
		if line == "" {
			continue
		}
		var rec metricsRecord
		if err := json.Unmarshal([]byte(line), &rec); err != nil {
			continue // skip malformed lines
		}
		// Filter test/synthetic workspaces and zero-duration records.
		if strings.HasPrefix(rec.WorkspaceID, "ws-") || strings.HasPrefix(rec.WorkspaceID, "test-") || rec.DurationSec == 0 {
			seen[rec.WorkspaceID] = true // mark as seen so cancelled scan skips it too
			continue
		}
		records = append(records, rec)
		seen[rec.WorkspaceID] = true
	}
	return records, seen, sc.Err()
}

// scanCancelledWorkspaces walks wsBase and returns workspaces that have a
// mission.cancelled event in their event log but no entry in metrics.jsonl.
func scanCancelledWorkspaces(wsBase, eventsBase string, knownIDs map[string]bool) []cancelledWorkspace {
	entries, err := os.ReadDir(wsBase)
	if err != nil {
		return nil // workspace dir may not exist yet — non-fatal
	}

	var cancelled []cancelledWorkspace
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		wsID := e.Name()
		if knownIDs[wsID] {
			continue
		}
		// Skip test/synthetic workspace directories.
		if strings.HasPrefix(wsID, "ws-") || strings.HasPrefix(wsID, "test-") {
			continue
		}

		// Only consider workspaces that have a checkpoint (skip temp/partial dirs).
		cp, err := core.LoadCheckpoint(filepath.Join(wsBase, wsID))
		if err != nil {
			continue
		}

		logPath := filepath.Join(eventsBase, wsID+".jsonl")
		if eventLogHasCancelled(logPath) {
			cancelled = append(cancelled, cancelledWorkspace{
				WorkspaceID: wsID,
				Domain:      cp.Domain,
			})
		}
	}
	return cancelled
}

// eventLogHasCancelled reports whether a mission event log contains a
// mission.cancelled event. Uses a fast string pre-check to avoid full JSON
// parsing on every line.
func eventLogHasCancelled(path string) bool {
	f, err := os.Open(path)
	if err != nil {
		return false
	}
	defer f.Close()

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 16*1024), 16*1024)
	needle := string(event.MissionCancelled)
	for sc.Scan() {
		line := sc.Text()
		if !strings.Contains(line, needle) {
			continue
		}
		var ev struct {
			Type event.EventType `json:"type"`
		}
		if json.Unmarshal([]byte(line), &ev) == nil && ev.Type == event.MissionCancelled {
			return true
		}
	}
	return false
}

// buildRecentMissions returns the most recent missions as a summary list.
// Metrics records are oldest-first in the file, so we walk newest-first.
// Cancelled entries (no timing data) are appended after up to the limit.
func buildRecentMissions(records []metricsRecord, cancelled []cancelledWorkspace, limit int) []RecentMission {
	recent := make([]RecentMission, 0, limit)

	for i := len(records) - 1; i >= 0 && len(recent) < limit; i-- {
		rec := records[i]
		recent = append(recent, RecentMission{
			WorkspaceID: rec.WorkspaceID,
			Domain:      rec.Domain,
			Task:        rec.Task,
			Status:      engineToAPIStatus(rec.Status),
			StartedAt:   rec.StartedAt,
			DurationS:   rec.DurationSec,
		})
	}

	for _, c := range cancelled {
		if len(recent) >= limit {
			break
		}
		recent = append(recent, RecentMission{
			WorkspaceID: c.WorkspaceID,
			Domain:      c.Domain,
			Status:      "cancelled",
		})
	}

	return recent
}

// ---- decomposition findings endpoint -------------------------------------------

// DecompositionFinding is one row from the decomposition_findings table.
type DecompositionFinding struct {
	ID           int64  `json:"id"`
	WorkspaceID  string `json:"workspace_id"`
	TargetID     string `json:"target_id"`
	FindingType  string `json:"finding_type"`
	PhaseName    string `json:"phase_name"`
	Detail       string `json:"detail"`
	DecompSource string `json:"decomp_source"`
	AuditScore   int    `json:"audit_score"`
	CreatedAt    string `json:"created_at"`
}

// FindingCount is one row of the by-type breakdown.
type FindingCount struct {
	FindingType string `json:"finding_type"`
	Count       int    `json:"count"`
}

// FindingTrend is one row of a time-series aggregation.
type FindingTrend struct {
	Period string `json:"period"` // date (daily) or "YYYY-Www" (weekly)
	Count  int    `json:"count"`
}

// DecompositionFindingsResponse is the response body for GET /api/decomposition-findings.
type DecompositionFindingsResponse struct {
	Counts       []FindingCount `json:"counts"`
	Recent       []DecompositionFinding `json:"recent"`
	DailyTrends  []FindingTrend `json:"daily_trends"`
	WeeklyTrends []FindingTrend `json:"weekly_trends"`
}

// handleDecompositionFindings queries decomposition_findings in learnings.db and
// returns grouped counts by finding_type, the 50 most recent individual findings
// with workspace_id, and daily/weekly trend aggregations.
func (s *APIServer) handleDecompositionFindings(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)

	base, err := config.Dir()
	if err != nil {
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}

	dbPath := filepath.Join(base, "learnings.db")
	db, err := sql.Open("sqlite", "file:"+dbPath+"?mode=ro")
	if err != nil {
		http.Error(w, fmt.Sprintf("opening learnings.db: %v", err), http.StatusInternalServerError)
		return
	}
	defer db.Close()

	resp := DecompositionFindingsResponse{
		Counts:       []FindingCount{},
		Recent:       []DecompositionFinding{},
		DailyTrends:  []FindingTrend{},
		WeeklyTrends: []FindingTrend{},
	}

	// Grouped counts by finding_type.
	countRows, err := db.QueryContext(r.Context(), `
		SELECT finding_type, COUNT(*) AS count
		FROM decomposition_findings
		GROUP BY finding_type
		ORDER BY count DESC`)
	if err != nil {
		http.Error(w, fmt.Sprintf("querying counts: %v", err), http.StatusInternalServerError)
		return
	}
	defer countRows.Close()
	for countRows.Next() {
		var fc FindingCount
		if err := countRows.Scan(&fc.FindingType, &fc.Count); err != nil {
			continue
		}
		resp.Counts = append(resp.Counts, fc)
	}
	countRows.Close()

	// 50 most recent findings.
	recentRows, err := db.QueryContext(r.Context(), `
		SELECT id, workspace_id, target_id, finding_type, phase_name,
		       detail, decomp_source, audit_score, created_at
		FROM decomposition_findings
		ORDER BY created_at DESC
		LIMIT 50`)
	if err != nil {
		http.Error(w, fmt.Sprintf("querying recent: %v", err), http.StatusInternalServerError)
		return
	}
	defer recentRows.Close()
	for recentRows.Next() {
		var f DecompositionFinding
		if err := recentRows.Scan(&f.ID, &f.WorkspaceID, &f.TargetID, &f.FindingType,
			&f.PhaseName, &f.Detail, &f.DecompSource, &f.AuditScore, &f.CreatedAt); err != nil {
			continue
		}
		resp.Recent = append(resp.Recent, f)
	}
	recentRows.Close()

	// Daily counts for the last 30 days.
	dailyRows, err := db.QueryContext(r.Context(), `
		SELECT DATE(created_at) AS date, COUNT(*) AS count
		FROM decomposition_findings
		WHERE created_at >= DATE('now', '-30 days')
		GROUP BY DATE(created_at)
		ORDER BY date ASC`)
	if err != nil {
		http.Error(w, fmt.Sprintf("querying daily trends: %v", err), http.StatusInternalServerError)
		return
	}
	defer dailyRows.Close()
	for dailyRows.Next() {
		var t FindingTrend
		if err := dailyRows.Scan(&t.Period, &t.Count); err != nil {
			continue
		}
		resp.DailyTrends = append(resp.DailyTrends, t)
	}
	dailyRows.Close()

	// Weekly counts for the last 90 days.
	weeklyRows, err := db.QueryContext(r.Context(), `
		SELECT STRFTIME('%Y-W%W', created_at) AS week, COUNT(*) AS count
		FROM decomposition_findings
		WHERE created_at >= DATE('now', '-90 days')
		GROUP BY STRFTIME('%Y-W%W', created_at)
		ORDER BY week ASC`)
	if err != nil {
		http.Error(w, fmt.Sprintf("querying weekly trends: %v", err), http.StatusInternalServerError)
		return
	}
	defer weeklyRows.Close()
	for weeklyRows.Next() {
		var t FindingTrend
		if err := weeklyRows.Scan(&t.Period, &t.Count); err != nil {
			continue
		}
		resp.WeeklyTrends = append(resp.WeeklyTrends, t)
	}

	writeJSON(w, resp)
}

// ---- missions run endpoint -----------------------------------------------

// runMissionFlags holds optional CLI flags forwarded to orchestrator run.
type runMissionFlags struct {
	NoReview   bool   `json:"no_review"`
	NoGit      bool   `json:"no_git"`
	Sequential bool   `json:"sequential"`
	Model      string `json:"model,omitempty"`
}

// runMissionRequest is the request body for POST /api/missions/run.
type runMissionRequest struct {
	Task   string           `json:"task"`
	Domain string           `json:"domain,omitempty"`
	Flags  *runMissionFlags `json:"flags,omitempty"`
}

// handleRunMission spawns orchestrator run in a background process and returns
// a request ID with status "accepted". The orchestrator generates its own
// workspace ID; the real mission_id arrives via SSE mission.started event.
func (s *APIServer) handleRunMission(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)

	var req runMissionRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		http.Error(w, fmt.Sprintf("invalid request body: %v", err), http.StatusBadRequest)
		return
	}
	if req.Task == "" {
		http.Error(w, "task is required", http.StatusBadRequest)
		return
	}

	bin, err := os.Executable()
	if err != nil {
		http.Error(w, fmt.Sprintf("resolving binary path: %v", err), http.StatusInternalServerError)
		return
	}

	// Build: orchestrator [--domain d] [--sequential] [--model m] run [--no-review] [--no-git] <task>
	var args []string
	if req.Domain != "" {
		args = append(args, "--domain", req.Domain)
	}
	if req.Flags != nil {
		if req.Flags.Sequential {
			args = append(args, "--sequential")
		}
		if req.Flags.Model != "" {
			args = append(args, "--model", req.Flags.Model)
		}
	}
	args = append(args, "run")
	if req.Flags != nil {
		if req.Flags.NoReview {
			args = append(args, "--no-review")
		}
		if req.Flags.NoGit {
			args = append(args, "--no-git")
		}
	}
	args = append(args, req.Task)

	requestID := generateRequestID()

	cmd := exec.Command(bin, args...)
	cmd.Stdout = os.Stderr
	cmd.Stderr = os.Stderr
	// Setpgid creates a new process group so we can SIGTERM/SIGKILL the
	// orchestrator and all its children together on daemon shutdown.
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}

	if err := cmd.Start(); err != nil {
		http.Error(w, fmt.Sprintf("starting mission process: %v", err), http.StatusInternalServerError)
		return
	}

	rm := &runningMission{
		cmd:       cmd,
		requestID: requestID,
		task:      req.Task,
		startedAt: time.Now(),
		done:      make(chan struct{}),
	}
	s.procMu.Lock()
	s.procTable[requestID] = rm
	s.procMu.Unlock()

	go func() {
		if waitErr := cmd.Wait(); waitErr != nil {
			fmt.Fprintf(os.Stderr, "daemon: mission %s (%q) exited: %v\n", requestID, req.Task, waitErr)
		}
		s.procMu.Lock()
		delete(s.procTable, requestID)
		s.procMu.Unlock()
		close(rm.done)
	}()

	w.WriteHeader(http.StatusAccepted)
	writeJSON(w, map[string]string{
		"status":     "accepted",
		"request_id": requestID,
		"task":       req.Task,
	})
}

// generateRequestID returns an opaque correlation ID for a launched mission
// request in the same YYYYMMDD-hex format used by core.CreateWorkspace.
func generateRequestID() string {
	ts := time.Now().Format("20060102")
	b := make([]byte, 4)
	rand.Read(b) //nolint:errcheck
	return fmt.Sprintf("%s-%s", ts, hex.EncodeToString(b))
}

// ---- shared subcommand executor ------------------------------------------

// execSubcommand resolves the daemon binary, runs it with args under timeout,
// and writes a JSON response with output/stderr/error fields. The HTTP status
// is always 200 — subcommand failures are reported in the body's "error" field
// so callers can distinguish API errors from subcommand errors.
func (s *APIServer) execSubcommand(w http.ResponseWriter, r *http.Request, timeout time.Duration, args ...string) {
	applyCORS(w, r)

	bin, err := os.Executable()
	if err != nil {
		http.Error(w, fmt.Sprintf("resolving binary path: %v", err), http.StatusInternalServerError)
		return
	}

	ctx, cancel := context.WithTimeout(r.Context(), timeout)
	defer cancel()

	cmd := exec.CommandContext(ctx, bin, args...)
	var outBuf, errBuf bytes.Buffer
	cmd.Stdout = &outBuf
	cmd.Stderr = &errBuf

	runErr := cmd.Run()

	resp := map[string]any{
		"output": outBuf.String(),
	}
	if errBuf.Len() > 0 {
		resp["stderr"] = errBuf.String()
	}
	if runErr != nil {
		resp["error"] = runErr.Error()
	}
	writeJSON(w, resp)
}


// ---- cleanup endpoint ----------------------------------------------------

// handleCleanup runs orchestrator cleanup --worktrees synchronously and returns its output.
func (s *APIServer) handleCleanup(w http.ResponseWriter, r *http.Request) {
	s.execSubcommand(w, r, 2*time.Minute, "cleanup", "--worktrees")
}

// handleListPlugins scans plugins/*/plugin.json under the nanika root directory
// and returns an array of plugin manifests that have api_version >= 1.
func (s *APIServer) handleListPlugins(w http.ResponseWriter, r *http.Request) {
	home, _ := os.UserHomeDir()
	pluginsDir := filepath.Join(home, "nanika", "plugins")
	entries, err := os.ReadDir(pluginsDir)
	if err != nil {
		writeJSON(w, []any{})
		return
	}

	type pluginManifest struct {
		Name     string   `json:"name"`
		Version  string   `json:"version,omitempty"`
		Desc     string   `json:"description,omitempty"`
		Kind     string   `json:"kind,omitempty"`
		APIVer   int      `json:"api_version,omitempty"`
		Binary   string   `json:"binary,omitempty"`
		Provides []string `json:"provides,omitempty"`
		UI       string   `json:"ui,omitempty"`
		Tags     []string `json:"tags,omitempty"`
	}

	var plugins []pluginManifest
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		pj := filepath.Join(pluginsDir, e.Name(), "plugin.json")
		data, err := os.ReadFile(pj)
		if err != nil {
			continue
		}
		var m pluginManifest
		if err := json.Unmarshal(data, &m); err != nil {
			continue
		}
		if m.APIVer < 1 {
			continue
		}
		if m.Name == "" {
			m.Name = e.Name()
		}
		plugins = append(plugins, m)
	}

	writeJSON(w, plugins)
}

// handlePluginInfo returns the plugin.json manifest for a single plugin.
func (s *APIServer) handlePluginInfo(w http.ResponseWriter, r *http.Request) {
	name := r.PathValue("name")
	home, _ := os.UserHomeDir()
	pj := filepath.Join(home, "nanika", "plugins", name, "plugin.json")
	data, err := os.ReadFile(pj)
	if err != nil {
		http.Error(w, "plugin not found", http.StatusNotFound)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	w.Write(data)
}

// handlePluginQuery returns a handler that execs `<binary> query <queryType> --json`
// and streams the result back. Used for status and items.
func (s *APIServer) handlePluginQuery(queryType string) func(http.ResponseWriter, *http.Request) {
	return func(w http.ResponseWriter, r *http.Request) {
		name := r.PathValue("name")
		binary := s.resolvePluginBinary(name)
		if binary == "" {
			http.Error(w, "plugin binary not found", http.StatusNotFound)
			return
		}
		ctx, cancel := context.WithTimeout(r.Context(), 15*time.Second)
		defer cancel()
		cmd := exec.CommandContext(ctx, binary, "query", queryType, "--json")
		out, err := cmd.Output()
		if err != nil {
			http.Error(w, fmt.Sprintf("plugin query failed: %v", err), http.StatusInternalServerError)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		w.Write(out)
	}
}

// handlePluginAction execs `<binary> query action <verb> [<id>] --json`.
// Body: {"verb": "run", "id": "33"}
func (s *APIServer) handlePluginAction(w http.ResponseWriter, r *http.Request) {
	name := r.PathValue("name")
	binary := s.resolvePluginBinary(name)
	if binary == "" {
		http.Error(w, "plugin binary not found", http.StatusNotFound)
		return
	}

	var body struct {
		Verb string `json:"verb"`
		ID   string `json:"id,omitempty"`
	}
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil || body.Verb == "" {
		http.Error(w, "missing verb", http.StatusBadRequest)
		return
	}

	args := []string{"query", "action", body.Verb}
	if body.ID != "" {
		args = append(args, body.ID)
	}
	args = append(args, "--json")

	ctx, cancel := context.WithTimeout(r.Context(), 30*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, binary, args...)
	out, err := cmd.Output()
	if err != nil {
		http.Error(w, fmt.Sprintf("plugin action failed: %v", err), http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	w.Write(out)
}

// resolvePluginBinary finds the binary for a plugin by checking plugin.json
// then falling back to ~/nanika/bin/<name>.
func (s *APIServer) resolvePluginBinary(name string) string {
	home, _ := os.UserHomeDir()
	// Try plugin.json binary field first
	pj := filepath.Join(home, "nanika", "plugins", name, "plugin.json")
	if data, err := os.ReadFile(pj); err == nil {
		var m struct {
			Binary string `json:"binary"`
		}
		if json.Unmarshal(data, &m) == nil && m.Binary != "" {
			if path, err := exec.LookPath(m.Binary); err == nil {
				return path
			}
		}
	}
	// Fallback: ~/nanika/bin/<name>
	binPath := filepath.Join(home, "nanika", "bin", name)
	if _, err := os.Stat(binPath); err == nil {
		return binPath
	}
	return ""
}

func ensureDomainStats(m map[string]*DomainStats, domain string) *DomainStats {
	if ds, ok := m[domain]; ok {
		return ds
	}
	ds := &DomainStats{}
	m[domain] = ds
	return ds
}

func ensurePersonaStats(m map[string]*PersonaStats, p string) *PersonaStats {
	if ps, ok := m[p]; ok {
		return ps
	}
	ps := &PersonaStats{}
	m[p] = ps
	return ps
}

// ---- nen findings endpoint -----------------------------------------------

// nenFindingScope is the nested scope object the frontend Finding type expects.
type nenFindingScope struct {
	Kind  string `json:"kind"`
	Value string `json:"value"`
}

// NenFinding matches the Finding type in the frontend: scope is nested,
// evidence is a JSON array parsed from the DB column.
type NenFinding struct {
	ID           string          `json:"id"`
	Ability      string          `json:"ability"`
	Category     string          `json:"category"`
	Severity     string          `json:"severity"`
	Title        string          `json:"title"`
	Description  string          `json:"description"`
	Scope        nenFindingScope `json:"scope"`
	Evidence     json.RawMessage `json:"evidence"`
	Source       string          `json:"source"`
	FoundAt      string          `json:"found_at"`
	ExpiresAt    string          `json:"expires_at,omitempty"`
	SupersededBy string          `json:"superseded_by,omitempty"`
	CreatedAt    string          `json:"created_at"`
}

// handleFindings queries ~/.alluka/nen/findings.db and returns active findings.
// Supports optional query params: ability, severity, limit.
func (s *APIServer) handleFindings(w http.ResponseWriter, r *http.Request) {
	applyCORS(w, r)

	base, err := config.Dir()
	if err != nil {
		http.Error(w, "internal error", http.StatusInternalServerError)
		return
	}

	dbPath := filepath.Join(base, "nen", "findings.db")
	if _, err := os.Stat(dbPath); os.IsNotExist(err) {
		writeJSON(w, []NenFinding{})
		return
	}
	db, err := sql.Open("sqlite", "file:"+dbPath+"?mode=ro&_busy_timeout=5000")
	if err != nil {
		http.Error(w, fmt.Sprintf("opening findings.db: %v", err), http.StatusInternalServerError)
		return
	}
	defer db.Close()

	q := r.URL.Query()
	ability := q.Get("ability")
	severity := q.Get("severity")
	limit := 500
	if l := q.Get("limit"); l != "" {
		if n, err := strconv.Atoi(l); err == nil && n > 0 {
			limit = n
		}
	}

	query := `
		SELECT id, ability, category, severity, title, description,
		       scope_kind, scope_value, evidence, source, found_at,
		       COALESCE(expires_at, ''), superseded_by, created_at
		FROM findings
		WHERE superseded_by = ''
		  AND (expires_at IS NULL OR expires_at > datetime('now'))`
	var args []any
	if ability != "" {
		query += " AND ability = ?"
		args = append(args, ability)
	}
	if severity != "" {
		query += " AND severity = ?"
		args = append(args, severity)
	}
	query += " ORDER BY found_at DESC LIMIT ?"
	args = append(args, limit)

	rows, err := db.QueryContext(r.Context(), query, args...)
	if err != nil {
		http.Error(w, fmt.Sprintf("querying findings: %v", err), http.StatusInternalServerError)
		return
	}
	defer rows.Close()

	findings := []NenFinding{}
	for rows.Next() {
		var f NenFinding
		var scopeKind, scopeValue, evidenceRaw string
		if err := rows.Scan(
			&f.ID, &f.Ability, &f.Category, &f.Severity, &f.Title, &f.Description,
			&scopeKind, &scopeValue, &evidenceRaw, &f.Source, &f.FoundAt,
			&f.ExpiresAt, &f.SupersededBy, &f.CreatedAt,
		); err != nil {
			continue
		}
		f.Scope = nenFindingScope{Kind: scopeKind, Value: scopeValue}
		if json.Valid([]byte(evidenceRaw)) {
			f.Evidence = json.RawMessage(evidenceRaw)
		} else {
			f.Evidence = json.RawMessage("[]")
		}
		findings = append(findings, f)
	}
	if err := rows.Err(); err != nil {
		http.Error(w, fmt.Sprintf("scanning findings: %v", err), http.StatusInternalServerError)
		return
	}

	writeJSON(w, findings)
}
