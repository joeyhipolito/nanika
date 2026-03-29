package event

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"time"
)

// Projection is a synchronous, single-pass replay of an event log into an
// in-memory mission/phase snapshot. Unlike LiveState it has no bus subscription
// or background goroutine — it is designed for offline CLI queries where the
// event log is read once and discarded.
type Projection struct {
	missions map[string]*MissionSnap
}

func newProjection() *Projection {
	return &Projection{missions: make(map[string]*MissionSnap)}
}

// Mission returns a copy of the projected snapshot for missionID, or nil if
// no events for that mission were seen during replay.
func (p *Projection) Mission(missionID string) *MissionSnap {
	m := p.missions[missionID]
	if m == nil {
		return nil
	}
	return m.clone()
}

// apply updates the projection state based on ev, following the same state
// machine as LiveState.apply.
func (p *Projection) apply(ev Event) {
	mid := ev.MissionID
	if mid == "" {
		return
	}

	switch ev.Type {
	case MissionStarted:
		if m, ok := p.missions[mid]; ok {
			m.Status = "in_progress"
		} else {
			p.missions[mid] = &MissionSnap{
				MissionID: mid,
				Status:    "in_progress",
				StartedAt: ev.Timestamp,
				Phases:    make(map[string]*PhaseSnap),
			}
		}

	case MissionCompleted:
		if m := p.missions[mid]; m != nil {
			m.Status = "completed"
			m.EndedAt = ev.Timestamp
		}

	case MissionFailed:
		if m := p.missions[mid]; m != nil {
			m.Status = "failed"
			m.EndedAt = ev.Timestamp
		}

	case MissionCancelled:
		if m := p.missions[mid]; m != nil {
			m.Status = "cancelled"
			m.EndedAt = ev.Timestamp
		}

	case PhaseStarted:
		if ev.PhaseID == "" {
			return
		}
		m := p.getOrCreateMission(mid, ev.Timestamp)
		name, _ := ev.Data["name"].(string)
		m.Phases[ev.PhaseID] = &PhaseSnap{
			ID:        ev.PhaseID,
			Name:      name,
			Status:    "running",
			StartedAt: ev.Timestamp,
		}

	case PhaseCompleted:
		p.setPhaseStatus(mid, ev, "completed")

	case PhaseFailed:
		p.setPhaseStatus(mid, ev, "failed")

	case PhaseSkipped:
		p.setPhaseStatus(mid, ev, "skipped")

	case PhaseRetrying:
		p.setPhaseStatus(mid, ev, "retrying")
	}
}

func (p *Projection) getOrCreateMission(mid string, ts time.Time) *MissionSnap {
	if m, ok := p.missions[mid]; ok {
		return m
	}
	m := &MissionSnap{
		MissionID: mid,
		Status:    "in_progress",
		StartedAt: ts,
		Phases:    make(map[string]*PhaseSnap),
	}
	p.missions[mid] = m
	return m
}

func (p *Projection) setPhaseStatus(mid string, ev Event, status string) {
	if ev.PhaseID == "" {
		return
	}
	m := p.getOrCreateMission(mid, ev.Timestamp)
	if ph, ok := m.Phases[ev.PhaseID]; ok {
		ph.Status = status
		ph.EndedAt = ev.Timestamp
	} else {
		name, _ := ev.Data["name"].(string)
		m.Phases[ev.PhaseID] = &PhaseSnap{
			ID:      ev.PhaseID,
			Name:    name,
			Status:  status,
			EndedAt: ev.Timestamp,
		}
	}
}

// ProjectFromLog replays the persisted JSONL event log for missionID and
// returns the resulting MissionSnap. Returns nil (no error) when the event log
// does not exist — the caller should fall back to the checkpoint in that case.
// Corrupt or unrecognised lines are skipped so a partial write never blocks
// the query.
func ProjectFromLog(missionID string) (*MissionSnap, error) {
	logPath, err := EventLogPath(missionID)
	if err != nil {
		return nil, fmt.Errorf("resolving event log path: %w", err)
	}

	f, err := os.Open(logPath)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil // no log — caller should fall back to checkpoint
		}
		return nil, fmt.Errorf("opening event log %s: %w", logPath, err)
	}
	defer f.Close()

	proj := newProjection()
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 1<<20), 1<<20) // guard against large payloads
	for scanner.Scan() {
		var ev Event
		if err := json.Unmarshal(scanner.Bytes(), &ev); err != nil {
			continue // skip corrupt lines
		}
		proj.apply(ev)
	}
	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("scanning event log %s: %w", logPath, err)
	}

	return proj.Mission(missionID), nil
}
