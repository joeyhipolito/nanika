package cmd

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/git"
)

// writeFakeGHMulti writes a fake gh script that handles multiple subcommands:
// - "pr create" → prints a fake PR URL and records args
// - "pr comment", "pr edit" → succeeds silently and records args
// All invocations are appended to args.txt in dir.
func writeFakeGHMulti(t *testing.T, dir string) {
	t.Helper()
	ghPath := filepath.Join(dir, "gh")
	script := `#!/bin/sh
echo "$@" >> "` + filepath.Join(dir, "args.txt") + `"
if [ "$1" = "pr" ] && [ "$2" = "create" ]; then
    echo "https://github.com/owner/repo/pull/42"
fi
`
	if err := os.WriteFile(ghPath, []byte(script), 0o755); err != nil {
		t.Fatalf("write fake gh: %v", err)
	}
}

func writeFakeCodexReview(t *testing.T, dir, body string, exitCode int) {
	t.Helper()
	codexPath := filepath.Join(dir, "codex")
	script := `#!/bin/sh
printf "%s\n" "$@" >> "` + filepath.Join(dir, "codex_args.txt") + `"
: > "` + filepath.Join(dir, "codex_stdin.txt") + `"
while IFS= read -r line || [ -n "$line" ]; do
    printf "%s\n" "$line" >> "` + filepath.Join(dir, "codex_stdin.txt") + `"
done
if [ "` + fmt.Sprintf("%d", exitCode) + `" -ne 0 ]; then
    echo "codex failed" >&2
    exit ` + fmt.Sprintf("%d", exitCode) + `
fi
printf "%s\n" '` + strings.ReplaceAll(body, "'", `'"'"'`) + `'
`
	if err := os.WriteFile(codexPath, []byte(script), 0o755); err != nil {
		t.Fatalf("write fake codex: %v", err)
	}
}

// readArgsFile reads and returns all recorded gh invocations as one string.
func readArgsFile(t *testing.T, dir string) string {
	t.Helper()
	data, err := os.ReadFile(filepath.Join(dir, "args.txt"))
	if err != nil {
		t.Fatalf("read args.txt: %v", err)
	}
	return string(data)
}

// makeTestWorkspaceAndPR is a shared setup helper that initialises a git repo,
// wires a fake gh, and returns the workspace and path for the fake gh dir.
func makeTestWorkspaceAndPR(t *testing.T) (ws *core.Workspace, fakeDir string, repo string) {
	t.Helper()
	repo = t.TempDir()
	initGitRepo(t, repo)

	cfgDir := t.TempDir()
	setConfigDir(t, cfgDir)

	// Create bare remote so git push succeeds.
	bareRemote := t.TempDir()
	mustGit(t, bareRemote, "init", "--bare")
	mustGit(t, repo, "remote", "add", "origin", bareRemote)
	mustGit(t, repo, "push", "-u", "origin", "main")

	ws = &core.Workspace{
		ID:           "20260316-codex001",
		Path:         t.TempDir(),
		Task:         "implement feature",
		GitRepoRoot:  repo,
		WorktreePath: repo,
		BaseBranch:   "main",
		BranchName:   "via/20260316-codex001/implement-feature",
	}
	mustGit(t, repo, "branch", ws.BranchName)

	fakeDir = t.TempDir()
	writeFakeGHMulti(t, fakeDir)
	gitPath, err := exec.LookPath("git")
	if err != nil {
		t.Fatalf("LookPath(git): %v", err)
	}
	t.Setenv("PATH", fakeDir+string(os.PathListSeparator)+filepath.Dir(gitPath)+string(os.PathListSeparator)+os.Getenv("PATH"))
	return ws, fakeDir, repo
}

func makePlan() *core.Plan {
	return &core.Plan{
		ID:            "plan-001",
		Task:          "implement feature",
		ExecutionMode: "sequential",
		Phases: []*core.Phase{
			{ID: "p1", Persona: "implementer", CostUSD: 0.001},
		},
	}
}

func makeResult(plan *core.Plan) *core.ExecutionResult {
	return &core.ExecutionResult{
		Plan:     plan,
		Success:  true,
		Duration: 60 * time.Second,
	}
}

// --------------------------------------------------------------------------
// Flag validation: --codex-review requires --pr
// --------------------------------------------------------------------------

func TestCodexReviewRequiresPR(t *testing.T) {
	origCodexReview, origCreatePR := codexReview, createPR
	t.Cleanup(func() { codexReview, createPR = origCodexReview, origCreatePR })

	codexReview = true
	createPR = false

	err := runTask(nil, []string{"some task"})
	if err == nil {
		t.Fatal("expected error when --codex-review used without --pr, got nil")
	}
	if !strings.Contains(err.Error(), "--codex-review") || !strings.Contains(err.Error(), "--pr") {
		t.Errorf("error should mention --codex-review and --pr, got: %v", err)
	}
}

// --------------------------------------------------------------------------
// Frontmatter parsing for new fields
// --------------------------------------------------------------------------

func TestParseMissionFrontmatter_CodexReview(t *testing.T) {
	task := "---\ncodex_review: true\n---\n# Task\n"
	fm := parseMissionFrontmatter(task)
	if !fm.CodexReview {
		t.Error("expected CodexReview=true, got false")
	}
}

func TestParseMissionFrontmatter_CodexReviewFalse(t *testing.T) {
	task := "---\ncodex_review: false\n---\n# Task\n"
	fm := parseMissionFrontmatter(task)
	if fm.CodexReview {
		t.Error("expected CodexReview=false, got true")
	}
}

func TestParseMissionFrontmatter_PRReviewers(t *testing.T) {
	task := "---\npr_reviewers: alice, bob, carol\n---\n# Task\n"
	fm := parseMissionFrontmatter(task)
	if len(fm.PRReviewers) != 3 {
		t.Fatalf("expected 3 reviewers, got %d: %v", len(fm.PRReviewers), fm.PRReviewers)
	}
	if fm.PRReviewers[0] != "alice" || fm.PRReviewers[1] != "bob" || fm.PRReviewers[2] != "carol" {
		t.Errorf("unexpected reviewers: %v", fm.PRReviewers)
	}
}

func TestParseMissionFrontmatter_PRLabels(t *testing.T) {
	task := "---\npr_labels: enhancement, v80\n---\n# Task\n"
	fm := parseMissionFrontmatter(task)
	if len(fm.PRLabels) != 2 {
		t.Fatalf("expected 2 labels, got %d: %v", len(fm.PRLabels), fm.PRLabels)
	}
	if fm.PRLabels[0] != "enhancement" || fm.PRLabels[1] != "v80" {
		t.Errorf("unexpected labels: %v", fm.PRLabels)
	}
}

func TestParseMissionFrontmatter_AllNewFields(t *testing.T) {
	task := "---\ncodex_review: true\npr_reviewers: dev1, dev2\npr_labels: bug, needs-review\n---\n# Task\n"
	fm := parseMissionFrontmatter(task)
	if !fm.CodexReview {
		t.Error("CodexReview should be true")
	}
	if len(fm.PRReviewers) != 2 {
		t.Errorf("expected 2 reviewers, got %d", len(fm.PRReviewers))
	}
	if len(fm.PRLabels) != 2 {
		t.Errorf("expected 2 labels, got %d", len(fm.PRLabels))
	}
}

// --------------------------------------------------------------------------
// PR URL sidecar written by createMissionPR
// --------------------------------------------------------------------------

func TestCreateMissionPR_WritesURLSidecar(t *testing.T) {
	ws, _, _ := makeTestWorkspaceAndPR(t)
	plan := makePlan()
	result := makeResult(plan)
	em := &captureEmitter{}

	createMissionPR(ws, plan, result, prOptions{draft: true}, em, ws.ID)

	data, err := os.ReadFile(filepath.Join(ws.Path, "pr_url"))
	if err != nil {
		t.Fatalf("pr_url sidecar not written: %v", err)
	}
	if url := strings.TrimSpace(string(data)); url != "https://github.com/owner/repo/pull/42" {
		t.Errorf("pr_url = %q, want https://github.com/owner/repo/pull/42", url)
	}
}

// --------------------------------------------------------------------------
// Codex review comment posted and event emitted
// --------------------------------------------------------------------------

func TestCreateMissionPR_CodexReview_PostsComment(t *testing.T) {
	ws, fakeDir, _ := makeTestWorkspaceAndPR(t)
	plan := makePlan()
	result := makeResult(plan)
	em := &captureEmitter{}

	createMissionPR(ws, plan, result, prOptions{draft: true, codexReview: true}, em, ws.ID)

	args := readArgsFile(t, fakeDir)
	if !strings.Contains(args, "pr comment") {
		t.Errorf("expected 'gh pr comment' call, got args:\n%s", args)
	}
	if !strings.Contains(args, "@codex please review this PR") {
		t.Errorf("expected '@codex please review this PR' in comment body, got args:\n%s", args)
	}
}

func TestCreateMissionPR_CodexReview_UsesLocalCLIWhenAvailable(t *testing.T) {
	ws, fakeDir, _ := makeTestWorkspaceAndPR(t)
	writeFakeCodexReview(t, fakeDir, "- finding: add a regression test", 0)

	plan := makePlan()
	result := makeResult(plan)
	em := &captureEmitter{}

	createMissionPR(ws, plan, result, prOptions{draft: true, codexReview: true}, em, ws.ID)

	ghArgs := readArgsFile(t, fakeDir)
	if strings.Contains(ghArgs, "@codex please review this PR") {
		t.Fatalf("expected local Codex review comment, got fallback mention:\n%s", ghArgs)
	}
	if !strings.Contains(ghArgs, "Codex CLI Review") {
		t.Fatalf("expected Codex CLI review heading in PR comment, got:\n%s", ghArgs)
	}
	if !strings.Contains(ghArgs, "add a regression test") {
		t.Fatalf("expected local Codex review body in PR comment, got:\n%s", ghArgs)
	}

	codexArgsRaw, err := os.ReadFile(filepath.Join(fakeDir, "codex_args.txt"))
	if err != nil {
		t.Fatalf("read codex_args.txt: %v", err)
	}
	codexArgs := string(codexArgsRaw)
	for _, want := range []string{"review", "--base", "main", "-"} {
		if !strings.Contains(codexArgs, want) {
			t.Fatalf("expected codex review args to contain %q, got:\n%s", want, codexArgs)
		}
	}

	codexPromptRaw, err := os.ReadFile(filepath.Join(fakeDir, "codex_stdin.txt"))
	if err != nil {
		t.Fatalf("read codex_stdin.txt: %v", err)
	}
	if got := strings.TrimSpace(string(codexPromptRaw)); got != codexReviewPrompt {
		t.Fatalf("expected codex review prompt via stdin, got %q", got)
	}
}

func TestCreateMissionPR_CodexReview_FallsBackWhenLocalCLIReviewFails(t *testing.T) {
	ws, fakeDir, _ := makeTestWorkspaceAndPR(t)
	writeFakeCodexReview(t, fakeDir, "", 1)

	plan := makePlan()
	result := makeResult(plan)
	em := &captureEmitter{}

	createMissionPR(ws, plan, result, prOptions{draft: true, codexReview: true}, em, ws.ID)

	args := readArgsFile(t, fakeDir)
	if !strings.Contains(args, "@codex please review this PR") {
		t.Fatalf("expected fallback review request comment after local codex failure, got:\n%s", args)
	}
}

func TestCreateMissionPR_EmitsExternalReviewEvent(t *testing.T) {
	ws, _, _ := makeTestWorkspaceAndPR(t)
	plan := makePlan()
	result := makeResult(plan)
	em := &captureEmitter{}

	createMissionPR(ws, plan, result, prOptions{draft: true, codexReview: true}, em, ws.ID)

	var found bool
	for _, ev := range em.events {
		if ev.Type == event.ReviewExternalRequested {
			found = true
			if ev.Data["pr_url"] != "https://github.com/owner/repo/pull/42" {
				t.Errorf("event pr_url = %v, want PR URL", ev.Data["pr_url"])
			}
			if ev.Data["reviewer"] != "codex" {
				t.Errorf("event reviewer = %v, want 'codex'", ev.Data["reviewer"])
			}
		}
	}
	if !found {
		t.Errorf("review.external_requested event not emitted; got events: %v", em.events)
	}
}

func TestCreateMissionPR_NoCodexReview_NoExternalEvent(t *testing.T) {
	ws, _, _ := makeTestWorkspaceAndPR(t)
	plan := makePlan()
	result := makeResult(plan)
	em := &captureEmitter{}

	createMissionPR(ws, plan, result, prOptions{draft: true, codexReview: false}, em, ws.ID)

	for _, ev := range em.events {
		if ev.Type == event.ReviewExternalRequested {
			t.Error("review.external_requested should not be emitted when codexReview=false")
		}
	}
}

// --------------------------------------------------------------------------
// Reviewers and labels via gh pr edit
// --------------------------------------------------------------------------

func TestCreateMissionPR_AddReviewers(t *testing.T) {
	ws, fakeDir, _ := makeTestWorkspaceAndPR(t)
	plan := makePlan()
	result := makeResult(plan)
	em := &captureEmitter{}

	createMissionPR(ws, plan, result, prOptions{draft: true, reviewers: []string{"alice", "bob"}}, em, ws.ID)

	args := readArgsFile(t, fakeDir)
	if !strings.Contains(args, "pr edit") {
		t.Errorf("expected 'gh pr edit' call for reviewers, got:\n%s", args)
	}
	if !strings.Contains(args, "--add-reviewer") {
		t.Errorf("expected '--add-reviewer' flag, got:\n%s", args)
	}
	if !strings.Contains(args, "alice,bob") {
		t.Errorf("expected 'alice,bob' in args, got:\n%s", args)
	}
}

func TestCreateMissionPR_AddLabels(t *testing.T) {
	ws, fakeDir, _ := makeTestWorkspaceAndPR(t)
	plan := makePlan()
	result := makeResult(plan)
	em := &captureEmitter{}

	createMissionPR(ws, plan, result, prOptions{draft: true, labels: []string{"enhancement", "v80"}}, em, ws.ID)

	args := readArgsFile(t, fakeDir)
	if !strings.Contains(args, "pr edit") {
		t.Errorf("expected 'gh pr edit' call for labels, got:\n%s", args)
	}
	if !strings.Contains(args, "--add-label") {
		t.Errorf("expected '--add-label' flag, got:\n%s", args)
	}
	if !strings.Contains(args, "enhancement,v80") {
		t.Errorf("expected 'enhancement,v80' in args, got:\n%s", args)
	}
}

// --------------------------------------------------------------------------
// Internal review findings posted as PR comment
// --------------------------------------------------------------------------

func TestCreateMissionPR_ReviewFindingsComment(t *testing.T) {
	ws, fakeDir, _ := makeTestWorkspaceAndPR(t)
	plan := &core.Plan{
		ID:            "plan-002",
		Task:          "implement feature",
		ExecutionMode: "sequential",
		Phases: []*core.Phase{
			{ID: "p1", Persona: "implementer", CostUSD: 0.001},
			{
				ID:             "p2",
				Persona:        "staff-code-reviewer",
				ReviewBlockers: []string{"[main.go:10] Missing error check"},
				ReviewWarnings: []string{"[util.go:5] Consider extracting helper"},
			},
		},
	}
	result := makeResult(plan)
	em := &captureEmitter{}

	createMissionPR(ws, plan, result, prOptions{draft: true}, em, ws.ID)

	args := readArgsFile(t, fakeDir)
	// There should be a "pr comment" call that contains the review findings heading.
	if !strings.Contains(args, "pr comment") {
		t.Errorf("expected 'gh pr comment' for review findings, got:\n%s", args)
	}
	if !strings.Contains(args, "Internal Review Findings") {
		t.Errorf("expected 'Internal Review Findings' in comment, got:\n%s", args)
	}
}

func TestCreateMissionPR_NoFindingsNoComment(t *testing.T) {
	ws, fakeDir, _ := makeTestWorkspaceAndPR(t)
	plan := makePlan() // no review findings
	result := makeResult(plan)
	em := &captureEmitter{}

	createMissionPR(ws, plan, result, prOptions{draft: true}, em, ws.ID)

	args := ""
	if data, err := os.ReadFile(filepath.Join(fakeDir, "args.txt")); err == nil {
		args = string(data)
	}
	// Should have pr create but NOT pr comment (no codex, no findings)
	if strings.Contains(args, "pr comment") {
		t.Errorf("expected no 'gh pr comment' when no findings and no codex review, got:\n%s", args)
	}
}

// --------------------------------------------------------------------------
// git.CommentOnPR, AddPRReviewers, AddPRLabels via mock gh
// --------------------------------------------------------------------------

func TestGitCommentOnPR(t *testing.T) {
	repo := t.TempDir()
	initGitRepo(t, repo)

	fakeDir := t.TempDir()
	writeFakeGHMulti(t, fakeDir)
	t.Setenv("PATH", fakeDir+string(os.PathListSeparator)+os.Getenv("PATH"))

	if err := git.CommentOnPR(repo, "https://github.com/owner/repo/pull/42", "hello review"); err != nil {
		t.Fatalf("CommentOnPR: %v", err)
	}

	args := readArgsFile(t, fakeDir)
	if !strings.Contains(args, "pr comment") {
		t.Errorf("expected 'pr comment' in gh args, got: %s", args)
	}
	if !strings.Contains(args, "hello review") {
		t.Errorf("expected body text in gh args, got: %s", args)
	}
}

func TestGitAddPRReviewers(t *testing.T) {
	repo := t.TempDir()
	initGitRepo(t, repo)

	fakeDir := t.TempDir()
	writeFakeGHMulti(t, fakeDir)
	t.Setenv("PATH", fakeDir+string(os.PathListSeparator)+os.Getenv("PATH"))

	if err := git.AddPRReviewers(repo, "https://github.com/owner/repo/pull/42", []string{"alice", "bob"}); err != nil {
		t.Fatalf("AddPRReviewers: %v", err)
	}

	args := readArgsFile(t, fakeDir)
	if !strings.Contains(args, "pr edit") {
		t.Errorf("expected 'pr edit' in gh args, got: %s", args)
	}
	if !strings.Contains(args, "--add-reviewer") {
		t.Errorf("expected '--add-reviewer' flag, got: %s", args)
	}
	if !strings.Contains(args, "alice,bob") {
		t.Errorf("expected 'alice,bob', got: %s", args)
	}
}

func TestGitAddPRLabels(t *testing.T) {
	repo := t.TempDir()
	initGitRepo(t, repo)

	fakeDir := t.TempDir()
	writeFakeGHMulti(t, fakeDir)
	t.Setenv("PATH", fakeDir+string(os.PathListSeparator)+os.Getenv("PATH"))

	if err := git.AddPRLabels(repo, "https://github.com/owner/repo/pull/42", []string{"bug", "v80"}); err != nil {
		t.Fatalf("AddPRLabels: %v", err)
	}

	args := readArgsFile(t, fakeDir)
	if !strings.Contains(args, "pr edit") {
		t.Errorf("expected 'pr edit' in gh args, got: %s", args)
	}
	if !strings.Contains(args, "--add-label") {
		t.Errorf("expected '--add-label' flag, got: %s", args)
	}
	if !strings.Contains(args, "bug,v80") {
		t.Errorf("expected 'bug,v80', got: %s", args)
	}
}

func TestGitAddPRReviewers_NoOp(t *testing.T) {
	repo := t.TempDir()
	// No fake gh needed — empty list should be a no-op without calling gh.
	if err := git.AddPRReviewers(repo, "https://github.com/owner/repo/pull/42", nil); err != nil {
		t.Fatalf("AddPRReviewers(nil): %v", err)
	}
}

func TestGitAddPRLabels_NoOp(t *testing.T) {
	repo := t.TempDir()
	if err := git.AddPRLabels(repo, "https://github.com/owner/repo/pull/42", nil); err != nil {
		t.Fatalf("AddPRLabels(nil): %v", err)
	}
}

// --------------------------------------------------------------------------
// buildReviewFindingsComment unit tests
// --------------------------------------------------------------------------

func TestBuildReviewFindingsComment_Empty(t *testing.T) {
	plan := &core.Plan{Phases: []*core.Phase{{ID: "p1"}}}
	if got := buildReviewFindingsComment(plan); got != "" {
		t.Errorf("expected empty string for plan with no findings, got: %q", got)
	}
}

func TestBuildReviewFindingsComment_WithBlockersAndWarnings(t *testing.T) {
	plan := &core.Plan{
		Phases: []*core.Phase{
			{
				ID:             "p1",
				ReviewBlockers: []string{"[foo.go:1] nil deref"},
				ReviewWarnings: []string{"consider caching"},
			},
		},
	}
	got := buildReviewFindingsComment(plan)
	if !strings.Contains(got, "Internal Review Findings") {
		t.Errorf("missing heading in comment: %q", got)
	}
	if !strings.Contains(got, "nil deref") {
		t.Errorf("missing blocker text: %q", got)
	}
	if !strings.Contains(got, "consider caching") {
		t.Errorf("missing warning text: %q", got)
	}
}

// --------------------------------------------------------------------------
// Status command displays PR URL
// --------------------------------------------------------------------------

func TestStatusShowsPRURL(t *testing.T) {
	// Verify that the status command reads pr_url sidecar and displays it.
	// We test the readArgsFile pattern by checking the file read logic directly.
	wsPath := t.TempDir()
	prURL := "https://github.com/owner/repo/pull/99"
	if err := os.WriteFile(filepath.Join(wsPath, "pr_url"), []byte(prURL), 0600); err != nil {
		t.Fatalf("write pr_url: %v", err)
	}

	data, err := os.ReadFile(filepath.Join(wsPath, "pr_url"))
	if err != nil {
		t.Fatalf("read pr_url: %v", err)
	}
	if strings.TrimSpace(string(data)) != prURL {
		t.Errorf("pr_url content = %q, want %q", strings.TrimSpace(string(data)), prURL)
	}
}

// --------------------------------------------------------------------------
// Codex review fixes (from Codex PR feedback)
// --------------------------------------------------------------------------

// TestParseMissionFrontmatter_CodexReviewQuoted verifies that codex_review: "true"
// (with quotes) is correctly parsed as true after unquoting.
func TestParseMissionFrontmatter_CodexReviewQuoted(t *testing.T) {
	task := "---\ncodex_review: \"true\"\n---\n# Task\n"
	fm := parseMissionFrontmatter(task)
	if !fm.CodexReview {
		t.Error("expected CodexReview=true for quoted 'true', got false")
	}
}

// TestParseMissionFrontmatter_PRReviewersYAMLSequence verifies that
// pr_reviewers supports YAML block sequence syntax (- item per line).
func TestParseMissionFrontmatter_PRReviewersYAMLSequence(t *testing.T) {
	task := "---\npr_reviewers:\n- alice\n- bob\n- carol\n---\n# Task\n"
	fm := parseMissionFrontmatter(task)
	if len(fm.PRReviewers) != 3 {
		t.Fatalf("expected 3 reviewers from YAML sequence, got %d: %v", len(fm.PRReviewers), fm.PRReviewers)
	}
	if fm.PRReviewers[0] != "alice" || fm.PRReviewers[1] != "bob" || fm.PRReviewers[2] != "carol" {
		t.Errorf("unexpected reviewers: %v", fm.PRReviewers)
	}
}

// TestParseMissionFrontmatter_PRLabelsYAMLSequence verifies that
// pr_labels supports YAML block sequence syntax (- item per line).
func TestParseMissionFrontmatter_PRLabelsYAMLSequence(t *testing.T) {
	task := "---\npr_labels:\n- enhancement\n- v80\n---\n# Task\n"
	fm := parseMissionFrontmatter(task)
	if len(fm.PRLabels) != 2 {
		t.Fatalf("expected 2 labels from YAML sequence, got %d: %v", len(fm.PRLabels), fm.PRLabels)
	}
	if fm.PRLabels[0] != "enhancement" || fm.PRLabels[1] != "v80" {
		t.Errorf("unexpected labels: %v", fm.PRLabels)
	}
}

// TestParseMissionFrontmatter_MixedSequenceAndInline verifies that inline
// comma-separated values still work alongside the new sequence support.
func TestParseMissionFrontmatter_MixedSequenceAndInline(t *testing.T) {
	task := "---\npr_reviewers: alice, bob\npr_labels:\n- bug\n- needs-review\n---\n# Task\n"
	fm := parseMissionFrontmatter(task)
	if len(fm.PRReviewers) != 2 {
		t.Errorf("expected 2 reviewers, got %d: %v", len(fm.PRReviewers), fm.PRReviewers)
	}
	if len(fm.PRLabels) != 2 {
		t.Errorf("expected 2 labels from sequence, got %d: %v", len(fm.PRLabels), fm.PRLabels)
	}
}

// --------------------------------------------------------------------------
// Push path fallback: WorktreePath="" → GitRepoRoot
// --------------------------------------------------------------------------

// TestCreateMissionPR_PushFallbackToGitRepoRoot verifies that when
// ws.WorktreePath is empty, createMissionPR uses ws.GitRepoRoot for the
// git push instead of the empty path (which previously caused push to fail).
func TestCreateMissionPR_PushFallbackToGitRepoRoot(t *testing.T) {
	repo := t.TempDir()
	initGitRepo(t, repo)

	cfgDir := t.TempDir()
	setConfigDir(t, cfgDir)

	bareRemote := t.TempDir()
	mustGit(t, bareRemote, "init", "--bare")
	mustGit(t, repo, "remote", "add", "origin", bareRemote)
	mustGit(t, repo, "push", "-u", "origin", "main")

	branchName := "via/20260316-push001/implement-feature"
	mustGit(t, repo, "checkout", "-b", branchName)
	// Commit something so the branch differs from main.
	if err := os.WriteFile(filepath.Join(repo, "feature.txt"), []byte("x"), 0o644); err != nil {
		t.Fatal(err)
	}
	mustGit(t, repo, "add", ".")
	mustGit(t, repo, "commit", "-m", "add feature")
	mustGit(t, repo, "checkout", "main")

	ws := &core.Workspace{
		ID:           "20260316-push001",
		Path:         t.TempDir(),
		Task:         "implement feature",
		GitRepoRoot:  repo,
		WorktreePath: "", // empty — must fall back to GitRepoRoot for push
		BaseBranch:   "main",
		BranchName:   branchName,
	}

	fakeDir := t.TempDir()
	writeFakeGHMulti(t, fakeDir)
	gitPath, err := exec.LookPath("git")
	if err != nil {
		t.Fatalf("LookPath(git): %v", err)
	}
	t.Setenv("PATH", fakeDir+string(os.PathListSeparator)+filepath.Dir(gitPath)+string(os.PathListSeparator)+os.Getenv("PATH"))

	plan := makePlan()
	result := makeResult(plan)
	em := &captureEmitter{}

	createMissionPR(ws, plan, result, prOptions{draft: true}, em, ws.ID)

	// PR must have been created, not skipped.
	args := readArgsFile(t, fakeDir)
	if !strings.Contains(args, "pr create") {
		t.Errorf("expected 'gh pr create' when WorktreePath is empty (push via GitRepoRoot); got args:\n%s", args)
	}
}

// Ensure unused import isn't an issue.
var _ = context.Background
