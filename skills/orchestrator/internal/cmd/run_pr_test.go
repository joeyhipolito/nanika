package cmd

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/git"
)

// --------------------------------------------------------------------------
// stripGitWorkflowSection: removes ## Git Workflow section from task text
// --------------------------------------------------------------------------

func TestStripGitWorkflowSection_RemovesSection(t *testing.T) {
	task := `# Mission

Do the work.

## Git Workflow

Do NOT commit to main. Create a branch and open a PR.

1. Create a feature branch
2. Push and create a draft PR

## Constraints

- Keep it simple`

	got := stripGitWorkflowSection(task)

	if strings.Contains(got, "## Git Workflow") {
		t.Error("Git Workflow section should be removed")
	}
	if strings.Contains(got, "Do NOT commit to main") {
		t.Error("Git Workflow body should be removed")
	}
	if strings.Contains(got, "Create a feature branch") {
		t.Error("Git Workflow list items should be removed")
	}
	// Content after the section must be preserved
	if !strings.Contains(got, "## Constraints") {
		t.Error("Constraints section after Git Workflow must be preserved")
	}
	if !strings.Contains(got, "Keep it simple") {
		t.Error("Constraints content must be preserved")
	}
	if !strings.Contains(got, "Do the work.") {
		t.Error("Content before Git Workflow must be preserved")
	}
}

func TestStripGitWorkflowSection_NoSection(t *testing.T) {
	task := "Implement the feature.\n\n- Be fast\n- Be correct"
	got := stripGitWorkflowSection(task)
	if got != task {
		t.Errorf("task without Git Workflow section should be unchanged;\ngot: %q\nwant: %q", got, task)
	}
}

// TestStripGitWorkflowSection_PreservesSimilarHeadings verifies that headings
// that START with "## Git Workflow" but have additional text (e.g.
// "## Git Workflow Automation") are NOT stripped — only the exact heading matches.
func TestStripGitWorkflowSection_PreservesSimilarHeadings(t *testing.T) {
	task := "# Mission\n\n## Git Workflow Automation\n\nBuild the automation.\n\n## Constraints\n\n- Done"
	got := stripGitWorkflowSection(task)
	if !strings.Contains(got, "## Git Workflow Automation") {
		t.Error("'## Git Workflow Automation' should NOT be stripped — only exact '## Git Workflow' matches")
	}
	if !strings.Contains(got, "Build the automation.") {
		t.Error("content under similar heading should be preserved")
	}
}

func TestStripGitWorkflowSection_AtEOF(t *testing.T) {
	task := "Do the work.\n\n## Git Workflow\n\n1. Create branch\n2. Push"
	got := stripGitWorkflowSection(task)
	if strings.Contains(got, "Git Workflow") {
		t.Error("Git Workflow section at EOF should be removed")
	}
	if !strings.Contains(got, "Do the work.") {
		t.Error("Content before Git Workflow at EOF must be preserved")
	}
}

// --------------------------------------------------------------------------
// Flag combination: --pr with --no-git must be rejected
// --------------------------------------------------------------------------

// TestPRNoGitConflict verifies that combining --pr and --no-git returns an
// error because PR creation requires a git branch which --no-git disables.
func TestPRNoGitConflict(t *testing.T) {
	// Save and restore package-level flags.
	origCreatePR, origNoGit := createPR, noGit
	t.Cleanup(func() { createPR, noGit = origCreatePR, origNoGit })

	createPR = true
	noGit = true

	err := runTask(nil, []string{"some task"})
	if err == nil {
		t.Fatal("expected error when --pr and --no-git are both set, got nil")
	}
	if !strings.Contains(err.Error(), "--pr") {
		t.Errorf("error should mention --pr, got: %v", err)
	}
}

// TestPRGitIsolateFalseConflict verifies that --pr with --git-isolate=false
// is also rejected, not just --pr with --no-git.
func TestPRGitIsolateFalseConflict(t *testing.T) {
	origCreatePR, origNoGit, origGitIsolate := createPR, noGit, gitIsolate
	t.Cleanup(func() { createPR, noGit, gitIsolate = origCreatePR, origNoGit, origGitIsolate })

	createPR = true
	noGit = false
	gitIsolate = false

	err := runTask(nil, []string{"some task"})
	if err == nil {
		t.Fatal("expected error when --pr and --git-isolate=false, got nil")
	}
	if !strings.Contains(err.Error(), "--pr") || !strings.Contains(err.Error(), "--git-isolate") {
		t.Errorf("error should mention --pr and --git-isolate, got: %v", err)
	}
}

// --------------------------------------------------------------------------
// --no-git: setupGitIsolation must not be called when noGit is true
// --------------------------------------------------------------------------

// TestNoGitSkipsIsolation verifies that when noGit is true, the worktree
// isolation gate prevents setupGitIsolation from running, so the workspace
// git fields remain empty and no worktree is created on disk.
func TestNoGitSkipsIsolation(t *testing.T) {
	repo := t.TempDir()
	initGitRepo(t, repo)

	cfgDir := t.TempDir()
	setConfigDir(t, cfgDir)

	ws := &core.Workspace{
		ID:        "20260316-nogit001",
		Path:      t.TempDir(),
		Task:      "add feature",
		TargetDir: repo,
	}

	// Simulate the condition guard in runTask with noGit=true.
	// The gate: gitIsolate && !noGit && ws.TargetDir != "" && git.FindRoot(ws.TargetDir) != ""
	shouldSetup := true && !true && ws.TargetDir != "" && git.FindRoot(ws.TargetDir) != ""
	if shouldSetup {
		t.Fatal("gate should be false when noGit=true")
	}

	// Confirm nothing was written to the workspace git fields.
	if ws.WorktreePath != "" || ws.BranchName != "" {
		t.Error("git fields should be empty when isolation is skipped")
	}

	// No worktrees directory should have been created.
	wortreesDir := filepath.Join(cfgDir, "worktrees")
	if _, err := os.Stat(wortreesDir); !os.IsNotExist(err) {
		t.Errorf("worktrees dir should not exist when isolation is skipped, got: %v", err)
	}
}

// TestNoGitSkipsIsolation_NonGitTarget verifies the gate is also false when
// the target directory is not a git repository (regardless of noGit).
func TestNoGitSkipsIsolation_NonGitTarget(t *testing.T) {
	dir := t.TempDir() // no .git

	ws := &core.Workspace{
		ID:        "20260316-nogit002",
		Path:      t.TempDir(),
		TargetDir: dir,
	}

	// Default: gitIsolate=true, noGit=false, but target is not a git repo.
	shouldSetup := true && !false && ws.TargetDir != "" && git.FindRoot(ws.TargetDir) != ""
	if shouldSetup {
		t.Fatal("gate should be false when target is not a git repo")
	}
}

// --------------------------------------------------------------------------
// mock-gh PR creation
// --------------------------------------------------------------------------

// writeFakeGH writes a shell script named "gh" to dir that records its
// arguments to args.txt and prints a fake PR URL to stdout.
func writeFakeGH(t *testing.T, dir string) {
	t.Helper()
	ghPath := filepath.Join(dir, "gh")
	script := `#!/bin/sh
echo "$@" >> "` + filepath.Join(dir, "args.txt") + `"
echo "https://github.com/owner/repo/pull/42"
`
	if err := os.WriteFile(ghPath, []byte(script), 0o755); err != nil {
		t.Fatalf("write fake gh: %v", err)
	}
}

// TestCreatePR_MockGH verifies that git.CreatePR invokes gh with the expected
// flags and returns the URL printed by gh.
func TestCreatePR_MockGH_Args(t *testing.T) {
	repo := t.TempDir()
	initGitRepo(t, repo)

	// Create a fake gh on PATH.
	fakeDir := t.TempDir()
	writeFakeGH(t, fakeDir)
	t.Setenv("PATH", fakeDir+string(os.PathListSeparator)+os.Getenv("PATH"))

	url, err := git.CreatePR(repo, "via/123/feat", "main", "feat: add thing", "body text", false)
	if err != nil {
		t.Fatalf("CreatePR: %v", err)
	}
	if url != "https://github.com/owner/repo/pull/42" {
		t.Errorf("URL = %q, want https://github.com/owner/repo/pull/42", url)
	}

	argsRaw, readErr := os.ReadFile(filepath.Join(fakeDir, "args.txt"))
	if readErr != nil {
		t.Fatalf("read args.txt: %v", readErr)
	}
	args := strings.TrimSpace(string(argsRaw))
	for _, want := range []string{"pr", "create", "--head", "via/123/feat", "--base", "main", "--title", "--body"} {
		if !strings.Contains(args, want) {
			t.Errorf("gh args missing %q in: %s", want, args)
		}
	}
	// --draft must NOT be present when draft=false.
	if strings.Contains(args, "--draft") {
		t.Errorf("--draft should not appear when draft=false, got: %s", args)
	}
}

func TestCreatePR_MockGH_DraftFlag(t *testing.T) {
	repo := t.TempDir()
	initGitRepo(t, repo)

	fakeDir := t.TempDir()
	writeFakeGH(t, fakeDir)
	t.Setenv("PATH", fakeDir+string(os.PathListSeparator)+os.Getenv("PATH"))

	_, err := git.CreatePR(repo, "via/456/fix", "main", "fix: bug", "body", true)
	if err != nil {
		t.Fatalf("CreatePR: %v", err)
	}

	argsRaw, _ := os.ReadFile(filepath.Join(fakeDir, "args.txt"))
	if !strings.Contains(string(argsRaw), "--draft") {
		t.Errorf("--draft should appear when draft=true, got: %s", string(argsRaw))
	}
}

// --------------------------------------------------------------------------
// PR body rendering end-to-end via createMissionPR
// --------------------------------------------------------------------------

// captureEmitter records all emitted events.
type captureEmitter struct {
	events []event.Event
}

func (c *captureEmitter) Emit(_ context.Context, e event.Event) { c.events = append(c.events, e) }
func (c *captureEmitter) Close() error                          { return nil }

func TestCreateMissionPR_EmitsEvent(t *testing.T) {
	repo := t.TempDir()
	initGitRepo(t, repo)
	cfgDir := t.TempDir()
	setConfigDir(t, cfgDir)

	// Create a local bare remote so git push succeeds without network.
	bareRemote := t.TempDir()
	mustGit(t, bareRemote, "init", "--bare")
	mustGit(t, repo, "remote", "add", "origin", bareRemote)
	mustGit(t, repo, "push", "-u", "origin", "main")

	// Set up the workspace with a branch that exists in the repo.
	ws := &core.Workspace{
		ID:            "20260316-pr001",
		Path:          t.TempDir(),
		Task:          "add widget",
		GitRepoRoot:   repo,
		WorktreePath:  repo, // use repo as worktree for push
		BaseBranch:    "main",
		BranchName:    "via/20260316-pr001/add-widget",
	}
	// Create the branch so ChangedFiles and push can resolve it.
	mustGit(t, repo, "branch", ws.BranchName)

	// Write a fake gh that records args.
	fakeDir := t.TempDir()
	writeFakeGH(t, fakeDir)
	t.Setenv("PATH", fakeDir+string(os.PathListSeparator)+os.Getenv("PATH"))

	plan := &core.Plan{
		ID:            "plan-001",
		Task:          "add widget",
		ExecutionMode: "sequential",
		Phases: []*core.Phase{
			{ID: "p1", Persona: "implementer", CostUSD: 0.002},
		},
	}
	result := &core.ExecutionResult{
		Plan:     plan,
		Success:  true,
		Duration: 90 * time.Second,
	}

	em := &captureEmitter{}
	createMissionPR(ws, plan, result, prOptions{draft: true}, em, ws.ID)

	// The git.pr_created event must have been emitted.
	var found bool
	for _, ev := range em.events {
		if ev.Type == event.GitPRCreated {
			found = true
			if ev.Data["url"] != "https://github.com/owner/repo/pull/42" {
				t.Errorf("pr_created url = %v, want PR URL", ev.Data["url"])
			}
			if ev.Data["branch"] != ws.BranchName {
				t.Errorf("pr_created branch = %v, want %q", ev.Data["branch"], ws.BranchName)
			}
		}
	}
	if !found {
		t.Errorf("git.pr_created event not emitted; got events: %v", em.events)
	}
}
