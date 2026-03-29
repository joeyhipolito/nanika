package event

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/config"
)

const (
	udsDialTimeout  = 100 * time.Millisecond
	udsWriteTimeout = 2 * time.Second
	udsDialRetries  = 3
	udsDialBackoff  = 50 * time.Millisecond
)

// UDSEmitter delivers events to a daemon over a Unix domain socket.
//
// The daemon is optional infrastructure: if the socket is unavailable, events
// are dropped and logged to stderr. Emit never blocks for more than
// udsDialTimeout per attempt (up to udsDialRetries attempts on startup).
// Write operations time out after udsWriteTimeout. A broken connection is
// re-dialed lazily on the next Emit call.
type UDSEmitter struct {
	path          string
	mu            sync.Mutex
	conn          net.Conn
	closed        bool
	dialFailed    bool // true when last dial attempt failed — suppresses repeated log lines
	droppedWrites atomic.Int64
}

// NewUDSEmitter creates an emitter that delivers to the UDS at path.
// The connection is established lazily on the first Emit call.
func NewUDSEmitter(path string) *UDSEmitter {
	return &UDSEmitter{path: path}
}

// Emit serialises ev as newline-terminated JSON and writes it to the daemon socket.
// If the socket is unreachable the event is dropped and an error is logged to stderr.
func (u *UDSEmitter) Emit(_ context.Context, ev Event) {
	u.mu.Lock()
	defer u.mu.Unlock()

	if u.closed {
		return
	}

	// Lazy connect with retry. Retries only when the socket file exists so
	// that a missing daemon fails fast without sleeping between attempts.
	if u.conn == nil {
		conn, err := u.dialWithRetry()
		if err != nil {
			u.droppedWrites.Add(1)
			if !u.dialFailed {
				fmt.Fprintf(os.Stderr, "uds: connect to daemon socket %s failed: %v (events will be dropped until daemon is reachable)\n", u.path, err)
				u.dialFailed = true
			}
			return
		}
		if u.dialFailed {
			fmt.Fprintf(os.Stderr, "uds: connected to daemon socket %s\n", u.path)
			u.dialFailed = false
		}
		u.conn = conn
	}

	data, err := json.Marshal(ev)
	if err != nil {
		return // should never happen with well-formed events
	}
	data = append(data, '\n')

	if err := u.conn.SetWriteDeadline(time.Now().Add(udsWriteTimeout)); err != nil {
		fmt.Fprintf(os.Stderr, "uds: SetWriteDeadline: %v\n", err)
	}
	if _, err := u.conn.Write(data); err != nil {
		u.droppedWrites.Add(1)
		fmt.Fprintf(os.Stderr, "uds: write to daemon socket failed: %v\n", err)
		u.conn.Close()
		u.conn = nil // reconnect on next Emit
	}
}

// dialWithRetry attempts to connect to the UDS, retrying up to udsDialRetries
// times when the socket file exists (daemon is starting up). If the socket
// file does not exist the first failure is returned immediately.
func (u *UDSEmitter) dialWithRetry() (net.Conn, error) {
	var lastErr error
	for i := 0; i < udsDialRetries; i++ {
		conn, err := net.DialTimeout("unix", u.path, udsDialTimeout)
		if err == nil {
			return conn, nil
		}
		lastErr = err
		if i < udsDialRetries-1 {
			// Only sleep and retry if the socket file exists — daemon may be
			// starting up. If the file is absent the daemon is not running.
			if _, statErr := os.Stat(u.path); os.IsNotExist(statErr) {
				return nil, lastErr
			}
			time.Sleep(udsDialBackoff)
		}
	}
	return nil, lastErr
}

// DroppedWrites returns the cumulative count of events that could not be
// delivered to the daemon — either because the socket was unreachable or a
// write failed. A non-zero value is normal when the daemon is not running;
// events are still persisted by the FileEmitter in that case.
func (u *UDSEmitter) DroppedWrites() int64 {
	return u.droppedWrites.Load()
}

// DropStats reports UDS delivery losses for operator-visible summaries.
func (u *UDSEmitter) DropStats() DropStats {
	return DropStats{UDSDroppedWrites: u.DroppedWrites()}
}

// Close shuts down the UDS connection.
func (u *UDSEmitter) Close() error {
	u.mu.Lock()
	defer u.mu.Unlock()
	u.closed = true
	if u.conn != nil {
		err := u.conn.Close()
		u.conn = nil
		return err
	}
	return nil
}

// DaemonSocketPath returns the canonical UDS socket path: ~/.alluka/daemon.sock.
func DaemonSocketPath() (string, error) {
	d, err := config.Dir()
	if err != nil {
		return "", fmt.Errorf("getting config dir: %w", err)
	}
	return filepath.Join(d, "daemon.sock"), nil
}

// DaemonPIDPath returns the canonical PID file path: ~/.alluka/daemon.pid.
func DaemonPIDPath() (string, error) {
	d, err := config.Dir()
	if err != nil {
		return "", fmt.Errorf("getting config dir: %w", err)
	}
	return filepath.Join(d, "daemon.pid"), nil
}

// EventsSocketPath returns the canonical broadcast socket path: ~/.alluka/events.sock.
// The daemon listens here and writes all bus events as newline-delimited JSON to
// every connected client. External tools (socat, nc, custom consumers) can
// subscribe to the live event stream by connecting to this socket.
func EventsSocketPath() (string, error) {
	d, err := config.Dir()
	if err != nil {
		return "", fmt.Errorf("getting config dir: %w", err)
	}
	return filepath.Join(d, "events.sock"), nil
}
