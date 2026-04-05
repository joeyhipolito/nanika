package preflight

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/config"
	"github.com/joeyhipolito/orchestrator-cli/internal/core"
)

func init() {
	Register(&missionSection{})
}

type missionSection struct{}

func (m *missionSection) Name() string  { return "mission" }
func (m *missionSection) Priority() int { return 5 }

// Fetch finds the most recently modified checkpoint.json across all workspaces
// and returns a brief with mission ID, current phase, status, and last event
// timestamp. Returns an empty block when no workspaces or checkpoints exist.
func (m *missionSection) Fetch(_ context.Context) (Block, error) {
	info, err := latestMissionInfo()
	if err != nil || info == nil {
		return Block{Title: "Active Mission", Body: ""}, nil
	}
	return Block{Title: "Active Mission", Body: formatMissionBlock(info)}, nil
}

// missionInfo holds the extracted fields from the most recent checkpoint.
type missionInfo struct {
	MissionID    string
	CurrentPhase string
	Status       string
	LastEventAt  time.Time
}

// latestMissionInfo scans ~/.alluka/workspaces/ and returns the mission from
// the workspace whose checkpoint.json has the most recent mtime.
// Returns nil, nil when no workspaces exist or none have a checkpoint.
func latestMissionInfo() (*missionInfo, error) {
	base, err := config.Dir()
	if err != nil {
		return nil, fmt.Errorf("resolving config dir: %w", err)
	}
	wsBase := filepath.Join(base, "workspaces")

	entries, err := os.ReadDir(wsBase)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, fmt.Errorf("reading workspaces dir: %w", err)
	}

	var (
		latestMtime time.Time
		latestWsDir string
	)
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		cpPath := filepath.Join(wsBase, e.Name(), "checkpoint.json")
		fi, err := os.Stat(cpPath)
		if err != nil {
			continue
		}
		if fi.ModTime().After(latestMtime) {
			latestMtime = fi.ModTime()
			latestWsDir = filepath.Join(wsBase, e.Name())
		}
	}
	if latestWsDir == "" {
		return nil, nil
	}

	cp, err := core.LoadCheckpoint(latestWsDir)
	if err != nil {
		return nil, fmt.Errorf("loading checkpoint from %s: %w", latestWsDir, err)
	}

	info := &missionInfo{
		MissionID: cp.WorkspaceID,
		Status:    cp.Status,
	}
	if cp.Plan != nil {
		info.CurrentPhase = currentPhaseName(cp.Plan.Phases)
	}
	info.LastEventAt = lastEventTimestamp(base, cp.WorkspaceID, latestMtime)
	return info, nil
}

// currentPhaseName returns the running phase name, falling back to the most
// recently ended phase, then the most recently started phase.
func currentPhaseName(phases []*core.Phase) string {
	for _, p := range phases {
		if p.Status == core.StatusRunning {
			return p.Name
		}
	}
	var latest *core.Phase
	for _, p := range phases {
		if p.EndTime == nil {
			continue
		}
		if latest == nil || p.EndTime.After(*latest.EndTime) {
			latest = p
		}
	}
	if latest != nil {
		return latest.Name
	}
	for _, p := range phases {
		if p.StartTime != nil {
			return p.Name
		}
	}
	return ""
}

// lastEventTimestamp reads the event log for missionID and returns the
// timestamp of the last valid event. Falls back to checkpointMtime when
// the event log is absent or contains no parseable events.
func lastEventTimestamp(base, missionID string, checkpointMtime time.Time) time.Time {
	logPath := filepath.Join(base, "events", missionID+".jsonl")
	f, err := os.Open(logPath)
	if err != nil {
		return checkpointMtime
	}
	defer f.Close()

	var last time.Time
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 1<<20), 1<<20)
	for scanner.Scan() {
		var ev struct {
			Timestamp time.Time `json:"timestamp"`
		}
		if err := json.Unmarshal(scanner.Bytes(), &ev); err != nil {
			continue
		}
		if !ev.Timestamp.IsZero() {
			last = ev.Timestamp
		}
	}
	if last.IsZero() {
		return checkpointMtime
	}
	return last
}

func formatMissionBlock(info *missionInfo) string {
	var sb strings.Builder
	fmt.Fprintf(&sb, "id: %s\n", info.MissionID)
	fmt.Fprintf(&sb, "status: %s\n", info.Status)
	if info.CurrentPhase != "" {
		fmt.Fprintf(&sb, "phase: %s\n", info.CurrentPhase)
	}
	if !info.LastEventAt.IsZero() {
		fmt.Fprintf(&sb, "last_event: %s", info.LastEventAt.UTC().Format(time.RFC3339))
	}
	return strings.TrimRight(sb.String(), "\n")
}
