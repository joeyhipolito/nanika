package cmd

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/config"
	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

// initGitRepo initialises a real git repository with an initial commit.
// Mirrors the helper used in the git package tests.
func initGitRepo(t *testing.T, dir string) {
	t.Helper()
	mustGit(t, dir, "init", "-b", "main")
	mustGit(t, dir, "config", "user.email", "test@example.com")
	mustGit(t, dir, "config", "user.name", "Test User")
	readmePath := filepath.Join(dir, "README.md")
	if err := os.WriteFile(readmePath, []byte("# test\n"), 0o644); err != nil {
		t.Fatalf("write README: %v", err)
	}
	mustGit(t, dir, "add", ".")
	mustGit(t, dir, "commit", "-m", "init")
}

func mustGit(t *testing.T, dir string, args ...string) {
	t.Helper()
	cmd := exec.Command("git", args...)
	cmd.Dir = dir
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("git %s in %s: %v\n%s", strings.Join(args, " "), dir, err, out)
	}
}

// setConfigDir redirects config.Dir() to cfgDir for the duration of the test.
func setConfigDir(t *testing.T, cfgDir string) {
	t.Helper()
	t.Setenv(config.EnvVar, cfgDir)
}

// TestSetupGitIsolation_CreatesWorktreeAndSetsFields verifies that
// setupGitIsolation creates a linked worktree, updates ws.TargetDir,
// and populates all four git fields.
func TestSetupGitIsolation_CreatesWorktreeAndSetsFields(t *testing.T) {
	repo := t.TempDir()
	initGitRepo(t, repo)

	cfgDir := t.TempDir()
	setConfigDir(t, cfgDir)

	ws := &core.Workspace{
		ID:        "20260316-test0001",
		Path:      t.TempDir(),
		Task:      "add feature X",
		TargetDir: repo,
	}

	if err := setupGitIsolation(ws, "add feature X", event.NoOpEmitter{}, ws.ID); err != nil {
		t.Fatalf("setupGitIsolation: %v", err)
	}

	// WorktreePath must exist on disk.
	if ws.WorktreePath == "" {
		t.Fatal("WorktreePath is empty after setup")
	}
	if _, err := os.Stat(ws.WorktreePath); err != nil {
		t.Fatalf("worktree not on disk at %s: %v", ws.WorktreePath, err)
	}

	// BranchName must follow the via/<id>/<slug> convention.
	if !strings.HasPrefix(ws.BranchName, "via/") {
		t.Errorf("BranchName = %q, want via/<id>/... prefix", ws.BranchName)
	}

	// BaseBranch must be "main" (what initGitRepo created).
	if ws.BaseBranch != "main" {
		t.Errorf("BaseBranch = %q, want main", ws.BaseBranch)
	}

	// GitRepoRoot must equal the repo directory.
	if ws.GitRepoRoot != repo {
		t.Errorf("GitRepoRoot = %q, want %q", ws.GitRepoRoot, repo)
	}

	// TargetDir must be updated to the worktree path.
	if ws.TargetDir != ws.WorktreePath {
		t.Errorf("TargetDir = %q, want WorktreePath %q", ws.TargetDir, ws.WorktreePath)
	}

	// WorktreePath must be under cfgDir/worktrees/<id>.
	wantWorktree := filepath.Join(cfgDir, "worktrees", ws.ID)
	if ws.WorktreePath != wantWorktree {
		t.Errorf("WorktreePath = %q, want %q", ws.WorktreePath, wantWorktree)
	}
}

// TestSetupGitIsolation_NotAGitRepo verifies that setupGitIsolation returns an
// error (rather than panicking) when TargetDir is not inside a git repository.
func TestSetupGitIsolation_NotAGitRepo(t *testing.T) {
	dir := t.TempDir() // plain directory, no .git

	cfgDir := t.TempDir()
	setConfigDir(t, cfgDir)

	ws := &core.Workspace{
		ID:        "20260316-test0002",
		Path:      t.TempDir(),
		TargetDir: dir,
	}

	if err := setupGitIsolation(ws, "task", event.NoOpEmitter{}, ws.ID); err == nil {
		t.Fatal("expected error for non-git dir, got nil")
	}
}

// TestTeardownGitIsolation_SuccessCommitsAndRemovesWorktree runs the full
// worktree lifecycle: create → write a file → teardown(success=true), then
// verifies the commit was created and the worktree directory was removed.
func TestTeardownGitIsolation_SuccessCommitsAndRemovesWorktree(t *testing.T) {
	repo := t.TempDir()
	initGitRepo(t, repo)

	cfgDir := t.TempDir()
	setConfigDir(t, cfgDir)

	ws := &core.Workspace{
		ID:        "20260316-test0003",
		Path:      t.TempDir(),
		Task:      "write output",
		TargetDir: repo,
	}

	if err := setupGitIsolation(ws, "write output", event.NoOpEmitter{}, ws.ID); err != nil {
		t.Fatalf("setupGitIsolation: %v", err)
	}

	// Write a file in the worktree to give CommitAll something to stage.
	outFile := filepath.Join(ws.WorktreePath, "output.txt")
	if err := os.WriteFile(outFile, []byte("result\n"), 0o644); err != nil {
		t.Fatalf("write output file: %v", err)
	}

	teardownGitIsolation(ws, true, "write output", event.NoOpEmitter{}, ws.ID)

	// Worktree directory must have been removed.
	if _, err := os.Stat(ws.WorktreePath); !os.IsNotExist(err) {
		t.Errorf("worktree still exists at %s after successful teardown", ws.WorktreePath)
	}

	// The commit must appear in the branch's log in the main repo.
	cmd := exec.Command("git", "log", "--oneline", ws.BranchName)
	cmd.Dir = repo
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("git log %s: %v\n%s", ws.BranchName, err, out)
	}
	if !strings.Contains(string(out), "via(") {
		t.Errorf("expected via(...) commit in log, got:\n%s", out)
	}
}

// TestTeardownGitIsolation_FailurePreservesWorktree verifies that on failure
// the worktree directory is left intact for user inspection.
func TestTeardownGitIsolation_FailurePreservesWorktree(t *testing.T) {
	repo := t.TempDir()
	initGitRepo(t, repo)

	cfgDir := t.TempDir()
	setConfigDir(t, cfgDir)

	ws := &core.Workspace{
		ID:        "20260316-test0004",
		Path:      t.TempDir(),
		TargetDir: repo,
	}

	if err := setupGitIsolation(ws, "fix bug", event.NoOpEmitter{}, ws.ID); err != nil {
		t.Fatalf("setupGitIsolation: %v", err)
	}

	savedPath := ws.WorktreePath

	teardownGitIsolation(ws, false, "fix bug", event.NoOpEmitter{}, ws.ID)

	// Worktree must still exist on disk.
	if _, err := os.Stat(savedPath); err != nil {
		t.Errorf("worktree was removed on failure, should be preserved: %v", err)
	}
}

// TestTeardownGitIsolation_NoopWhenNoWorktree verifies that teardownGitIsolation
// is a no-op when no git isolation was configured (WorktreePath is empty).
func TestTeardownGitIsolation_NoopWhenNoWorktree(t *testing.T) {
	ws := &core.Workspace{
		ID:   "20260316-test0005",
		Path: t.TempDir(),
	}
	// Must not panic or error.
	teardownGitIsolation(ws, true, "task", event.NoOpEmitter{}, ws.ID)
	teardownGitIsolation(ws, false, "task", event.NoOpEmitter{}, ws.ID)
	teardownGitIsolation(nil, true, "task", event.NoOpEmitter{}, "")
}

// TestGitIsolation_CheckpointRoundtrip verifies that SaveCheckpointFull
// serialises git fields into checkpoint.json and LoadCheckpoint reads them back.
func TestGitIsolation_CheckpointRoundtrip(t *testing.T) {
	wsDir := t.TempDir()

	ws := &core.Workspace{
		ID:           "20260316-roundtrip",
		Path:         wsDir,
		GitRepoRoot:  "/some/repo",
		WorktreePath: "/some/worktrees/20260316-roundtrip",
		BranchName:   "via/20260316-roundtrip/fix-bug",
		BaseBranch:   "main",
	}

	plan := &core.Plan{ID: "p1", Task: "fix bug"}
	if err := core.SaveCheckpointFull(wsDir, plan, "dev", "in_progress", time.Time{}, ws); err != nil {
		t.Fatalf("SaveCheckpointFull: %v", err)
	}

	cp, err := core.LoadCheckpoint(wsDir)
	if err != nil {
		t.Fatalf("LoadCheckpoint: %v", err)
	}

	if cp.GitRepoRoot != ws.GitRepoRoot {
		t.Errorf("GitRepoRoot: got %q, want %q", cp.GitRepoRoot, ws.GitRepoRoot)
	}
	if cp.WorktreePath != ws.WorktreePath {
		t.Errorf("WorktreePath: got %q, want %q", cp.WorktreePath, ws.WorktreePath)
	}
	if cp.BranchName != ws.BranchName {
		t.Errorf("BranchName: got %q, want %q", cp.BranchName, ws.BranchName)
	}
	if cp.BaseBranch != ws.BaseBranch {
		t.Errorf("BaseBranch: got %q, want %q", cp.BaseBranch, ws.BaseBranch)
	}
}

// TestGitIsolation_OldCheckpointCompatible verifies that a checkpoint.json
// that pre-dates git isolation (no git fields) loads without error, and the
// git fields default to empty strings (zero values).
func TestGitIsolation_OldCheckpointCompatible(t *testing.T) {
	wsDir := t.TempDir()

	// Write a v2 checkpoint without any git fields (simulating an old checkpoint).
	oldJSON := `{"version":2,"workspace_id":"old-ws","domain":"dev","plan":{"id":"p1"},"status":"completed"}`
	if err := os.WriteFile(filepath.Join(wsDir, "checkpoint.json"), []byte(oldJSON), 0o600); err != nil {
		t.Fatalf("write checkpoint: %v", err)
	}

	cp, err := core.LoadCheckpoint(wsDir)
	if err != nil {
		t.Fatalf("LoadCheckpoint on old checkpoint: %v", err)
	}
	if cp.GitRepoRoot != "" || cp.WorktreePath != "" || cp.BranchName != "" || cp.BaseBranch != "" {
		t.Errorf("expected zero git fields for old checkpoint, got non-empty: root=%q wt=%q branch=%q base=%q",
			cp.GitRepoRoot, cp.WorktreePath, cp.BranchName, cp.BaseBranch)
	}
}
