// Package daemon provides a best-effort event emitter that delivers scout
// events to the Nanika orchestrator daemon over its Unix domain socket.
//
// The daemon is optional infrastructure: if the socket is unavailable, events
// are dropped silently. Emit never blocks the caller.
package daemon

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"sync"
	"time"
)

const (
	udsDialTimeout  = 100 * time.Millisecond
	udsWriteTimeout = 2 * time.Second
)

// EventType mirrors the orchestrator's event type string.
type EventType = string

const EventTypeScoutIntelGathered EventType = "scout.intel_gathered"

// event is a minimal envelope matching the daemon's expected JSON format.
type event struct {
	ID        string         `json:"id"`
	Type      EventType      `json:"type"`
	Timestamp time.Time      `json:"timestamp"`
	Sequence  int64          `json:"sequence"`
	MissionID string         `json:"mission_id"`
	Data      map[string]any `json:"data,omitempty"`
}

// Emitter delivers scout events to the daemon UDS.
// It is safe for concurrent use. The connection is established lazily and
// re-established if broken. All operations are best-effort.
type Emitter struct {
	path string

	mu         sync.Mutex
	conn       net.Conn
	closed     bool
	dialFailed bool
}

// NewEmitter returns an Emitter targeting the canonical daemon socket
// (~/.alluka/daemon.sock). Returns an error only if the home directory
// cannot be determined; a missing daemon is not an error.
func NewEmitter() (*Emitter, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil, fmt.Errorf("daemon emitter: resolving home dir: %w", err)
	}
	return &Emitter{path: filepath.Join(home, ".alluka", "daemon.sock")}, nil
}

// EmitIntelGathered sends a scout.intel_gathered event for a single topic.
// Errors are written to stderr; the function never returns an error so it
// can be called fire-and-forget.
func (e *Emitter) EmitIntelGathered(topic string, itemCount int) {
	ev := event{
		ID:        newEventID(),
		Type:      EventTypeScoutIntelGathered,
		Timestamp: time.Now().UTC(),
		MissionID: "scout",
		Data: map[string]any{
			"topic":      topic,
			"item_count": itemCount,
		},
	}
	e.emit(ev)
}

// emit serialises ev and writes it to the daemon socket.
func (e *Emitter) emit(ev event) {
	e.mu.Lock()
	defer e.mu.Unlock()

	if e.closed {
		return
	}

	if e.conn == nil {
		conn, err := net.DialTimeout("unix", e.path, udsDialTimeout)
		if err != nil {
			e.droppedLog(err)
			return
		}
		if e.dialFailed {
			fmt.Fprintf(os.Stderr, "scout: daemon connected\n")
			e.dialFailed = false
		}
		e.conn = conn
	}

	data, err := json.Marshal(ev)
	if err != nil {
		return // should never happen
	}
	data = append(data, '\n')

	if err := e.conn.SetWriteDeadline(time.Now().Add(udsWriteTimeout)); err != nil {
		fmt.Fprintf(os.Stderr, "scout: daemon write deadline: %v\n", err)
	}
	if _, err := e.conn.Write(data); err != nil {
		fmt.Fprintf(os.Stderr, "scout: daemon write failed: %v\n", err)
		e.conn.Close()
		e.conn = nil
	}
}

func (e *Emitter) droppedLog(err error) {
	if !e.dialFailed {
		// Only log once to avoid spamming stderr when daemon is not running.
		fmt.Fprintf(os.Stderr, "scout: daemon unreachable (%v); events dropped\n", err)
		e.dialFailed = true
	}
}

// Close shuts down the UDS connection.
func (e *Emitter) Close() {
	e.mu.Lock()
	defer e.mu.Unlock()
	e.closed = true
	if e.conn != nil {
		e.conn.Close()
		e.conn = nil
	}
}

func newEventID() string {
	b := make([]byte, 8)
	if _, err := rand.Read(b); err != nil {
		return fmt.Sprintf("evt_%d", time.Now().UnixNano())
	}
	return "evt_" + hex.EncodeToString(b)
}
