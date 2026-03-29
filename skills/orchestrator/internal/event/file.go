package event

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sync"
	"sync/atomic"

	"github.com/joeyhipolito/orchestrator-cli/internal/config"
)

// FileEmitter writes events as newline-delimited JSON (JSONL) to a file.
//
// The file is created with 0600 permissions (user-only) so event logs —
// which may contain mission context — are not world-readable.
// Writes are serialised under a mutex; the sequence counter is atomic.
// I/O errors do not propagate to callers (the event log is best-effort),
// but the count of dropped writes is tracked and exposed via DroppedWrites.
type FileEmitter struct {
	mu            sync.Mutex
	f             *os.File
	seq           atomic.Int64
	droppedWrites atomic.Int64
}

// NewFileEmitter opens (or creates) a JSONL event log at path.
// The parent directory is created with 0700 permissions if it does not exist.
func NewFileEmitter(path string) (*FileEmitter, error) {
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return nil, fmt.Errorf("creating event log directory: %w", err)
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0600)
	if err != nil {
		return nil, fmt.Errorf("opening event log %s: %w", path, err)
	}
	return &FileEmitter{f: f}, nil
}

// Emit appends the event as a JSONL record.
// If a parent MultiEmitter has already assigned a sequence number, it is used
// as-is. Only when Sequence is zero (standalone use, no MultiEmitter parent)
// does FileEmitter assign its own monotonic counter.
// Malformed events (json.Marshal failure) are silently dropped to avoid
// corrupting the log file.
func (fe *FileEmitter) Emit(_ context.Context, ev Event) {
	if ev.Sequence == 0 {
		ev.Sequence = fe.seq.Add(1)
	}

	data, err := json.Marshal(ev)
	if err != nil {
		return // drop rather than corrupt the log
	}

	fe.mu.Lock()
	defer fe.mu.Unlock()

	if _, err := fe.f.Write(data); err != nil {
		fe.droppedWrites.Add(1)
		return
	}
	if _, err := fe.f.Write([]byte{'\n'}); err != nil {
		fe.droppedWrites.Add(1)
	}
}

// DroppedWrites returns the cumulative count of events that could not be
// written to the log file due to I/O errors. A non-zero value means the
// event log is incomplete and warrants operator attention.
func (fe *FileEmitter) DroppedWrites() int64 {
	return fe.droppedWrites.Load()
}

// DropStats reports file-backed delivery losses for operator-visible
// summaries. FileEmitter only contributes file-write drops.
func (fe *FileEmitter) DropStats() DropStats {
	return DropStats{FileDroppedWrites: fe.DroppedWrites()}
}

// Close flushes and closes the underlying file.
func (fe *FileEmitter) Close() error {
	fe.mu.Lock()
	defer fe.mu.Unlock()
	return fe.f.Close()
}

// LastSequence reads the JSONL event log at path and returns the highest
// sequence number found. Returns 0 if the file does not exist or is empty.
// Corrupt lines are skipped so a partial write never blocks resume.
func LastSequence(path string) (int64, error) {
	f, err := os.Open(path)
	if err != nil {
		if os.IsNotExist(err) {
			return 0, nil
		}
		return 0, fmt.Errorf("opening event log for sequence scan: %w", err)
	}
	defer f.Close()

	var maxSeq int64
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 1<<20), 1<<20) // 1 MB max line — guard against large payloads
	for scanner.Scan() {
		var ev Event
		if err := json.Unmarshal(scanner.Bytes(), &ev); err != nil {
			continue // skip corrupt lines
		}
		if ev.Sequence > maxSeq {
			maxSeq = ev.Sequence
		}
	}
	return maxSeq, scanner.Err()
}

// EventLogPath returns the canonical path for a mission's event log:
//
//	~/.alluka/events/<mission_id>.jsonl
func EventLogPath(missionID string) (string, error) {
	d, err := config.Dir()
	if err != nil {
		return "", fmt.Errorf("getting config dir: %w", err)
	}
	return filepath.Join(d, "events", missionID+".jsonl"), nil
}

// EventLogsDir returns the directory that holds all mission event logs.
func EventLogsDir() (string, error) {
	d, err := config.Dir()
	if err != nil {
		return "", fmt.Errorf("getting config dir: %w", err)
	}
	return filepath.Join(d, "events"), nil
}
