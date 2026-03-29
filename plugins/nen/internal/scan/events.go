package scan

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// EventsDir returns the path to the orchestrator event logs directory.
func EventsDir() (string, error) {
	base, err := Dir()
	if err != nil {
		return "", err
	}
	return filepath.Join(base, "events"), nil
}

// DaemonSocketPath returns the path to the orchestrator daemon unix socket.
func DaemonSocketPath() (string, error) {
	base, err := Dir()
	if err != nil {
		return "", fmt.Errorf("config dir: %w", err)
	}
	return filepath.Join(base, "daemon.sock"), nil
}

// EventsSocketPath returns the path to the orchestrator broadcast socket.
// The daemon listens here and writes all bus events as newline-delimited JSON
// to every connected client.
func EventsSocketPath() (string, error) {
	base, err := Dir()
	if err != nil {
		return "", fmt.Errorf("config dir: %w", err)
	}
	return filepath.Join(base, "events.sock"), nil
}

// CountPhaseRetryingEvents scans a JSONL event log file for phase.retrying events
// that occurred after since. Corrupt lines are silently skipped.
func CountPhaseRetryingEvents(ctx context.Context, path string, since time.Time) (int, error) {
	f, err := os.Open(path)
	if err != nil {
		return 0, fmt.Errorf("open %s: %w", path, err)
	}
	defer f.Close()

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 256*1024), 256*1024)

	count := 0
	for sc.Scan() {
		if ctx.Err() != nil {
			return count, ctx.Err()
		}
		line := sc.Text()
		if !strings.Contains(line, `"phase.retrying"`) {
			continue
		}
		var ev struct {
			Type      string `json:"type"`
			Timestamp string `json:"timestamp"`
		}
		if err := json.Unmarshal([]byte(line), &ev); err != nil {
			continue
		}
		if ev.Type != "phase.retrying" {
			continue
		}
		ts, err := time.Parse(time.RFC3339, ev.Timestamp)
		if err != nil {
			ts, err = time.Parse(time.RFC3339Nano, ev.Timestamp)
			if err != nil {
				continue
			}
		}
		if ts.After(since) {
			count++
		}
	}
	return count, sc.Err()
}

// FindSilentFailures returns phase IDs where a worker.failed event occurred
// without a corresponding phase.failed event in the same event log file.
func FindSilentFailures(logPath string) ([]string, error) {
	f, err := os.Open(logPath)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, fmt.Errorf("open %s: %w", logPath, err)
	}
	defer f.Close()

	workerFailed := make(map[string]bool)
	phaseFailed := make(map[string]bool)

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 1<<20), 1<<20)
	for sc.Scan() {
		var ev struct {
			Type    string `json:"type"`
			PhaseID string `json:"phase_id"`
		}
		if err := json.Unmarshal(sc.Bytes(), &ev); err != nil {
			continue
		}
		switch ev.Type {
		case "worker.failed":
			if ev.PhaseID != "" {
				workerFailed[ev.PhaseID] = true
			}
		case "phase.failed":
			if ev.PhaseID != "" {
				phaseFailed[ev.PhaseID] = true
			}
		}
	}
	if err := sc.Err(); err != nil {
		return nil, fmt.Errorf("scanning %s: %w", logPath, err)
	}

	var silent []string
	for phaseID := range workerFailed {
		if !phaseFailed[phaseID] {
			silent = append(silent, phaseID)
		}
	}
	return silent, nil
}
