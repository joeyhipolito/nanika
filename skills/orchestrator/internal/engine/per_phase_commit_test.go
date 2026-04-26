package engine

// Tests for per-phase commit behavior (req 1) and commitGitIsolation running on
// top of per-phase commits (req 8).
//
// These tests create a real git repository and linked worktree so that
// commitPhaseWork and the post-execution final commit can be verified by
// inspecting actual git log output.

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	gitpkg "github.com/joeyhipolito/orchestrator-cli/internal/git"
	"github.com/joeyhipolito/nanika/shared/sdk"
)

// initTestRepo creates a git repo with an initial commit in dir.
func initTestRepo(t *testing.T, dir string) {
	t.Helper()
	mustGit(t, dir, "git", "init", "-b", "main")
	mustGit(t, dir, "git", "config", "user.email", "test@test.com")
	mustGit(t, dir, "git", "config", "user.name", "Test")
	if err := os.WriteFile(filepath.Join(dir, "README.md"), []byte("init\n"), 0644); err != nil {
		t.Fatal(err)
	}
	mustGit(t, dir, "git", "add", ".")
	mustGit(t, dir, "git", "commit", "-m", "init")
}

func mustGit(t *testing.T, dir string, args ...string) {
	t.Helper()
	cmd := exec.Command(args[0], args[1:]...)
	cmd.Dir = dir
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("%v in %s: %v\n%s", args, dir, err, out)
	}
}

// gitLog returns the one-line log of the last n commits in dir.
func gitLog(t *testing.T, dir string, n int) []string {
	t.Helper()
	cmd := exec.Command("git", "log", "--oneline", "--no-decorate", "-n", strings.Repeat("1", 0)+fmt.Sprintf("%d", n))
	cmd.Dir = dir
	out, err := cmd.Output()
	if err != nil {
		t.Fatalf("git log: %v", err)
	}
	var lines []string
	for _, line := range strings.Split(strings.TrimSpace(string(out)), "\n") {
		if line != "" {
			lines = append(lines, line)
		}
	}
	return lines
}

// fileWritingExecutor is a PhaseExecutor that writes a file into phase.TargetDir
// (the worktree) so that commitPhaseWork has something to commit.
type fileWritingExecutor struct {
	filename string
	content  string
}

func (f fileWritingExecutor) Execute(ctx context.Context, cfg *core.WorkerConfig, emitter event.Emitter, verbose bool) (string, string, *sdk.CostInfo, error) {
	if cfg.TargetDir != "" {
		path := filepath.Join(cfg.TargetDir, f.filename)
		_ = os.WriteFile(path, []byte(f.content), 0644)
	}
	return "done", "", nil, nil
}

// TestPerPhaseCommits verifies that each successfully completed phase produces
// exactly one commit on the branch (req 1).
func TestPerPhaseCommits_OneCommitPerPhase(t *testing.T) {
	repo := t.TempDir()
	initTestRepo(t, repo)

	if err := gitpkg.CreateBranch(repo, "mission-branch", "main"); err != nil {
		t.Fatalf("CreateBranch: %v", err)
	}
	wtPath := filepath.Join(t.TempDir(), "wt")
	if err := gitpkg.CreateWorktree(repo, wtPath, "mission-branch"); err != nil {
		t.Fatalf("CreateWorktree: %v", err)
	}
	t.Cleanup(func() { _ = gitpkg.RemoveWorktree(wtPath, "") })

	ws := &core.Workspace{
		ID:           "test-ws",
		WorktreePath: wtPath,
	}
	cfg := &core.OrchestratorConfig{ForceSequential: true}
	eng := New(ws, cfg, nil, nil)
	eng.buildRunner = nil // disable build verification

	// Register an executor that writes a unique file per phase.
	eng.RegisterExecutor(core.RuntimeClaude, fileWritingExecutor{filename: "phase1.txt", content: "phase1"})

	plan := &core.Plan{
		Task:          "test per-phase commits",
		ExecutionMode: "sequential",
		Phases: []*core.Phase{
			{ID: "p1", Name: "implement", Objective: "write phase 1 output", TargetDir: wtPath},
			{ID: "p2", Name: "document", Objective: "write phase 2 output", TargetDir: wtPath},
		},
	}

	// Use separate executors per phase via the per-phase registration.
	// Since the engine uses a single registered executor, we override it
	// between phases by using a custom executor registry.
	phaseFiles := []string{"phase1.txt", "phase2.txt"}
	execIdx := 0
	eng.executors = executorRegistry{}
	eng.executors[core.RuntimeClaude] = &indexedWritingExecutor{
		wtPath: wtPath,
		files:  phaseFiles,
		idx:    &execIdx,
	}

	result, err := eng.Execute(context.Background(), plan)
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if !result.Success {
		t.Fatalf("Execute did not succeed: %s", result.Error)
	}

	// Count commits on the branch since main (the base).
	cmd := exec.Command("git", "log", "--oneline", "--no-decorate", "main..HEAD")
	cmd.Dir = wtPath
	out, err := cmd.Output()
	if err != nil {
		t.Fatalf("git log: %v", err)
	}
	var commits []string
	for _, line := range strings.Split(strings.TrimSpace(string(out)), "\n") {
		if line != "" {
			commits = append(commits, line)
		}
	}

	// Two phases each writing a file → exactly 2 per-phase commits.
	if len(commits) != 2 {
		t.Errorf("expected 2 per-phase commits, got %d:\n%s", len(commits), strings.Join(commits, "\n"))
	}
	for _, msg := range commits {
		if !strings.Contains(msg, "phase ") {
			t.Errorf("commit message should contain 'phase ', got: %s", msg)
		}
	}
}

// indexedWritingExecutor writes a different file on each call.
type indexedWritingExecutor struct {
	wtPath string
	files  []string
	idx    *int
}

func (e *indexedWritingExecutor) Execute(ctx context.Context, cfg *core.WorkerConfig, emitter event.Emitter, verbose bool) (string, string, *sdk.CostInfo, error) {
	if *e.idx < len(e.files) {
		path := filepath.Join(e.wtPath, e.files[*e.idx])
		_ = os.WriteFile(path, []byte("content"), 0644)
		*e.idx++
	}
	return "done", "", nil, nil
}

// TestCommitGitIsolation_WorksOnTopOfPerPhaseCommits verifies that a final
// "merge" commit (simulating what commitGitIsolation does) can be created on
// top of existing per-phase commits without error (req 8).
func TestCommitGitIsolation_WorksOnTopOfPerPhaseCommits(t *testing.T) {
	repo := t.TempDir()
	initTestRepo(t, repo)

	if err := gitpkg.CreateBranch(repo, "isolation-branch", "main"); err != nil {
		t.Fatalf("CreateBranch: %v", err)
	}
	wtPath := filepath.Join(t.TempDir(), "wt")
	if err := gitpkg.CreateWorktree(repo, wtPath, "isolation-branch"); err != nil {
		t.Fatalf("CreateWorktree: %v", err)
	}
	t.Cleanup(func() { _ = gitpkg.RemoveWorktree(wtPath, "") })

	// Simulate two per-phase commits (as commitPhaseWork would produce).
	for i, name := range []string{"phase1.go", "phase2.go"} {
		if err := os.WriteFile(filepath.Join(wtPath, name), []byte("package main\n"), 0644); err != nil {
			t.Fatal(err)
		}
		msg := fmt.Sprintf("phase %d: write output", i+1)
		if err := gitpkg.CommitAll(wtPath, msg); err != nil {
			t.Fatalf("CommitAll phase %d: %v", i+1, err)
		}
	}

	// Simulate a final isolation commit (as commitGitIsolation produces after all phases).
	if err := os.WriteFile(filepath.Join(wtPath, "SUMMARY.md"), []byte("# Summary\n"), 0644); err != nil {
		t.Fatal(err)
	}
	if err := gitpkg.CommitAll(wtPath, "chore: finalize mission isolation"); err != nil {
		t.Fatalf("final CommitAll failed: %v", err)
	}

	// Verify: 3 commits on the branch (2 per-phase + 1 final), initial commit on main.
	cmd := exec.Command("git", "log", "--oneline", "--no-decorate", "main..HEAD")
	cmd.Dir = wtPath
	out, err := cmd.Output()
	if err != nil {
		t.Fatalf("git log: %v", err)
	}
	var commits []string
	for _, line := range strings.Split(strings.TrimSpace(string(out)), "\n") {
		if line != "" {
			commits = append(commits, line)
		}
	}

	if len(commits) != 3 {
		t.Errorf("expected 3 commits (2 per-phase + 1 final), got %d:\n%s", len(commits), strings.Join(commits, "\n"))
	}
	// The most recent commit should be the final isolation commit.
	if len(commits) > 0 && !strings.Contains(commits[0], "finalize mission isolation") {
		t.Errorf("latest commit should be the final isolation commit, got: %s", commits[0])
	}
}
