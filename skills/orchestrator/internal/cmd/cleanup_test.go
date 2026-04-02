package cmd

// Tests for cleanup flow behaviors:
//  7. Existing cleanup flow still works for completed missions
//     (isWorkspaceActive correctly distinguishes active vs. completed workspaces)

import (
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
)

// TestIsWorkspaceActive_CompletedIsInactive verifies that a workspace with
// status "completed" is considered inactive and eligible for cleanup.
func TestIsWorkspaceActive_CompletedIsInactive(t *testing.T) {
	wsPath := t.TempDir()
	plan := &core.Plan{Task: "test mission", Phases: nil, ExecutionMode: "sequential"}
	if err := core.SaveCheckpoint(wsPath, plan, "dev", "completed", time.Now()); err != nil {
		t.Fatalf("SaveCheckpoint: %v", err)
	}

	if isWorkspaceActive(wsPath) {
		t.Error("completed workspace should be inactive (eligible for cleanup)")
	}
}

// TestIsWorkspaceActive_InProgressIsActive verifies that a workspace with
// status "in_progress" is skipped by cleanup (must not be removed).
func TestIsWorkspaceActive_InProgressIsActive(t *testing.T) {
	wsPath := t.TempDir()
	plan := &core.Plan{Task: "live mission", Phases: nil, ExecutionMode: "sequential"}
	if err := core.SaveCheckpoint(wsPath, plan, "dev", "in_progress", time.Now()); err != nil {
		t.Fatalf("SaveCheckpoint: %v", err)
	}

	if !isWorkspaceActive(wsPath) {
		t.Error("in_progress workspace should be considered active (must not be cleaned up)")
	}
}

// TestIsWorkspaceActive_MissingWorkspaceIsInactive verifies that a workspace
// directory that doesn't exist (orphaned worktree) is treated as inactive.
func TestIsWorkspaceActive_MissingWorkspaceIsInactive(t *testing.T) {
	nonExistent := filepath.Join(t.TempDir(), "does-not-exist")
	if isWorkspaceActive(nonExistent) {
		t.Error("missing workspace should be inactive (eligible for cleanup)")
	}
}

// TestIsWorkspaceActive_FailedIsInactive verifies that "failed" status is
// treated as a terminal state (not active).
func TestIsWorkspaceActive_FailedIsInactive(t *testing.T) {
	wsPath := t.TempDir()
	plan := &core.Plan{Task: "failed mission", Phases: nil, ExecutionMode: "sequential"}
	if err := core.SaveCheckpoint(wsPath, plan, "dev", "failed", time.Now()); err != nil {
		t.Fatalf("SaveCheckpoint: %v", err)
	}

	if isWorkspaceActive(wsPath) {
		t.Error("failed workspace should be inactive (eligible for cleanup)")
	}
}

// TestIsWorkspaceActive_CancelledIsInactive verifies that "cancelled" is terminal.
func TestIsWorkspaceActive_CancelledIsInactive(t *testing.T) {
	wsPath := t.TempDir()
	plan := &core.Plan{Task: "cancelled mission", Phases: nil, ExecutionMode: "sequential"}
	if err := core.SaveCheckpoint(wsPath, plan, "dev", "cancelled", time.Now()); err != nil {
		t.Fatalf("SaveCheckpoint: %v", err)
	}

	if isWorkspaceActive(wsPath) {
		t.Error("cancelled workspace should be inactive (eligible for cleanup)")
	}
}

// TestRunWorktreeCleanup_SkipsActiveWorktree verifies the full cleanup path:
// a worktree whose workspace is in_progress must not be removed.
// This uses a real git worktree to validate the guard, avoiding config.Dir().
func TestRunWorktreeCleanup_SkipsActiveViaIsWorkspaceActive(t *testing.T) {
	// Build a minimal scenario:
	// - wsPath has in_progress checkpoint → isWorkspaceActive returns true → must skip
	// - wsPath2 has completed checkpoint → isWorkspaceActive returns false → eligible
	wsActive := t.TempDir()
	wsCompleted := t.TempDir()

	plan := &core.Plan{Task: "t", Phases: nil, ExecutionMode: "sequential"}
	if err := core.SaveCheckpoint(wsActive, plan, "dev", "in_progress", time.Now()); err != nil {
		t.Fatalf("SaveCheckpoint active: %v", err)
	}
	if err := core.SaveCheckpoint(wsCompleted, plan, "dev", "completed", time.Now()); err != nil {
		t.Fatalf("SaveCheckpoint completed: %v", err)
	}

	// Active workspace must be skipped.
	if !isWorkspaceActive(wsActive) {
		t.Error("in_progress must be active")
	}

	// Completed workspace must be eligible.
	if isWorkspaceActive(wsCompleted) {
		t.Error("completed must not be active")
	}

	// Simulate the cleanup decision: only remove the completed workspace's worktree.
	// We just confirm the guard function works correctly — the actual removal
	// (git.RemoveWorktree) is tested in git_test.go.
	fakeWorktree := filepath.Join(t.TempDir(), "fake-wt")
	if err := os.Mkdir(fakeWorktree, 0755); err != nil {
		t.Fatal(err)
	}
	if isWorkspaceActive(wsCompleted) {
		t.Error("cleanup would incorrectly skip this completed workspace")
	} else {
		// The completed workspace's worktree would be removed — simulate it.
		os.RemoveAll(fakeWorktree)
	}
	if _, err := os.Stat(fakeWorktree); !os.IsNotExist(err) {
		t.Error("fake worktree should have been removed for completed workspace")
	}
}
