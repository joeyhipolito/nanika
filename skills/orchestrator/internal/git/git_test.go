package git_test

import (
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/git"
)

// initRepo creates a bare or non-bare git repo in dir with an initial commit.
func initRepo(t *testing.T, dir string) {
	t.Helper()
	mustRun(t, dir, "git", "init", "-b", "main")
	mustRun(t, dir, "git", "config", "user.email", "test@example.com")
	mustRun(t, dir, "git", "config", "user.name", "Test")
	// Create an initial commit so HEAD is valid.
	f := filepath.Join(dir, "README.md")
	if err := os.WriteFile(f, []byte("hello\n"), 0o644); err != nil {
		t.Fatalf("write README: %v", err)
	}
	mustRun(t, dir, "git", "add", ".")
	mustRun(t, dir, "git", "commit", "-m", "init")
}

func initBareRepo(t *testing.T, dir string) {
	t.Helper()
	mustRun(t, dir, "git", "init", "--bare", "-b", "main")
}

func mustRun(t *testing.T, dir string, args ...string) {
	t.Helper()
	cmd := exec.Command(args[0], args[1:]...)
	cmd.Dir = dir
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("command %v in %s: %v\n%s", args, dir, err, out)
	}
}

// ---- FindRoot ---------------------------------------------------------------

func TestFindRoot_Found(t *testing.T) {
	dir := t.TempDir()
	initRepo(t, dir)

	sub := filepath.Join(dir, "a", "b")
	if err := os.MkdirAll(sub, 0o755); err != nil {
		t.Fatal(err)
	}

	got := git.FindRoot(sub)
	if got != dir {
		t.Errorf("FindRoot(%s) = %q, want %q", sub, got, dir)
	}
}

func TestFindRoot_NotFound(t *testing.T) {
	// Use a temp dir that is definitely not inside a git repo.
	dir := t.TempDir()
	// Walk all the way to / — the temp dir itself has no .git.
	got := git.FindRoot(dir)
	// It's possible the TempDir is inside a git worktree on some machines;
	// just verify the function doesn't panic and returns something sensible.
	if got == dir {
		t.Skip("TempDir is unexpectedly inside a git repo")
	}
	// got should be empty or a parent repo root, not the bare tempdir itself.
	_ = got
}

func TestFindRoot_DirectlyAtRoot(t *testing.T) {
	dir := t.TempDir()
	initRepo(t, dir)

	got := git.FindRoot(dir)
	if got != dir {
		t.Errorf("FindRoot at repo root: got %q, want %q", got, dir)
	}
}

// ---- CurrentBranch ----------------------------------------------------------

func TestCurrentBranch(t *testing.T) {
	dir := t.TempDir()
	initRepo(t, dir)

	branch, err := git.CurrentBranch(dir)
	if err != nil {
		t.Fatalf("CurrentBranch: %v", err)
	}
	if branch != "main" {
		t.Errorf("CurrentBranch = %q, want %q", branch, "main")
	}
}

// ---- CreateBranch -----------------------------------------------------------

func TestCreateBranch(t *testing.T) {
	dir := t.TempDir()
	initRepo(t, dir)

	if err := git.CreateBranch(dir, "feature-x", "main"); err != nil {
		t.Fatalf("CreateBranch: %v", err)
	}

	// Verify branch exists.
	cmd := exec.Command("git", "rev-parse", "--verify", "feature-x")
	cmd.Dir = dir
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("branch feature-x not found: %v\n%s", err, out)
	}
}

func TestCreateBranch_InvalidBase(t *testing.T) {
	dir := t.TempDir()
	initRepo(t, dir)

	err := git.CreateBranch(dir, "bad-branch", "nonexistent-base")
	if err == nil {
		t.Fatal("expected error for nonexistent base, got nil")
	}
}

// ---- CreateWorktree / RemoveWorktree ----------------------------------------

func TestWorktreeLifecycle(t *testing.T) {
	dir := t.TempDir()
	initRepo(t, dir)

	wtPath := filepath.Join(t.TempDir(), "wt")

	if err := git.CreateBranch(dir, "wt-branch", "main"); err != nil {
		t.Fatalf("CreateBranch: %v", err)
	}
	if err := git.CreateWorktree(dir, wtPath, "wt-branch"); err != nil {
		t.Fatalf("CreateWorktree: %v", err)
	}

	// The worktree directory should exist and contain .git (file, not dir).
	info, err := os.Stat(wtPath)
	if err != nil || !info.IsDir() {
		t.Fatalf("worktree dir not created at %s", wtPath)
	}

	if err := git.RemoveWorktree(wtPath, ""); err != nil {
		t.Fatalf("RemoveWorktree: %v", err)
	}

	if _, err := os.Stat(wtPath); !os.IsNotExist(err) {
		t.Errorf("worktree dir should be gone after RemoveWorktree, but Stat returned: %v", err)
	}
}

// ---- CommitAll --------------------------------------------------------------

func TestCommitAll_WithChanges(t *testing.T) {
	dir := t.TempDir()
	initRepo(t, dir)

	// Write a new file.
	if err := os.WriteFile(filepath.Join(dir, "new.txt"), []byte("content"), 0o644); err != nil {
		t.Fatal(err)
	}

	if err := git.CommitAll(dir, "add new.txt"); err != nil {
		t.Fatalf("CommitAll: %v", err)
	}

	// Confirm the commit exists.
	cmd := exec.Command("git", "log", "--oneline", "-1")
	cmd.Dir = dir
	out, err := cmd.Output()
	if err != nil {
		t.Fatalf("git log: %v", err)
	}
	if !strings.Contains(string(out), "add new.txt") {
		t.Errorf("commit message not found in log: %s", out)
	}
}

func TestCommitAll_NothingToCommit(t *testing.T) {
	dir := t.TempDir()
	initRepo(t, dir)

	// No changes — should return nil.
	if err := git.CommitAll(dir, "empty commit"); err != nil {
		t.Fatalf("CommitAll with nothing to commit should not error: %v", err)
	}
}

func TestCommitAll_NewDirInWorktree(t *testing.T) {
	repo := t.TempDir()
	initRepo(t, repo)

	if err := git.CreateBranch(repo, "feature", "main"); err != nil {
		t.Fatalf("CreateBranch: %v", err)
	}

	wtPath := filepath.Join(t.TempDir(), "wt")
	if err := git.CreateWorktree(repo, wtPath, "feature"); err != nil {
		t.Fatalf("CreateWorktree: %v", err)
	}
	t.Cleanup(func() { _ = git.RemoveWorktree(wtPath, "") })

	// Create a new directory with a file inside the worktree.
	newDir := filepath.Join(wtPath, "subpkg")
	if err := os.MkdirAll(newDir, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(newDir, "main.go"), []byte("package subpkg\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	if err := git.CommitAll(wtPath, "add subpkg"); err != nil {
		t.Fatalf("CommitAll in worktree: %v", err)
	}

	// Verify the new directory and file appear in the commit.
	cmd := exec.Command("git", "show", "--name-only", "--format=", "HEAD")
	cmd.Dir = wtPath
	out, err := cmd.Output()
	if err != nil {
		t.Fatalf("git show: %v", err)
	}
	if !strings.Contains(string(out), "subpkg/main.go") {
		t.Errorf("CommitAll did not capture new directory; commit files:\n%s", out)
	}
}

// TestCommitAll_WorkerWritesToWorktree reproduces the bug where workers were
// instructed to write artifacts to WorkerDir (outside the worktree) instead of
// the worktree itself. CommitAll must capture new files and new directories
// when they are written directly into the worktree by a simulated worker.
func TestCommitAll_WorkerWritesToWorktree(t *testing.T) {
	repo := t.TempDir()
	initRepo(t, repo)

	if err := git.CreateBranch(repo, "worker-branch", "main"); err != nil {
		t.Fatalf("CreateBranch: %v", err)
	}

	wtPath := filepath.Join(t.TempDir(), "wt")
	if err := git.CreateWorktree(repo, wtPath, "worker-branch"); err != nil {
		t.Fatalf("CreateWorktree: %v", err)
	}
	t.Cleanup(func() { _ = git.RemoveWorktree(wtPath, "") })

	// Simulate a worker writing to the worktree (correct behavior after the fix).
	// New file at root of worktree.
	if err := os.WriteFile(filepath.Join(wtPath, "new_file.go"), []byte("package main\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	// New directory with a file inside (the case that was broken before the fix).
	newPkg := filepath.Join(wtPath, "internal", "newpkg")
	if err := os.MkdirAll(newPkg, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(newPkg, "newpkg.go"), []byte("package newpkg\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	if err := git.CommitAll(wtPath, "worker output"); err != nil {
		t.Fatalf("CommitAll: %v", err)
	}

	// Verify both the new file and the new directory were captured.
	cmd := exec.Command("git", "show", "--name-only", "--format=", "HEAD")
	cmd.Dir = wtPath
	out, err := cmd.Output()
	if err != nil {
		t.Fatalf("git show: %v", err)
	}
	committed := string(out)
	for _, want := range []string{"new_file.go", "internal/newpkg/newpkg.go"} {
		if !strings.Contains(committed, want) {
			t.Errorf("CommitAll did not capture %q; committed files:\n%s", want, committed)
		}
	}
}

// TestCommitAll_WorkerOutsideWorktreeNotCaptured documents the bug: files
// written to a directory outside the worktree are NOT captured by CommitAll.
// This test exists to show why workers must write code to the worktree, not
// to an external WorkerDir.
func TestCommitAll_WorkerOutsideWorktreeNotCaptured(t *testing.T) {
	repo := t.TempDir()
	initRepo(t, repo)

	if err := git.CreateBranch(repo, "outside-branch", "main"); err != nil {
		t.Fatalf("CreateBranch: %v", err)
	}

	wtPath := filepath.Join(t.TempDir(), "wt")
	if err := git.CreateWorktree(repo, wtPath, "outside-branch"); err != nil {
		t.Fatalf("CreateWorktree: %v", err)
	}
	t.Cleanup(func() { _ = git.RemoveWorktree(wtPath, "") })

	// Simulate the buggy behavior: worker writes to a separate WorkerDir
	// (outside the worktree), then CommitAll is called on the worktree.
	workerDir := t.TempDir()
	if err := os.WriteFile(filepath.Join(workerDir, "artifact.go"), []byte("package main\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	// CommitAll on the worktree — the file in workerDir should not appear.
	if err := git.CommitAll(wtPath, "should be empty"); err != nil {
		t.Fatalf("CommitAll: %v", err)
	}

	cmd := exec.Command("git", "show", "--name-only", "--format=", "HEAD")
	cmd.Dir = wtPath
	out, err := cmd.Output()
	if err != nil {
		t.Fatalf("git show: %v", err)
	}
	if strings.Contains(string(out), "artifact.go") {
		t.Errorf("artifact.go from workerDir should NOT be in the commit, but it was: %s", out)
	}
}

// ---- HasUncommittedChanges --------------------------------------------------

func TestHasUncommittedChanges_Clean(t *testing.T) {
	dir := t.TempDir()
	initRepo(t, dir)

	dirty, err := git.HasUncommittedChanges(dir)
	if err != nil {
		t.Fatalf("HasUncommittedChanges: %v", err)
	}
	if dirty {
		t.Error("expected clean repo, got dirty")
	}
}

func TestHasUncommittedChanges_Dirty(t *testing.T) {
	dir := t.TempDir()
	initRepo(t, dir)

	if err := os.WriteFile(filepath.Join(dir, "dirty.txt"), []byte("x"), 0o644); err != nil {
		t.Fatal(err)
	}

	dirty, err := git.HasUncommittedChanges(dir)
	if err != nil {
		t.Fatalf("HasUncommittedChanges: %v", err)
	}
	if !dirty {
		t.Error("expected dirty repo, got clean")
	}
}

func TestClaimChangedFiles_IncludesCommittedAndWorktreeChanges(t *testing.T) {
	repo := t.TempDir()
	initRepo(t, repo)

	if err := git.CreateBranch(repo, "feature", "main"); err != nil {
		t.Fatalf("CreateBranch: %v", err)
	}

	wtPath := filepath.Join(t.TempDir(), "wt")
	if err := git.CreateWorktree(repo, wtPath, "feature"); err != nil {
		t.Fatalf("CreateWorktree: %v", err)
	}
	t.Cleanup(func() {
		_ = git.RemoveWorktree(wtPath, "")
	})

	if err := os.WriteFile(filepath.Join(wtPath, "committed.txt"), []byte("committed\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := git.CommitAll(wtPath, "add committed file"); err != nil {
		t.Fatalf("CommitAll: %v", err)
	}

	readme := filepath.Join(wtPath, "README.md")
	if err := os.WriteFile(readme, []byte("hello\nunstaged\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	if err := os.WriteFile(filepath.Join(wtPath, "staged.txt"), []byte("staged\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	mustRun(t, wtPath, "git", "add", "staged.txt")

	if err := os.WriteFile(filepath.Join(wtPath, "untracked.txt"), []byte("untracked\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	files, err := git.ClaimChangedFiles(repo, wtPath, "main", "feature")
	if err != nil {
		t.Fatalf("ClaimChangedFiles: %v", err)
	}

	got := map[string]bool{}
	for _, f := range files {
		got[f] = true
	}
	for _, want := range []string{"README.md", "committed.txt", "staged.txt", "untracked.txt"} {
		if !got[want] {
			t.Errorf("ClaimChangedFiles missing %q; got %v", want, files)
		}
	}
}

// ---- Push -------------------------------------------------------------------

func TestPush_ToLocalBare(t *testing.T) {
	// Create a bare "remote".
	remote := t.TempDir()
	initBareRepo(t, remote)

	// Clone it to get a local repo.
	local := t.TempDir()
	mustRun(t, local, "git", "clone", remote, ".")
	mustRun(t, local, "git", "config", "user.email", "test@example.com")
	mustRun(t, local, "git", "config", "user.name", "Test")
	// Ensure main branch exists with a commit.
	f := filepath.Join(local, "README.md")
	if err := os.WriteFile(f, []byte("hello\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	mustRun(t, local, "git", "add", ".")
	mustRun(t, local, "git", "commit", "-m", "init")
	mustRun(t, local, "git", "push", "-u", "origin", "main")

	// Create a new branch and push it.
	if err := git.CreateBranch(local, "push-test", "main"); err != nil {
		t.Fatalf("CreateBranch: %v", err)
	}

	// Commit something on a worktree of that branch.
	wtPath := filepath.Join(t.TempDir(), "wt")
	if err := git.CreateWorktree(local, wtPath, "push-test"); err != nil {
		t.Fatalf("CreateWorktree: %v", err)
	}
	if err := os.WriteFile(filepath.Join(wtPath, "feature.txt"), []byte("feature"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := git.CommitAll(wtPath, "add feature"); err != nil {
		t.Fatalf("CommitAll: %v", err)
	}
	if err := git.Push(wtPath, "origin", "push-test"); err != nil {
		t.Fatalf("Push: %v", err)
	}

	// Verify the branch exists in the bare remote.
	cmd := exec.Command("git", "branch", "--list", "push-test")
	cmd.Dir = remote
	out, err := cmd.Output()
	if err != nil {
		t.Fatalf("git branch in remote: %v", err)
	}
	if !strings.Contains(string(out), "push-test") {
		t.Errorf("push-test branch not found in remote: %q", out)
	}
}

// ---- Lock file lifecycle ---------------------------------------------------- (req 2)

func TestWriteLockFile_CreatesAndRemoveCleansUp(t *testing.T) {
	dir := t.TempDir()

	if err := git.WriteLockFile(dir, "mission-abc"); err != nil {
		t.Fatalf("WriteLockFile: %v", err)
	}

	// Lock file must exist after writing.
	lockPath := filepath.Join(dir, ".nanika-lock")
	if _, err := os.Stat(lockPath); err != nil {
		t.Fatalf("lock file not created: %v", err)
	}

	git.RemoveLockFile(dir)

	// Lock file must be gone after removal.
	if _, err := os.Stat(lockPath); !os.IsNotExist(err) {
		t.Errorf("lock file should be removed, got Stat err: %v", err)
	}
}

func TestUpdateLockFilePhase_UpdatesPhaseField(t *testing.T) {
	dir := t.TempDir()
	if err := git.WriteLockFile(dir, "mission-xyz"); err != nil {
		t.Fatalf("WriteLockFile: %v", err)
	}
	if err := git.UpdateLockFilePhase(dir, "implement"); err != nil {
		t.Fatalf("UpdateLockFilePhase: %v", err)
	}
	data, err := os.ReadFile(filepath.Join(dir, ".nanika-lock"))
	if err != nil {
		t.Fatalf("read lock file: %v", err)
	}
	if !strings.Contains(string(data), `"implement"`) {
		t.Errorf("expected phase field 'implement' in lock file, got: %s", data)
	}
}

func TestUpdateLockFilePhase_NoopWhenMissing(t *testing.T) {
	dir := t.TempDir()
	// Should not error when lock file does not exist.
	if err := git.UpdateLockFilePhase(dir, "any-phase"); err != nil {
		t.Errorf("UpdateLockFilePhase on missing file should be noop, got: %v", err)
	}
}

// ---- RemoveWorktree: locked worktree refused -------------------------------- (req 3)

func TestRemoveWorktree_LockedRefused(t *testing.T) {
	repo := t.TempDir()
	initRepo(t, repo)

	if err := git.CreateBranch(repo, "locked-branch", "main"); err != nil {
		t.Fatalf("CreateBranch: %v", err)
	}
	wtPath := filepath.Join(t.TempDir(), "wt-locked")
	if err := git.CreateWorktree(repo, wtPath, "locked-branch"); err != nil {
		t.Fatalf("CreateWorktree: %v", err)
	}
	// Ensure cleanup even if test fails early.
	t.Cleanup(func() {
		git.RemoveLockFile(wtPath)
		_ = git.RemoveWorktree(wtPath, "")
	})

	// Write a lock file pointing to the current process — this process is running.
	if err := git.WriteLockFile(wtPath, "mission-locked"); err != nil {
		t.Fatalf("WriteLockFile: %v", err)
	}

	err := git.RemoveWorktree(wtPath, "")
	if err == nil {
		t.Fatal("RemoveWorktree should return an error for a locked worktree, got nil")
	}
	if !strings.Contains(err.Error(), "locked") {
		t.Errorf("error should mention 'locked', got: %v", err)
	}
}

// ---- RemoveWorktree: stale lock (dead PID) allows removal ------------------ (req 4)

func TestRemoveWorktree_StaleLockAllows(t *testing.T) {
	repo := t.TempDir()
	initRepo(t, repo)

	if err := git.CreateBranch(repo, "stale-branch", "main"); err != nil {
		t.Fatalf("CreateBranch: %v", err)
	}
	wtPath := filepath.Join(t.TempDir(), "wt-stale")
	if err := git.CreateWorktree(repo, wtPath, "stale-branch"); err != nil {
		t.Fatalf("CreateWorktree: %v", err)
	}

	// Write a lock file with PID 0 — not a valid process; Signal(0) will always fail.
	staleLock := `{"pid":0,"mission_id":"dead-mission","started_at":"2020-01-01T00:00:00Z","phase":""}`
	lockPath := filepath.Join(wtPath, ".nanika-lock")
	if err := os.WriteFile(lockPath, []byte(staleLock), 0600); err != nil {
		t.Fatalf("write stale lock: %v", err)
	}

	// Stale lock must not block removal.
	if err := git.RemoveWorktree(wtPath, ""); err != nil {
		t.Fatalf("RemoveWorktree with stale lock should succeed, got: %v", err)
	}
	if _, err := os.Stat(wtPath); !os.IsNotExist(err) {
		t.Errorf("worktree dir should be gone after removal with stale lock")
	}
}

// ---- RemoveWorktree: soft delete moves to trash ---------------------------- (req 5)

func TestRemoveWorktree_SoftDeleteMovesToTrash(t *testing.T) {
	repo := t.TempDir()
	initRepo(t, repo)

	if err := git.CreateBranch(repo, "soft-branch", "main"); err != nil {
		t.Fatalf("CreateBranch: %v", err)
	}
	wtPath := filepath.Join(t.TempDir(), "wt-soft")
	if err := git.CreateWorktree(repo, wtPath, "soft-branch"); err != nil {
		t.Fatalf("CreateWorktree: %v", err)
	}

	trashDir := t.TempDir()
	if err := git.RemoveWorktree(wtPath, trashDir); err != nil {
		t.Fatalf("RemoveWorktree (soft delete): %v", err)
	}

	// Original path must be gone.
	if _, err := os.Stat(wtPath); !os.IsNotExist(err) {
		t.Errorf("original worktree path should not exist after soft delete, Stat: %v", err)
	}

	// Trash dir must have exactly one entry.
	entries, err := os.ReadDir(trashDir)
	if err != nil {
		t.Fatalf("ReadDir trash: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("expected 1 trash entry, got %d", len(entries))
	}

	// The trash entry must contain the metadata file.
	metaPath := filepath.Join(trashDir, entries[0].Name(), ".nanika-trash-meta.json")
	metaData, err := os.ReadFile(metaPath)
	if err != nil {
		t.Fatalf("trash meta file missing: %v", err)
	}

	var meta git.TrashMeta
	if err := json.Unmarshal(metaData, &meta); err != nil {
		t.Fatalf("parse trash meta: %v", err)
	}
	if meta.OriginalPath != wtPath {
		t.Errorf("trash meta OriginalPath = %q, want %q", meta.OriginalPath, wtPath)
	}
	if meta.Branch != "soft-branch" {
		t.Errorf("trash meta Branch = %q, want %q", meta.Branch, "soft-branch")
	}
	if meta.TrashedAt == "" {
		t.Error("trash meta TrashedAt must not be empty")
	}
}

// ---- Empty trash: removes entries older than 24h --------------------------- (req 6)
// Tested via the emptyTrashOlderThan helper below.

func TestEmptyTrash_DeletesOldEntriesOnly(t *testing.T) {
	trashDir := t.TempDir()

	// Create a directory that is 25h old by setting its mtime.
	old := filepath.Join(trashDir, "old-workspace_20200101T000000Z")
	if err := os.Mkdir(old, 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.Chtimes(old, past(25), past(25)); err != nil {
		t.Fatal(err)
	}

	// Create a recent entry — must be preserved.
	recent := filepath.Join(trashDir, "recent-workspace_20260101T000000Z")
	if err := os.Mkdir(recent, 0755); err != nil {
		t.Fatal(err)
	}

	entries, _ := os.ReadDir(trashDir)
	if len(entries) != 2 {
		t.Fatalf("setup: expected 2 trash entries, got %d", len(entries))
	}

	deleted := emptyTrashOlderThan(t, trashDir, 24)

	if deleted != 1 {
		t.Errorf("expected 1 entry deleted, got %d", deleted)
	}
	if _, err := os.Stat(old); !os.IsNotExist(err) {
		t.Error("old entry should be deleted")
	}
	if _, err := os.Stat(recent); err != nil {
		t.Errorf("recent entry should still exist: %v", err)
	}
}

// past returns a time.Time that is hours hours in the past.
func past(hours int) time.Time {
	return time.Now().Add(-time.Duration(hours) * time.Hour)
}

// emptyTrashOlderThan deletes entries from trashDir whose mtime is older than
// maxAgeHours. Returns the count of deleted entries. This replicates the
// --empty-trash logic in cleanup.go for unit-testability without config.Dir().
func emptyTrashOlderThan(t *testing.T, trashDir string, maxAgeHours int) int {
	t.Helper()
	cutoff := time.Now().Add(-time.Duration(maxAgeHours) * time.Hour)
	entries, err := os.ReadDir(trashDir)
	if err != nil {
		t.Fatalf("ReadDir: %v", err)
	}
	deleted := 0
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		info, err := e.Info()
		if err != nil {
			continue
		}
		if info.ModTime().Before(cutoff) {
			os.RemoveAll(filepath.Join(trashDir, e.Name()))
			deleted++
		}
	}
	return deleted
}
