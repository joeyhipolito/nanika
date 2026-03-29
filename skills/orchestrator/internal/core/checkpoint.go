package core

import (
	"encoding/json"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// Checkpoint holds the state needed to resume a mission.
type Checkpoint struct {
	Version     int       `json:"version"`
	WorkspaceID string    `json:"workspace_id"`
	Domain      string    `json:"domain"`
	Plan        *Plan     `json:"plan"`
	Status      string    `json:"status"`    // "in_progress", "completed", "failed"
	StartedAt   time.Time `json:"started_at"` // zero value for v1 checkpoints (backward compat)
	// Linking metadata — populated from workspace sidecar files when present.
	// These fields are not written to checkpoint.json; they are loaded on demand
	// by LoadCheckpoint to give callers (audit, status, etc.) a single struct
	// that carries both execution state and provenance.
	LinearIssueID string `json:"linear_issue_id,omitempty"` // e.g. "V-5"
	MissionPath   string `json:"mission_path,omitempty"`    // absolute path to source mission file

	// Git isolation fields — zero values for missions without worktree isolation
	// (backward compatible with older checkpoints that lack these fields).
	GitRepoRoot  string `json:"git_repo_root,omitempty"`  // root of the target git repository
	WorktreePath string `json:"worktree_path,omitempty"`  // path to the linked worktree
	BranchName   string `json:"branch_name,omitempty"`    // e.g. "via/<mission-id>/<slug>"
	BaseBranch   string `json:"base_branch,omitempty"`    // branch that BranchName was cut from
}

// checkpointEnvelope wraps a Checkpoint with a version field for integrity checking.
// This allows detection of truncated or corrupted writes.
type checkpointEnvelope struct {
	Version  int        `json:"version"` // envelope version (currently 1)
	Payload  Checkpoint `json:"payload"`
}

// SaveCheckpoint writes the checkpoint to disk.
func SaveCheckpoint(wsPath string, plan *Plan, domain, status string, startedAt time.Time) error {
	return saveCheckpointInternal(wsPath, plan, domain, status, startedAt, nil)
}

// SaveCheckpointFull is like SaveCheckpoint but also serializes git isolation
// fields from ws into the checkpoint JSON. Pass nil for ws when git isolation
// is not in use — this behaves identically to SaveCheckpoint.
func SaveCheckpointFull(wsPath string, plan *Plan, domain, status string, startedAt time.Time, ws *Workspace) error {
	return saveCheckpointInternal(wsPath, plan, domain, status, startedAt, ws)
}

func saveCheckpointInternal(wsPath string, plan *Plan, domain, status string, startedAt time.Time, ws *Workspace) error {
	cp := Checkpoint{
		Version:     2,
		WorkspaceID: filepath.Base(wsPath),
		Domain:      domain,
		Plan:        plan,
		Status:      status,
		StartedAt:   startedAt,
	}
	if ws != nil {
		cp.GitRepoRoot  = ws.GitRepoRoot
		cp.WorktreePath = ws.WorktreePath
		cp.BranchName   = ws.BranchName
		cp.BaseBranch   = ws.BaseBranch
	}

	// Wrap checkpoint in envelope with version for integrity checking.
	envelope := checkpointEnvelope{
		Version: 1,
		Payload: cp,
	}

	data, err := json.MarshalIndent(envelope, "", "  ")
	if err != nil {
		return fmt.Errorf("marshal checkpoint: %w", err)
	}

	// Atomic write: write to temporary file, then rename.
	// This prevents truncated writes if the process crashes mid-write.
	cpPath := filepath.Join(wsPath, "checkpoint.json")
	tmpPath := cpPath + ".tmp"

	// Write to temporary file first.
	if err := os.WriteFile(tmpPath, data, 0600); err != nil {
		return fmt.Errorf("write checkpoint temp: %w", err)
	}

	// Rename is atomic on POSIX systems.
	if err := os.Rename(tmpPath, cpPath); err != nil {
		// Try to clean up the temp file on rename failure.
		_ = os.Remove(tmpPath)
		return fmt.Errorf("rename checkpoint temp to final: %w", err)
	}

	return nil
}

// LoadCheckpoint reads a checkpoint from disk and enriches it with linking
// metadata from workspace sidecar files (linear_issue_id, mission_path).
// Missing sidecar files are silently ignored — they are optional.
// On parse failure, logs a clear error message for operator debugging.
// Supports both new envelope format and legacy direct checkpoint format
// for backward compatibility.
func LoadCheckpoint(wsPath string) (*Checkpoint, error) {
	cpPath := filepath.Join(wsPath, "checkpoint.json")
	data, err := os.ReadFile(cpPath)
	if err != nil {
		return nil, fmt.Errorf("read checkpoint: %w", err)
	}

	// Try legacy direct checkpoint format first (for backward compatibility).
	// This ensures old checkpoints parse correctly before trying the envelope.
	var cp Checkpoint
	legacyErr := json.Unmarshal(data, &cp)
	if legacyErr == nil && cp.WorkspaceID != "" {
		// Success: legacy format (detected by non-empty workspace_id which is always set).
		enrichCheckpointSidecars(wsPath, &cp)
		return &cp, nil
	}

	// Try envelope format.
	var env checkpointEnvelope
	envelopeErr := json.Unmarshal(data, &env)
	if envelopeErr == nil && env.Payload.WorkspaceID != "" {
		// Success: envelope format.
		cp := env.Payload
		enrichCheckpointSidecars(wsPath, &cp)
		return &cp, nil
	}

	// Neither format worked. Log clear error message for operator debugging.
	log.Printf("failed to parse checkpoint from %s: legacy format error: %v, envelope format error: %v\n", cpPath, legacyErr, envelopeErr)
	return nil, fmt.Errorf("parse checkpoint: %w", legacyErr)
}

// enrichCheckpointSidecars loads linking metadata from workspace sidecar files.
func enrichCheckpointSidecars(wsPath string, cp *Checkpoint) {
	if raw, err := os.ReadFile(filepath.Join(wsPath, "linear_issue_id")); err == nil {
		cp.LinearIssueID = strings.TrimSpace(string(raw))
	}
	if raw, err := os.ReadFile(filepath.Join(wsPath, "mission_path")); err == nil {
		cp.MissionPath = strings.TrimSpace(string(raw))
	}
}
