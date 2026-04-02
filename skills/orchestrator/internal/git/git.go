// Package git provides thin wrappers around git CLI operations used by the
// orchestrator to manage per-mission branch and worktree isolation.
package git

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"syscall"
	"time"
)

// FindRoot walks up from dir looking for a .git entry.
// Returns the directory containing .git, or "" if not found.
func FindRoot(dir string) string {
	for {
		if _, err := os.Stat(filepath.Join(dir, ".git")); err == nil {
			return dir
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			return "" // reached filesystem root
		}
		dir = parent
	}
}

// CurrentBranch returns the name of the currently checked-out branch in
// repoRoot. Returns an error if HEAD is detached.
func CurrentBranch(repoRoot string) (string, error) {
	out, err := run(repoRoot, "git", "symbolic-ref", "--short", "HEAD")
	if err != nil {
		return "", fmt.Errorf("git symbolic-ref: %w", err)
	}
	return strings.TrimSpace(out), nil
}

// CreateBranch creates a new branch named name in repoRoot, branching off base.
// base may be a branch name, tag, or commit SHA.
func CreateBranch(repoRoot, name, base string) error {
	if _, err := run(repoRoot, "git", "branch", name, base); err != nil {
		return fmt.Errorf("git branch %s %s: %w", name, base, err)
	}
	return nil
}

// CreateWorktree adds a linked worktree at path, checking out branch.
// The branch must already exist (create it first with CreateBranch).
func CreateWorktree(repoRoot, path, branch string) error {
	if _, err := run(repoRoot, "git", "worktree", "add", path, branch); err != nil {
		return fmt.Errorf("git worktree add %s %s: %w", path, branch, err)
	}
	return nil
}

const lockFileName = ".nanika-lock"

type worktreeLock struct {
	PID       int    `json:"pid"`
	MissionID string `json:"mission_id"`
	StartedAt string `json:"started_at"`
	Phase     string `json:"phase"`
}

// WriteLockFile writes a .nanika-lock file in worktreePath recording the
// current process PID, missionID, start time, and an empty initial phase.
func WriteLockFile(worktreePath, missionID string) error {
	lock := worktreeLock{
		PID:       os.Getpid(),
		MissionID: missionID,
		StartedAt: time.Now().UTC().Format(time.RFC3339),
	}
	data, err := json.Marshal(lock)
	if err != nil {
		return fmt.Errorf("marshal lock file: %w", err)
	}
	if err := os.WriteFile(filepath.Join(worktreePath, lockFileName), data, 0600); err != nil {
		return fmt.Errorf("write lock file: %w", err)
	}
	return nil
}

// UpdateLockFilePhase updates the phase field in an existing lock file.
// No-op when the lock file does not exist.
func UpdateLockFilePhase(worktreePath, phase string) error {
	lockPath := filepath.Join(worktreePath, lockFileName)
	data, err := os.ReadFile(lockPath)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("read lock file: %w", err)
	}
	var lock worktreeLock
	if err := json.Unmarshal(data, &lock); err != nil {
		return fmt.Errorf("parse lock file: %w", err)
	}
	lock.Phase = phase
	updated, err := json.Marshal(lock)
	if err != nil {
		return fmt.Errorf("marshal lock file: %w", err)
	}
	return os.WriteFile(lockPath, updated, 0600)
}

// RemoveLockFile removes the .nanika-lock file from worktreePath.
// Errors are silently ignored — removal is best-effort.
func RemoveLockFile(worktreePath string) {
	os.Remove(filepath.Join(worktreePath, lockFileName)) //nolint:errcheck
}

// isWorktreeLocked returns true when worktreePath contains a .nanika-lock file
// whose recorded PID refers to a still-running process.
func isWorktreeLocked(path string) bool {
	data, err := os.ReadFile(filepath.Join(path, lockFileName))
	if err != nil {
		return false
	}
	var lock worktreeLock
	if err := json.Unmarshal(data, &lock); err != nil || lock.PID <= 0 {
		return false
	}
	proc, err := os.FindProcess(lock.PID)
	if err != nil {
		return false
	}
	return proc.Signal(syscall.Signal(0)) == nil
}

const trashMetaFileName = ".nanika-trash-meta.json"

// TrashMeta records what a trashed worktree was before soft-deletion.
// Stored as JSON in the trash entry directory so restore can work without
// consulting any other state.
type TrashMeta struct {
	WorkspaceID  string `json:"workspace_id"`
	OriginalPath string `json:"original_path"`
	Branch       string `json:"branch"`
	RepoRoot     string `json:"repo_root"`
	TrashedAt    string `json:"trashed_at"`
}

// RemoveWorktree removes the worktree at path.
// When trashDir is non-empty the directory is moved there (soft delete) and a
// metadata file is written inside the trash entry so the worktree can be
// restored later.  When trashDir is empty the worktree is hard-deleted (the
// original behaviour, used by tests and fallback paths).
func RemoveWorktree(path, trashDir string) error {
	if isWorktreeLocked(path) {
		return fmt.Errorf("worktree %s is locked by an active process", path)
	}
	// Resolve the main repo root before the worktree is touched.
	mainRoot, err := mainRepoRoot(path)
	if err != nil {
		mainRoot = path // best-effort fallback
	}

	if trashDir == "" {
		// Hard delete — original behaviour.
		if _, err := run(mainRoot, "git", "worktree", "remove", "--force", path); err != nil {
			return fmt.Errorf("git worktree remove %s: %w", path, err)
		}
		if _, err := run(mainRoot, "git", "worktree", "prune"); err != nil {
			return fmt.Errorf("git worktree prune: %w", err)
		}
		return nil
	}

	// Soft delete — move to trash then prune the dangling git reference.
	if err := os.MkdirAll(trashDir, 0755); err != nil {
		return fmt.Errorf("create trash dir: %w", err)
	}

	now := time.Now().UTC()
	workspaceID := filepath.Base(path)
	trashEntry := filepath.Join(trashDir, workspaceID+"_"+now.Format("20060102T150405Z"))

	// Determine the current branch from the worktree's HEAD before moving it.
	branch := ""
	if b, err := CurrentBranch(path); err == nil {
		branch = b
	}

	meta := TrashMeta{
		WorkspaceID:  workspaceID,
		OriginalPath: path,
		Branch:       branch,
		RepoRoot:     mainRoot,
		TrashedAt:    now.Format(time.RFC3339),
	}

	if err := os.Rename(path, trashEntry); err != nil {
		return fmt.Errorf("move worktree to trash: %w", err)
	}

	// Write metadata so restore can find everything it needs.
	if metaData, err := json.Marshal(meta); err == nil {
		_ = os.WriteFile(filepath.Join(trashEntry, trashMetaFileName), metaData, 0600)
	}

	// Prune the now-missing worktree reference from the main repo.
	if _, err := run(mainRoot, "git", "worktree", "prune"); err != nil {
		// Non-fatal — the worktree is in trash, just the git ref is stale.
		fmt.Printf("warning: git worktree prune: %v\n", err)
	}

	fmt.Printf("worktree moved to trash: %s\n", trashEntry)
	return nil
}

// RepairWorktree re-registers a linked worktree whose git metadata was lost
// (e.g. after a prune).  Run from the main repo root after the worktree
// directory has been restored to its original path.
func RepairWorktree(repoRoot, worktreePath string) error {
	if _, err := run(repoRoot, "git", "worktree", "repair", worktreePath); err != nil {
		return fmt.Errorf("git worktree repair %s: %w", worktreePath, err)
	}
	return nil
}

// mainRepoRoot returns the root directory of the main (non-linked) worktree
// that owns the git repository at dir. Works correctly from both the main
// worktree and linked worktrees.
func mainRepoRoot(dir string) (string, error) {
	// --git-common-dir returns ".git" from the main worktree, or an absolute
	// path to the main .git dir from a linked worktree. Its parent is the
	// main worktree root in both cases.
	out, err := run(dir, "git", "rev-parse", "--git-common-dir")
	if err != nil {
		return "", fmt.Errorf("rev-parse --git-common-dir: %w", err)
	}
	commonDir := strings.TrimSpace(out)
	if !filepath.IsAbs(commonDir) {
		commonDir = filepath.Join(dir, commonDir)
	}
	return filepath.Dir(commonDir), nil
}

// CommitAll stages all changes in worktreePath and creates a commit with message.
// Returns nil (no error) if there is nothing to commit.
func CommitAll(worktreePath, message string) error {
	if _, err := run(worktreePath, "git", "add", "-A"); err != nil {
		return fmt.Errorf("git add -A: %w", err)
	}
	_, err := run(worktreePath, "git", "commit", "-m", message)
	if err != nil {
		// "nothing to commit" is not an error for callers.
		if strings.Contains(err.Error(), "nothing to commit") {
			return nil
		}
		return fmt.Errorf("git commit: %w", err)
	}
	return nil
}

// HeadSHA returns the full commit hash of HEAD in dir.
func HeadSHA(dir string) (string, error) {
	out, err := run(dir, "git", "rev-parse", "HEAD")
	if err != nil {
		return "", fmt.Errorf("git rev-parse HEAD: %w", err)
	}
	return strings.TrimSpace(out), nil
}

// Push pushes branch to remote from within worktreePath.
func Push(worktreePath, remote, branch string) error {
	if _, err := run(worktreePath, "git", "push", remote, branch); err != nil {
		return fmt.Errorf("git push %s %s: %w", remote, branch, err)
	}
	return nil
}

// DiffNameOnly returns the list of files that differ between the current
// worktree state (including uncommitted changes) and base in dir.
// Equivalent to: git diff --name-only <base>
// Returns nil (no error) when there are no differences.
func DiffNameOnly(dir, base string) ([]string, error) {
	return nameOnly(dir, "git", "diff", "--name-only", base)
}

// DiffNameStatus returns a map of file path → status letter for all files
// that differ between the current worktree state and base in dir.
// Status letters follow git convention: 'A' added, 'M' modified, 'D' deleted, etc.
// Equivalent to: git diff --name-status <base>
func DiffNameStatus(dir, base string) (map[string]string, error) {
	out, err := run(dir, "git", "diff", "--name-status", base)
	if err != nil {
		return nil, fmt.Errorf("git diff --name-status %s: %w", base, err)
	}
	result := make(map[string]string)
	for _, line := range strings.Split(strings.TrimSpace(out), "\n") {
		if line == "" {
			continue
		}
		fields := strings.Fields(line)
		if len(fields) < 2 {
			continue
		}
		// Status letter may have a score suffix (e.g. "R100"), use first char.
		status := string(fields[0][0])
		result[fields[len(fields)-1]] = status
	}
	return result, nil
}

// HasUncommittedChanges reports whether path contains any staged or unstaged
// changes (including untracked files).
func HasUncommittedChanges(path string) (bool, error) {
	out, err := run(path, "git", "status", "--porcelain")
	if err != nil {
		return false, fmt.Errorf("git status: %w", err)
	}
	return strings.TrimSpace(out) != "", nil
}

// ClaimChangedFiles returns the union of committed branch diff files and any
// staged, unstaged, or untracked files currently present in worktreePath.
// This preserves advisory file claims for preserved worktrees whose latest
// changes have not yet been committed.
func ClaimChangedFiles(repoRoot, worktreePath, base, head string) ([]string, error) {
	seen := make(map[string]bool)
	var files []string

	addFiles := func(list []string) {
		for _, f := range list {
			if f == "" || seen[f] {
				continue
			}
			seen[f] = true
			files = append(files, f)
		}
	}

	if repoRoot != "" && base != "" && head != "" {
		committed, err := ChangedFiles(repoRoot, base, head)
		if err != nil {
			return nil, err
		}
		addFiles(committed)
	}

	if worktreePath != "" {
		staged, err := nameOnly(worktreePath, "git", "diff", "--name-only", "--cached")
		if err != nil {
			return nil, fmt.Errorf("git diff --name-only --cached: %w", err)
		}
		addFiles(staged)

		unstaged, err := nameOnly(worktreePath, "git", "diff", "--name-only")
		if err != nil {
			return nil, fmt.Errorf("git diff --name-only: %w", err)
		}
		addFiles(unstaged)

		untracked, err := nameOnly(worktreePath, "git", "ls-files", "--others", "--exclude-standard")
		if err != nil {
			return nil, fmt.Errorf("git ls-files --others --exclude-standard: %w", err)
		}
		addFiles(untracked)
	}

	slices.Sort(files)
	return files, nil
}

// ListTrackedFiles returns the list of files tracked by git in repoRoot.
// The paths are relative to the repository root, matching the output of
// `git ls-files`.
func ListTrackedFiles(repoRoot string) ([]string, error) {
	out, err := run(repoRoot, "git", "ls-files")
	if err != nil {
		return nil, fmt.Errorf("git ls-files: %w", err)
	}
	var files []string
	for _, line := range strings.Split(strings.TrimSpace(out), "\n") {
		if line != "" {
			files = append(files, line)
		}
	}
	return files, nil
}

// BaseBranchMoved reports whether baseBranch contains commits that are not
// reachable from featureBranch. Returns the one-line summaries of those
// commits so the caller can surface a meaningful warning.
// A non-empty slice (and true) means the base has advanced since the feature
// branch was created from it.
func BaseBranchMoved(repoRoot, baseBranch, featureBranch string) (bool, []string, error) {
	out, err := run(repoRoot, "git", "log", "--oneline", baseBranch, "--not", featureBranch)
	if err != nil {
		return false, nil, fmt.Errorf("git log: %w", err)
	}
	var commits []string
	for _, line := range strings.Split(strings.TrimSpace(out), "\n") {
		if line != "" {
			commits = append(commits, line)
		}
	}
	return len(commits) > 0, commits, nil
}

func nameOnly(dir string, args ...string) ([]string, error) {
	out, err := run(dir, args...)
	if err != nil {
		return nil, err
	}
	var files []string
	for _, line := range strings.Split(strings.TrimSpace(out), "\n") {
		if line != "" {
			files = append(files, line)
		}
	}
	return files, nil
}

// run executes a git command in dir and returns combined stdout+stderr output.
// Git-specific environment variables that override repository discovery
// (GIT_DIR, GIT_WORK_TREE, GIT_INDEX_FILE) are stripped from the child
// process so that commands targeting a linked worktree are not redirected
// to whatever repository the parent process happens to be running inside.
func run(dir string, args ...string) (string, error) {
	cmd := exec.Command(args[0], args[1:]...) //nolint:gosec
	cmd.Dir = dir
	cmd.Env = gitSafeEnv()
	var buf bytes.Buffer
	cmd.Stdout = &buf
	cmd.Stderr = &buf
	if err := cmd.Run(); err != nil {
		return "", fmt.Errorf("%s: %w", buf.String(), err)
	}
	return buf.String(), nil
}

// gitSafeEnv returns os.Environ() with git repository-override variables
// removed. GIT_DIR, GIT_WORK_TREE, and GIT_INDEX_FILE, when set in the
// parent process, cause git to operate on the wrong repository and bypass
// linked-worktree discovery entirely.
func gitSafeEnv() []string {
	skip := map[string]bool{
		"GIT_DIR":        true,
		"GIT_WORK_TREE":  true,
		"GIT_INDEX_FILE": true,
	}
	env := os.Environ()
	out := make([]string, 0, len(env))
	for _, e := range env {
		key := e
		if i := strings.IndexByte(e, '='); i >= 0 {
			key = e[:i]
		}
		if !skip[key] {
			out = append(out, e)
		}
	}
	return out
}
