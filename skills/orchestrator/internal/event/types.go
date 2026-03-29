// Package event defines the orchestrator's event system: typed constants,
// the Event envelope, and constructor helpers.
package event

import (
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"time"
)

// EventType is a typed string for event kinds.
type EventType string

// The 28 event type constants, grouped by category.
const (
	// Mission lifecycle (4)
	MissionStarted   EventType = "mission.started"
	MissionCompleted EventType = "mission.completed"
	MissionFailed    EventType = "mission.failed"
	MissionCancelled EventType = "mission.cancelled"

	// Phase lifecycle (5)
	PhaseStarted   EventType = "phase.started"
	PhaseCompleted EventType = "phase.completed"
	PhaseFailed    EventType = "phase.failed"
	PhaseSkipped   EventType = "phase.skipped"
	PhaseRetrying  EventType = "phase.retrying"

	// Worker lifecycle (4)
	WorkerSpawned   EventType = "worker.spawned"
	WorkerOutput    EventType = "worker.output"
	WorkerCompleted EventType = "worker.completed"
	WorkerFailed    EventType = "worker.failed"

	// Decomposition (3)
	DecomposeStarted   EventType = "decompose.started"
	DecomposeCompleted EventType = "decompose.completed"
	DecomposeFallback  EventType = "decompose.fallback"

	// Learning (2)
	LearningExtracted EventType = "learning.extracted"
	LearningStored    EventType = "learning.stored"

	// DAG progress (2)
	DAGDependencyResolved EventType = "dag.dependency_resolved"
	DAGPhaseDispatched    EventType = "dag.phase_dispatched"

	// Role handoffs (1)
	RoleHandoff EventType = "role.handoff"

	// Contract validation (2)
	ContractValidated EventType = "contract.validated"
	ContractViolated  EventType = "contract.violated"

	// Persona output contracts (1)
	PersonaContractViolation EventType = "persona.contract_violation"

	// Review loop (2)
	// ReviewFindingsEmitted is emitted once per review-gate completion and
	// carries all parsed findings (blockers + warnings). Emitted regardless of
	// whether the review passed, so non-blocking warnings and unresolved
	// blockers at loop-exhaustion are never silently discarded.
	ReviewFindingsEmitted EventType = "review.findings_emitted"
	// ReviewExternalRequested is emitted when an external code review (e.g.
	// Codex) is requested on the PR via a comment. Data includes "pr_url".
	ReviewExternalRequested EventType = "review.external_requested"

	// Git lifecycle (3)
	GitWorktreeCreated EventType = "git.worktree_created"
	GitCommitted       EventType = "git.committed"
	GitPRCreated       EventType = "git.pr_created"

	// System (2)
	SystemError           EventType = "system.error"
	SystemCheckpointSaved EventType = "system.checkpoint_saved"

	// Completion signals (3)
	// SignalScopeExpansion is emitted when a worker signals that the task scope
	// grew beyond what was planned. Data includes "summary" and "suggested_phases".
	SignalScopeExpansion EventType = "signal.scope_expansion"
	// SignalReplanRequired is emitted when a worker determines the current plan
	// cannot achieve the objective and a replan is needed.
	SignalReplanRequired EventType = "signal.replan_required"
	// SignalHumanDecisionNeeded is emitted when a worker encounters a decision
	// point that requires human judgement before proceeding.
	SignalHumanDecisionNeeded EventType = "signal.human_decision_needed"

	// File overlap (1)
	// FileOverlapDetected is emitted when parallel phases modify the same file.
	// Data includes "file" (string), "phases" ([]string of phase IDs), and
	// "severity" ("high" if both modified an existing file, "medium" if one created it).
	FileOverlapDetected EventType = "file_overlap.detected"

	// Security (2)
	// SecurityInvisibleCharsStripped is emitted when invisible Unicode characters
	// are detected and stripped from task/objective text before execution.
	// Data includes "count" (int), "types" ([]string of descriptions).
	SecurityInvisibleCharsStripped EventType = "security.invisible_chars_stripped"

	// SecurityInjectionDetected is emitted when pattern-based prompt injection
	// detection fires on task or objective text. Data includes "context"
	// (string: "channel-message" or "phase-objective"), "reasons" (string,
	// semicolon-separated list of matched pattern descriptions), and "sanitized"
	// (bool: true when block patterns fired and content was modified).
	SecurityInjectionDetected EventType = "security.injection_detected"
)

// Event is the envelope for all orchestrator events.
// Sequence is assigned by the Emitter; callers leave it as zero.
type Event struct {
	ID        string         `json:"id"`
	Type      EventType      `json:"type"`
	Timestamp time.Time      `json:"timestamp"`
	Sequence  int64          `json:"sequence"`
	MissionID string         `json:"mission_id"`
	PhaseID   string         `json:"phase_id,omitempty"`
	WorkerID  string         `json:"worker_id,omitempty"`
	Data      map[string]any `json:"data,omitempty"`
}

// New creates an Event with a generated ID and current UTC timestamp.
// Sequence is left at zero — the Emitter sets it on Emit.
func New(typ EventType, missionID, phaseID, workerID string, data map[string]any) Event {
	return Event{
		ID:        newEventID(),
		Type:      typ,
		Timestamp: time.Now().UTC(),
		MissionID: missionID,
		PhaseID:   phaseID,
		WorkerID:  workerID,
		Data:      data,
	}
}

func newEventID() string {
	b := make([]byte, 8)
	if _, err := rand.Read(b); err != nil {
		// Fallback: use nanosecond timestamp (unique enough for local logging)
		return fmt.Sprintf("evt_%d", time.Now().UnixNano())
	}
	return "evt_" + hex.EncodeToString(b)
}
