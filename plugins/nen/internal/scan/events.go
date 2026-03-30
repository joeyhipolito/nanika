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

// RetryEvent holds the details extracted from a single phase.retrying event.
type RetryEvent struct {
	PhaseID   string
	WorkerID  string
	Error     string
	Attempt   int
	Timestamp time.Time
}

// MissionRetryInfo holds the retry events and mission context collected from one log file.
type MissionRetryInfo struct {
	Events []RetryEvent
	Task   string // first heading from mission.started task text, or empty
}

// CountPhaseRetryingEvents scans a JSONL event log file for phase.retrying events
// that occurred after since. Corrupt lines are silently skipped.
func CountPhaseRetryingEvents(ctx context.Context, path string, since time.Time) (int, error) {
	info, err := CollectMissionRetryInfo(ctx, path, since)
	return len(info.Events), err
}

// CollectMissionRetryInfo scans a JSONL event log file in a single pass and returns
// the phase.retrying events that occurred after since and the mission title from
// the mission.started event. Corrupt lines are silently skipped.
func CollectMissionRetryInfo(ctx context.Context, path string, since time.Time) (MissionRetryInfo, error) {
	f, err := os.Open(path)
	if err != nil {
		return MissionRetryInfo{}, fmt.Errorf("open %s: %w", path, err)
	}
	defer f.Close()

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 1<<20), 1<<20)

	var info MissionRetryInfo
	for sc.Scan() {
		if ctx.Err() != nil {
			return info, ctx.Err()
		}
		line := sc.Text()

		if info.Task == "" && strings.Contains(line, `"mission.started"`) {
			var ev struct {
				Type string `json:"type"`
				Data struct {
					Task string `json:"task"`
				} `json:"data"`
			}
			if err := json.Unmarshal([]byte(line), &ev); err == nil && ev.Type == "mission.started" {
				info.Task = missionTitle(ev.Data.Task)
			}
			continue
		}

		if !strings.Contains(line, `"phase.retrying"`) {
			continue
		}
		var ev struct {
			Type      string    `json:"type"`
			Timestamp string    `json:"timestamp"`
			PhaseID   string    `json:"phase_id"`
			WorkerID  string    `json:"worker_id"`
			Data      struct {
				Error   string  `json:"error"`
				Attempt float64 `json:"attempt"`
			} `json:"data"`
		}
		if err := json.Unmarshal([]byte(line), &ev); err != nil || ev.Type != "phase.retrying" {
			continue
		}
		ts, err := time.Parse(time.RFC3339, ev.Timestamp)
		if err != nil {
			if ts, err = time.Parse(time.RFC3339Nano, ev.Timestamp); err != nil {
				continue
			}
		}
		if !ts.After(since) {
			continue
		}
		re := RetryEvent{
			PhaseID:   ev.PhaseID,
			WorkerID:  ev.WorkerID,
			Timestamp: ts,
			Error:     ev.Data.Error,
			Attempt:   int(ev.Data.Attempt),
		}
		info.Events = append(info.Events, re)
	}
	return info, sc.Err()
}

// missionTitle extracts the first markdown heading from a task string,
// falling back to the first non-empty non-frontmatter line.
func missionTitle(task string) string {
	// dashCount tracks YAML frontmatter delimiters: 0=before, 1=inside, 2=after.
	dashCount := 0
	firstNonEmpty := ""
	for _, line := range strings.Split(task, "\n") {
		line = strings.TrimSpace(line)
		if line == "---" && dashCount < 2 {
			dashCount++
			continue
		}
		if dashCount == 1 {
			continue
		}
		if strings.HasPrefix(line, "# ") {
			return strings.TrimPrefix(line, "# ")
		}
		if firstNonEmpty == "" && line != "" {
			firstNonEmpty = line
		}
	}
	if firstNonEmpty != "" {
		return firstNonEmpty
	}
	return task
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
	phaseCompleted := make(map[string]bool)

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
		case "phase.completed":
			if ev.PhaseID != "" {
				phaseCompleted[ev.PhaseID] = true
			}
		}
	}
	if err := sc.Err(); err != nil {
		return nil, fmt.Errorf("scanning %s: %w", logPath, err)
	}

	var silent []string
	for phaseID := range workerFailed {
		if !phaseFailed[phaseID] && !phaseCompleted[phaseID] {
			silent = append(silent, phaseID)
		}
	}
	return silent, nil
}
