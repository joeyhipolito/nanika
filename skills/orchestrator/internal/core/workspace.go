package core

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/config"
)

// ValidateWorkspacePath ensures the given path is a real subdirectory of
// ~/.alluka/workspaces/ and contains no path traversal components. This prevents
// --resume from being used to read arbitrary files on disk.
func ValidateWorkspacePath(wsPath string) error {
	base, err := config.Dir()
	if err != nil {
		return fmt.Errorf("cannot determine config directory: %w", err)
	}

	wsBase := filepath.Join(base, "workspaces")

	// Resolve to absolute, canonicalized path (follows symlinks)
	abs, err := filepath.EvalSymlinks(wsPath)
	if err != nil {
		// Path doesn't exist yet — at least resolve what we can
		abs, err = filepath.Abs(wsPath)
		if err != nil {
			return fmt.Errorf("cannot resolve workspace path: %w", err)
		}
	}

	// Reject any remaining ".." after resolution
	if strings.Contains(abs, "..") {
		return fmt.Errorf("path traversal rejected: %q", wsPath)
	}

	// Must be strictly inside ~/.alluka/workspaces/
	if !strings.HasPrefix(abs, wsBase+string(filepath.Separator)) {
		return fmt.Errorf("workspace path %q is outside %s", wsPath, wsBase)
	}

	return nil
}

// Workspace represents a mission workspace on disk.
type Workspace struct {
	ID        string
	Path      string
	Task      string
	Domain    string
	CreatedAt time.Time
	// TargetDir is the default CWD for all phases in this workspace.
	// Derived from the resolved target repository (e.g. from tc.TargetID).
	// Individual phases may override this via Phase.TargetDir.
	// Empty means each worker runs in its own WorkerDir (legacy behaviour).
	TargetDir string

	// Git isolation fields (populated when the workspace is associated with a
	// git repository and branch/worktree isolation is enabled).

	// GitRepoRoot is the root of the target git repository (contains .git).
	GitRepoRoot string `json:"git_repo_root,omitempty"`
	// WorktreePath is the path to the linked worktree for this mission
	// (e.g. ~/.alluka/worktrees/<id>/). Empty when worktree isolation is not used.
	WorktreePath string `json:"worktree_path,omitempty"`
	// BranchName is the name of the git branch created for this mission
	// (e.g. via/<mission-id>/<slug>).
	BranchName string `json:"branch_name,omitempty"`
	// BaseBranch is the branch that BranchName was created from (e.g. "main").
	BaseBranch string `json:"base_branch,omitempty"`
}

// CreateWorkspace creates a new workspace directory structure.
func CreateWorkspace(task, domain string) (*Workspace, error) {
	id := generateID()

	base, err := config.Dir()
	if err != nil {
		return nil, fmt.Errorf("failed to get config directory: %w", err)
	}

	wsPath := filepath.Join(base, "workspaces", id)

	// Create directory structure
	dirs := []string{
		wsPath,
		filepath.Join(wsPath, "workers"),
		filepath.Join(wsPath, "artifacts"),
		filepath.Join(wsPath, "artifacts", "merged"),
		filepath.Join(wsPath, "learnings"),
	}

	for _, dir := range dirs {
		if err := os.MkdirAll(dir, 0700); err != nil {
			return nil, fmt.Errorf("failed to create directory %s: %w", dir, err)
		}
	}

	// Write mission file
	missionPath := filepath.Join(wsPath, "mission.md")
	if err := os.WriteFile(missionPath, []byte(task), 0600); err != nil {
		return nil, fmt.Errorf("failed to write mission file: %w", err)
	}

	return &Workspace{
		ID:        id,
		Path:      wsPath,
		Task:      task,
		Domain:    domain,
		CreatedAt: time.Now(),
	}, nil
}

// CreateWorkerDir creates a worker subdirectory within the workspace.
func CreateWorkerDir(wsPath, workerName string) (string, error) {
	workerDir := filepath.Join(wsPath, "workers", workerName)

	dirs := []string{
		workerDir,
		filepath.Join(workerDir, ".claude", "hooks"),
	}

	for _, dir := range dirs {
		if err := os.MkdirAll(dir, 0700); err != nil {
			return "", fmt.Errorf("failed to create worker directory %s: %w", dir, err)
		}
	}

	return workerDir, nil
}

// CreatePhaseArtifactDir creates the artifact directory for a phase.
func CreatePhaseArtifactDir(wsPath, phaseID string) (string, error) {
	dir := filepath.Join(wsPath, "artifacts", phaseID)
	if err := os.MkdirAll(dir, 0700); err != nil {
		return "", fmt.Errorf("failed to create artifact dir: %w", err)
	}
	return dir, nil
}

// MergedArtifactsDir returns the path to the merged artifacts directory.
func MergedArtifactsDir(wsPath string) string {
	return filepath.Join(wsPath, "artifacts", "merged")
}

// LearningsDir returns the path to the learnings directory.
func LearningsDir(wsPath string) string {
	return filepath.Join(wsPath, "learnings")
}

// ListWorkspaces returns all workspace directories, newest first.
func ListWorkspaces() ([]string, error) {
	base, err := config.Dir()
	if err != nil {
		return nil, err
	}

	wsBase := filepath.Join(base, "workspaces")
	entries, err := os.ReadDir(wsBase)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, err
	}

	var paths []string
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		// Only include v2 workspaces (contain mission.md, not v1 domain subdirs)
		wsPath := filepath.Join(wsBase, e.Name())
		if _, err := os.Stat(filepath.Join(wsPath, "mission.md")); err == nil {
			paths = append(paths, wsPath)
		}
	}

	// Reverse for newest first (IDs are timestamp-prefixed)
	for i, j := 0, len(paths)-1; i < j; i, j = i+1, j-1 {
		paths[i], paths[j] = paths[j], paths[i]
	}

	return paths, nil
}

// WorkspaceLink holds the linking metadata for a workspace: which issue it
// belongs to, the source mission file, and its execution status.
// All fields are optional — older workspaces without frontmatter will have
// zero-value strings.
type WorkspaceLink struct {
	WorkspaceID   string // e.g. "20260315-ab12cd34"
	WorkspacePath string // absolute path to workspace dir
	LinearIssueID string // e.g. "V-5", empty if no frontmatter
	MissionPath   string // absolute path to source mission file, empty if ad-hoc
	Status        string // from checkpoint: "completed", "failed", "in_progress", ""
	// Degradations is non-nil (and non-empty) when one or more linkage problems
	// were detected. Older workspaces that simply have no sidecars are not
	// degraded — they just have empty optional fields.
	Degradations []string
}

// CheckWorkspaceLinkHealth inspects sidecar files for linkage problems and
// returns a (possibly empty) slice of human-readable degradation descriptions.
// The checks are intentionally conservative: a missing sidecar is treated as
// "older/ad-hoc workspace" (not degraded), while an *existing* sidecar whose
// content is invalid or whose referent is gone is flagged.
//
// Degradation conditions detected:
//   - linear_issue_id sidecar exists but contains only whitespace
//   - mission_path sidecar exists but the referenced file is not accessible
//   - checkpoint.json is absent (workspace was started but never checkpointed)
//   - checkpoint.json exists but cannot be parsed (corrupt JSON)
func CheckWorkspaceLinkHealth(wsPath string) []string {
	var d []string

	// Check 1: issue ID sidecar exists but is blank.
	if raw, err := os.ReadFile(filepath.Join(wsPath, "linear_issue_id")); err == nil {
		if strings.TrimSpace(string(raw)) == "" {
			d = append(d, "issue ID sidecar is blank")
		}
	}

	// Check 2: mission_path sidecar exists but the file is gone.
	if raw, err := os.ReadFile(filepath.Join(wsPath, "mission_path")); err == nil {
		mp := strings.TrimSpace(string(raw))
		if mp != "" {
			if _, statErr := os.Stat(mp); statErr != nil {
				if os.IsNotExist(statErr) {
					d = append(d, "mission file missing: "+mp)
				} else {
					d = append(d, "mission file unreadable: "+mp)
				}
			}
		}
	}

	// Check 3: checkpoint.json absent — workspace never reached first save.
	cpPath := filepath.Join(wsPath, "checkpoint.json")
	cpData, cpErr := os.ReadFile(cpPath)
	if cpErr != nil {
		if os.IsNotExist(cpErr) {
			d = append(d, "checkpoint missing")
		}
		// Non-NotExist errors (permissions, I/O) are not surfaced as a
		// degradation; they would have been caught by the caller already.
	} else {
		// Check 4: checkpoint.json present but unparseable.
		var stub struct {
			Status string `json:"status"`
		}
		if json.Unmarshal(cpData, &stub) != nil {
			d = append(d, "checkpoint corrupt")
		}
	}

	return d
}

func normalizeIssueID(raw string) string {
	return strings.ToUpper(strings.TrimSpace(raw))
}

// FindWorkspacesByIssue scans all workspaces and returns those whose
// linear_issue_id sidecar matches the given issueID (case-insensitive).
// Returns nil (not error) when no matches are found. Workspaces without
// a linear_issue_id sidecar are silently skipped.
func FindWorkspacesByIssue(issueID string) ([]WorkspaceLink, error) {
	workspaces, err := ListWorkspaces()
	if err != nil {
		return nil, err
	}

	issueID = normalizeIssueID(issueID)
	var matches []WorkspaceLink
	for _, wsPath := range workspaces {
		raw, err := os.ReadFile(filepath.Join(wsPath, "linear_issue_id"))
		if err != nil {
			continue // no sidecar — skip
		}
		wsIssue := normalizeIssueID(string(raw))
		if wsIssue != issueID {
			continue
		}

		link := WorkspaceLink{
			WorkspaceID:   filepath.Base(wsPath),
			WorkspacePath: wsPath,
			LinearIssueID: wsIssue,
		}
		if mp, err := os.ReadFile(filepath.Join(wsPath, "mission_path")); err == nil {
			link.MissionPath = strings.TrimSpace(string(mp))
		}
		// Best-effort checkpoint status — don't fail if checkpoint is missing.
		if data, err := os.ReadFile(filepath.Join(wsPath, "checkpoint.json")); err == nil {
			var stub struct {
				Status string `json:"status"`
			}
			if json.Unmarshal(data, &stub) == nil {
				link.Status = stub.Status
			}
		}
		link.Degradations = CheckWorkspaceLinkHealth(wsPath)
		matches = append(matches, link)
	}
	return matches, nil
}

// ListWorkspaceLinks returns linking metadata for the N most recent workspaces.
// Workspaces without sidecar files get zero-value strings for the optional
// fields — they are never skipped.
func ListWorkspaceLinks(limit int) ([]WorkspaceLink, error) {
	workspaces, err := ListWorkspaces()
	if err != nil {
		return nil, err
	}
	if limit > 0 && limit < len(workspaces) {
		workspaces = workspaces[:limit]
	}

	links := make([]WorkspaceLink, 0, len(workspaces))
	for _, wsPath := range workspaces {
		link := WorkspaceLink{
			WorkspaceID:   filepath.Base(wsPath),
			WorkspacePath: wsPath,
		}
		if raw, err := os.ReadFile(filepath.Join(wsPath, "linear_issue_id")); err == nil {
			link.LinearIssueID = normalizeIssueID(string(raw))
		}
		if raw, err := os.ReadFile(filepath.Join(wsPath, "mission_path")); err == nil {
			link.MissionPath = strings.TrimSpace(string(raw))
		}
		if data, err := os.ReadFile(filepath.Join(wsPath, "checkpoint.json")); err == nil {
			var stub struct {
				Status string `json:"status"`
			}
			if json.Unmarshal(data, &stub) == nil {
				link.Status = stub.Status
			}
		}
		link.Degradations = CheckWorkspaceLinkHealth(wsPath)
		links = append(links, link)
	}
	return links, nil
}

func readWorkspaceMissionPath(wsPath string) (string, error) {
	raw, err := os.ReadFile(filepath.Join(wsPath, "mission_path"))
	if err != nil {
		return "", nil // no source mission — not an error
	}
	missionPath := strings.TrimSpace(string(raw))
	if missionPath == "" {
		return "", nil
	}
	return missionPath, nil
}

func managedMissionPath(wsPath string) (string, bool, error) {
	missionPath, err := readWorkspaceMissionPath(wsPath)
	if err != nil || missionPath == "" {
		return "", false, err
	}
	base, err := config.Dir()
	if err != nil {
		return "", false, fmt.Errorf("resolving config dir: %w", err)
	}
	managedBase := filepath.Join(base, "missions") + string(filepath.Separator)
	cleanMission, err := filepath.Abs(missionPath)
	if err != nil {
		return "", false, fmt.Errorf("resolving mission path: %w", err)
	}
	return cleanMission, strings.HasPrefix(cleanMission, managedBase), nil
}

// ResolveTargetDir converts a canonical target ID (e.g. "repo:~/nanika/skills/orchestrator")
// to an absolute filesystem path. Returns "" when the ID is not repo:-scheme,
// when the path does not exist on disk, or when the home directory cannot be determined.
// This is the inverse of the "repo:~/<path>" canonical form used by routing.
//
// Legacy target IDs using "repo:~/skills/<name>" are resolved by trying
// ~/nanika/skills/<name> first, then falling back to ~/skills/<name> (symlink).
func ResolveTargetDir(targetID string) string {
	if !strings.HasPrefix(targetID, "repo:") {
		return ""
	}
	path := strings.TrimPrefix(targetID, "repo:")
	if strings.HasPrefix(path, "~/") {
		home, err := os.UserHomeDir()
		if err != nil {
			return ""
		}
		path = filepath.Join(home, path[2:])

		// Legacy: try ~/nanika/skills/ when ~/skills/ is requested
		if strings.HasPrefix(path, filepath.Join(home, "skills")+string(filepath.Separator)) {
			nanikaPath := filepath.Join(home, "nanika", path[len(home)+1:])
			if _, err := os.Stat(nanikaPath); err == nil {
				return nanikaPath
			}
		}
	}
	if _, err := os.Stat(path); err != nil {
		return ""
	}
	return path
}

// SyncManagedMissionStatus is the automatic write-back path used during
// execution. It only updates runtime mission files under ~/.alluka/missions/,
// avoiding silent edits to repo-local mission files. Returns the synced path,
// or "" when auto-sync is not applicable.
func SyncManagedMissionStatus(wsPath string) (string, error) {
	missionPath, ok, err := managedMissionPath(wsPath)
	if err != nil || !ok {
		return "", err
	}
	return syncMissionStatusToPath(wsPath, missionPath)
}

// SyncMissionStatus reads the workspace's checkpoint status and mission_path
// sidecar, then updates (or inserts) the `status:` field in the source mission
// file's YAML frontmatter. No-op when:
//   - mission_path sidecar is missing (ad-hoc task, no source file)
//   - the mission file has no YAML frontmatter (older missions)
//   - the file no longer exists on disk
//
// Returns the path that was written, or "" when nothing was synced.
// Callers are responsible for scope gating. Automatic execution paths should
// use SyncManagedMissionStatus instead.
func SyncMissionStatus(wsPath string) (string, error) {
	missionPath, err := readWorkspaceMissionPath(wsPath)
	if err != nil || missionPath == "" {
		return "", err
	}
	return syncMissionStatusToPath(wsPath, missionPath)
}

func syncMissionStatusToPath(wsPath, missionPath string) (string, error) {
	// Read workspace status from checkpoint.
	cpData, err := os.ReadFile(filepath.Join(wsPath, "checkpoint.json"))
	if err != nil {
		return "", fmt.Errorf("reading checkpoint: %w", err)
	}
	var stub struct {
		Status string `json:"status"`
	}
	if err := json.Unmarshal(cpData, &stub); err != nil {
		return "", fmt.Errorf("parsing checkpoint: %w", err)
	}
	if stub.Status == "" || stub.Status == "in_progress" {
		return "", nil // nothing final to sync
	}

	// Read the mission file.
	body, err := os.ReadFile(missionPath)
	if err != nil {
		if os.IsNotExist(err) {
			return "", nil // file gone — not an error
		}
		return "", fmt.Errorf("reading mission file: %w", err)
	}

	content := string(body)
	newline := "\n"
	switch {
	case strings.HasPrefix(content, "---\r\n"):
		newline = "\r\n"
	case strings.HasPrefix(content, "---\n"):
		newline = "\n"
	default:
		return "", nil // no frontmatter — don't touch
	}

	lines := strings.Split(content, newline)
	closingIdx := -1
	for i := 1; i < len(lines); i++ {
		if strings.TrimSpace(lines[i]) == "---" {
			closingIdx = i
			break
		}
	}
	if closingIdx == -1 {
		return "", nil // malformed frontmatter
	}

	// Update or insert status: field within frontmatter.
	// Values like "completed", "failed", "in_progress" are plain YAML scalars
	// and must not be Go-quoted.
	statusLine := "status: " + stub.Status
	found := false
	for i := 1; i < closingIdx; i++ {
		trimmed := strings.TrimSpace(lines[i])
		parts := strings.SplitN(trimmed, ":", 2)
		if len(parts) == 2 && strings.TrimSpace(parts[0]) == "status" {
			lines[i] = statusLine
			found = true
			break
		}
	}
	if !found {
		// Insert before closing --- using a safe copy to avoid aliasing.
		tail := make([]string, len(lines[closingIdx:]))
		copy(tail, lines[closingIdx:])
		lines = append(lines[:closingIdx], statusLine)
		lines = append(lines, tail...)
	}

	updated := strings.Join(lines, newline)
	if err := os.WriteFile(missionPath, []byte(updated), 0600); err != nil {
		return "", fmt.Errorf("writing mission file: %w", err)
	}
	return missionPath, nil
}

// WritePID writes the current process PID to the workspace so that the cancel
// command can find and signal a running mission.
func WritePID(wsPath string) error {
	return os.WriteFile(filepath.Join(wsPath, "pid"), []byte(fmt.Sprintf("%d", os.Getpid())), 0600)
}

// ReadPID reads the orchestrator process PID from the workspace.
// Returns 0 and a nil error when the pid file does not exist (older workspace).
func ReadPID(wsPath string) (int, error) {
	data, err := os.ReadFile(filepath.Join(wsPath, "pid"))
	if err != nil {
		if os.IsNotExist(err) {
			return 0, nil
		}
		return 0, fmt.Errorf("reading pid file: %w", err)
	}
	var pid int
	if _, err := fmt.Sscanf(strings.TrimSpace(string(data)), "%d", &pid); err != nil {
		return 0, fmt.Errorf("parsing pid: %w", err)
	}
	return pid, nil
}

// WriteCancelSentinel creates a cancel sentinel file in the workspace.
// The engine checks for this file between phases and aborts if present.
func WriteCancelSentinel(wsPath string) error {
	return os.WriteFile(filepath.Join(wsPath, "cancel"), []byte("cancelled"), 0600)
}

// HasCancelSentinel reports whether a cancel sentinel exists in the workspace.
func HasCancelSentinel(wsPath string) bool {
	_, err := os.Stat(filepath.Join(wsPath, "cancel"))
	return err == nil
}

// ResolveWorkspacePath converts a workspace ID (e.g. "20260316-ab12cd34") to
// its absolute path under ~/.alluka/workspaces/. Returns an error if the workspace
// does not exist.
func ResolveWorkspacePath(wsID string) (string, error) {
	base, err := config.Dir()
	if err != nil {
		return "", fmt.Errorf("cannot determine config directory: %w", err)
	}
	wsPath := filepath.Join(base, "workspaces", wsID)
	if _, err := os.Stat(wsPath); err != nil {
		if os.IsNotExist(err) {
			return "", fmt.Errorf("workspace %q not found", wsID)
		}
		return "", fmt.Errorf("accessing workspace: %w", err)
	}
	return wsPath, nil
}

func generateID() string {
	ts := time.Now().Format("20060102")
	b := make([]byte, 4)
	rand.Read(b)
	return fmt.Sprintf("%s-%s", ts, hex.EncodeToString(b))
}
