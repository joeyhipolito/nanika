package event

import (
	"sync"
	"time"
)

// MissionSnap is the minimal live snapshot of a mission derived from events.
// It carries only the fields needed for observability; structural plan data
// (phases list, dependencies, objectives) remains in the checkpoint.
type MissionSnap struct {
	MissionID string
	Status    string // "in_progress", "completed", "failed", "cancelled"
	StartedAt time.Time
	EndedAt   time.Time
	Phases    map[string]*PhaseSnap // keyed by phase ID
}

// PhaseSnap is the minimal live snapshot of a phase derived from events.
type PhaseSnap struct {
	ID        string
	Name      string
	// Status is one of "running", "completed", "failed", "skipped", or
	// "retrying". "retrying" is live-only: it is never persisted to a
	// checkpoint and will not appear in historical (replayed) snapshots.
	Status    string
	StartedAt time.Time
	EndedAt   time.Time
}

// LiveState maintains an in-memory projection of mission and phase status
// derived solely from events. It is intentionally separate from checkpoints:
// checkpoints handle crash recovery and structural plan data; LiveState
// handles low-latency observability of currently-running missions.
//
// Use LiveState.Mission to query live status for a specific mission.
// Fall back to the checkpoint when LiveState returns nil (historical missions
// whose events pre-date the current daemon session).
//
// The zero value is not usable; call NewLiveState.
type LiveState struct {
	mu        sync.RWMutex
	missions  map[string]*MissionSnap
	bus       *Bus
	subID     uint64
	done      chan struct{}
	closeOnce sync.Once
}

// NewLiveState attaches a new LiveState to b and starts consuming events.
// Call Close when done to release the bus subscription.
func NewLiveState(b *Bus) *LiveState {
	ls := &LiveState{
		missions: make(map[string]*MissionSnap),
		bus:      b,
		done:     make(chan struct{}),
	}
	id, ch := b.Subscribe()
	ls.subID = id
	go ls.consume(ch)
	return ls
}

// Mission returns a copy of the live snapshot for missionID, or nil if the
// mission has not been seen in this daemon session (e.g. historical missions).
func (ls *LiveState) Mission(missionID string) *MissionSnap {
	ls.mu.RLock()
	defer ls.mu.RUnlock()
	m := ls.missions[missionID]
	if m == nil {
		return nil
	}
	return m.clone()
}

// RunningMissions returns the IDs of missions whose live status is "in_progress".
// Only missions seen since this daemon session started are returned; historical
// missions that pre-date the session are not tracked by LiveState.
func (ls *LiveState) RunningMissions() []string {
	ls.mu.RLock()
	defer ls.mu.RUnlock()
	var ids []string
	for id, m := range ls.missions {
		if m.Status == "in_progress" {
			ids = append(ids, id)
		}
	}
	return ids
}

// Close stops consuming events and releases the bus subscription.
// Safe to call more than once; subsequent calls are no-ops.
func (ls *LiveState) Close() {
	ls.closeOnce.Do(func() {
		close(ls.done)
		ls.bus.Unsubscribe(ls.subID)
	})
}

func (ls *LiveState) consume(ch <-chan Event) {
	for {
		select {
		case ev, ok := <-ch:
			if !ok {
				return
			}
			ls.apply(ev)
		case <-ls.done:
			return
		}
	}
}

func (ls *LiveState) apply(ev Event) {
	mid := ev.MissionID
	if mid == "" {
		return
	}

	ls.mu.Lock()
	defer ls.mu.Unlock()

	switch ev.Type {
	case MissionStarted:
		if m, ok := ls.missions[mid]; ok {
			// Duplicate mission.started (at-least-once delivery, reconnect, etc.).
			// Preserve accumulated phase state — only reset the mission status.
			m.Status = "in_progress"
		} else {
			ls.missions[mid] = &MissionSnap{
				MissionID: mid,
				Status:    "in_progress",
				StartedAt: ev.Timestamp,
				Phases:    make(map[string]*PhaseSnap),
			}
		}

	case MissionCompleted:
		if m := ls.missions[mid]; m != nil {
			m.Status = "completed"
			m.EndedAt = ev.Timestamp
		}

	case MissionFailed:
		if m := ls.missions[mid]; m != nil {
			m.Status = "failed"
			m.EndedAt = ev.Timestamp
		}

	case MissionCancelled:
		if m := ls.missions[mid]; m != nil {
			m.Status = "cancelled"
			m.EndedAt = ev.Timestamp
		}

	case PhaseStarted:
		if ev.PhaseID == "" {
			return
		}
		m := ls.getOrCreateMission(mid, ev.Timestamp)
		name, _ := ev.Data["name"].(string)
		m.Phases[ev.PhaseID] = &PhaseSnap{
			ID:        ev.PhaseID,
			Name:      name,
			Status:    "running",
			StartedAt: ev.Timestamp,
		}

	case PhaseCompleted:
		ls.setPhaseStatus(mid, ev, "completed")

	case PhaseFailed:
		ls.setPhaseStatus(mid, ev, "failed")

	case PhaseSkipped:
		ls.setPhaseStatus(mid, ev, "skipped")

	case PhaseRetrying:
		ls.setPhaseStatus(mid, ev, "retrying")
	}
}

// getOrCreateMission returns the existing snap or synthesises one for a
// mission whose mission.started event was missed (e.g. late subscription).
func (ls *LiveState) getOrCreateMission(mid string, ts time.Time) *MissionSnap {
	if m, ok := ls.missions[mid]; ok {
		return m
	}
	m := &MissionSnap{
		MissionID: mid,
		Status:    "in_progress",
		StartedAt: ts,
		Phases:    make(map[string]*PhaseSnap),
	}
	ls.missions[mid] = m
	return m
}

func (ls *LiveState) setPhaseStatus(mid string, ev Event, status string) {
	if ev.PhaseID == "" {
		return
	}
	m := ls.getOrCreateMission(mid, ev.Timestamp)
	if p, ok := m.Phases[ev.PhaseID]; ok {
		p.Status = status
		p.EndedAt = ev.Timestamp
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

func (m *MissionSnap) clone() *MissionSnap {
	cp := *m
	phases := make(map[string]*PhaseSnap, len(m.Phases))
	for k, v := range m.Phases {
		p := *v
		phases[k] = &p
	}
	cp.Phases = phases
	return &cp
}
