package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/claims"
	"github.com/joeyhipolito/orchestrator-cli/internal/config"
	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/git"
)

var (
	cleanWorktrees   bool
	cleanClaims      bool
	emptyTrash       bool
	restoreWorktree  string
)

func init() {
	cleanupCmd := &cobra.Command{
		Use:   "cleanup",
		Short: "Remove old workspaces (older than 7 days)",
		RunE:  runCleanup,
	}

	cleanupCmd.Flags().BoolVar(&cleanWorktrees, "worktrees", false, "remove orphaned worktrees from ~/.alluka/worktrees/")
	cleanupCmd.Flags().BoolVar(&cleanClaims, "claims", false, "purge file claims older than 7 days from the registry")
	cleanupCmd.Flags().BoolVar(&emptyTrash, "empty-trash", false, "permanently delete trashed worktrees older than 24h")
	cleanupCmd.Flags().StringVar(&restoreWorktree, "restore", "", "restore a trashed worktree by workspace ID (e.g. 20260331-abc)")

	rootCmd.AddCommand(cleanupCmd)
}

func runCleanup(cmd *cobra.Command, args []string) error {
	if restoreWorktree != "" {
		return runRestoreWorktree(restoreWorktree)
	}

	if emptyTrash {
		return runEmptyTrash()
	}

	if cleanWorktrees {
		return runWorktreeCleanup()
	}

	if cleanClaims {
		return runClaimsCleanup()
	}

	workspaces, err := core.ListWorkspaces()
	if err != nil {
		return err
	}

	cutoff := time.Now().Add(-7 * 24 * time.Hour)
	removed := 0

	for _, wsPath := range workspaces {
		info, err := os.Stat(wsPath)
		if err != nil {
			continue
		}

		if info.ModTime().Before(cutoff) {
			if dryRun {
				fmt.Printf("would remove: %s\n", filepath.Base(wsPath))
			} else {
				os.RemoveAll(wsPath)
				fmt.Printf("removed: %s\n", filepath.Base(wsPath))
			}
			removed++
		}
	}

	if removed == 0 {
		fmt.Println("no old workspaces to clean up")
	} else if dryRun {
		fmt.Printf("%d workspaces would be removed\n", removed)
	} else {
		fmt.Printf("%d workspaces removed\n", removed)
	}

	return nil
}

// runWorktreeCleanup removes orphaned worktrees under ~/.alluka/worktrees/.
// A worktree is considered orphaned when its corresponding workspace
// (same directory name) has a terminal checkpoint status or no longer exists.
// Active (in_progress) workspaces are skipped to avoid destroying live missions.
func runWorktreeCleanup() error {
	base, err := config.Dir()
	if err != nil {
		return fmt.Errorf("get config dir: %w", err)
	}

	trashDir := filepath.Join(base, "trash")
	worktreesDir := filepath.Join(base, "worktrees")
	entries, err := os.ReadDir(worktreesDir)
	if err != nil {
		if os.IsNotExist(err) {
			fmt.Println("no orphaned worktrees to clean up")
			return nil
		}
		return fmt.Errorf("reading worktrees dir: %w", err)
	}

	removed := 0
	skipped := 0
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}

		// Check if the corresponding workspace is still active.
		wsPath := filepath.Join(base, "workspaces", e.Name())
		if isWorkspaceActive(wsPath) {
			if verbose {
				fmt.Printf("skipping active worktree: %s\n", e.Name())
			}
			skipped++
			continue
		}

		worktreePath := filepath.Join(worktreesDir, e.Name())
		if dryRun {
			fmt.Printf("would remove worktree: %s\n", e.Name())
		} else {
			if err := git.RemoveWorktree(worktreePath, trashDir); err != nil {
				// Graceful removal failed (repo may be gone); fall back to direct delete.
				os.RemoveAll(worktreePath)
			}
		}
		removed++
	}

	if removed == 0 && skipped == 0 {
		fmt.Println("no orphaned worktrees to clean up")
	} else {
		if removed > 0 {
			if dryRun {
				fmt.Printf("%d worktrees would be removed\n", removed)
			} else {
				fmt.Printf("%d worktrees removed\n", removed)
			}
		}
		if skipped > 0 {
			fmt.Printf("%d active worktrees skipped\n", skipped)
		}
	}

	return nil
}

// isWorkspaceActive returns true if the workspace exists and has a non-terminal
// checkpoint status (i.e., "in_progress"). Missing workspaces or terminal
// statuses (completed, failed, cancelled) are considered inactive.
func isWorkspaceActive(wsPath string) bool {
	cp, err := core.LoadCheckpoint(wsPath)
	if err != nil {
		return false // workspace gone or corrupt — safe to clean
	}
	return cp.Status == "in_progress"
}

// runClaimsCleanup purges file_claims rows older than 7 days from the registry.
// This removes both released and abandoned (unreleased) claims from crashed
// or long-finished missions.
func runClaimsCleanup() error {
	cdb, err := claims.OpenDB("")
	if err != nil {
		return fmt.Errorf("open claims db: %w", err)
	}
	defer cdb.Close()

	maxAge := 7 * 24 * time.Hour
	if dryRun {
		fmt.Println("dry run: would purge file claims older than 7 days")
		return nil
	}

	n, err := cdb.PurgeStaleClaims(maxAge)
	if err != nil {
		return fmt.Errorf("purge claims: %w", err)
	}

	if n == 0 {
		fmt.Println("no stale claims to clean up")
	} else {
		fmt.Printf("%d stale claim(s) removed\n", n)
	}
	return nil
}

// runEmptyTrash permanently deletes trash entries older than 24h.
func runEmptyTrash() error {
	base, err := config.Dir()
	if err != nil {
		return fmt.Errorf("get config dir: %w", err)
	}

	trashDir := filepath.Join(base, "trash")
	entries, err := os.ReadDir(trashDir)
	if err != nil {
		if os.IsNotExist(err) {
			fmt.Println("trash is empty")
			return nil
		}
		return fmt.Errorf("reading trash dir: %w", err)
	}

	cutoff := time.Now().Add(-24 * time.Hour)
	removed := 0

	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		info, err := e.Info()
		if err != nil {
			continue
		}
		if info.ModTime().After(cutoff) {
			if verbose {
				fmt.Printf("skipping recent trash entry: %s\n", e.Name())
			}
			continue
		}

		entryPath := filepath.Join(trashDir, e.Name())
		if dryRun {
			fmt.Printf("would permanently delete: %s\n", e.Name())
		} else {
			if err := os.RemoveAll(entryPath); err != nil {
				fmt.Printf("warning: could not delete %s: %v\n", e.Name(), err)
				continue
			}
			fmt.Printf("permanently deleted: %s\n", e.Name())
		}
		removed++
	}

	if removed == 0 {
		fmt.Println("no trash entries old enough to delete (older than 24h)")
	} else if dryRun {
		fmt.Printf("%d trash entries would be permanently deleted\n", removed)
	} else {
		fmt.Printf("%d trash entries permanently deleted\n", removed)
	}

	return nil
}

// runRestoreWorktree moves a trashed worktree back to its original location
// and re-registers it with git.
func runRestoreWorktree(workspaceID string) error {
	base, err := config.Dir()
	if err != nil {
		return fmt.Errorf("get config dir: %w", err)
	}

	trashDir := filepath.Join(base, "trash")
	entries, err := os.ReadDir(trashDir)
	if err != nil {
		if os.IsNotExist(err) {
			return fmt.Errorf("no trash entries found (trash dir does not exist)")
		}
		return fmt.Errorf("reading trash dir: %w", err)
	}

	// Find all trash entries for this workspace ID (there may be multiple if
	// the same workspace was cleaned up more than once).
	var matches []string
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		if strings.HasPrefix(e.Name(), workspaceID+"_") || e.Name() == workspaceID {
			matches = append(matches, filepath.Join(trashDir, e.Name()))
		}
	}

	switch len(matches) {
	case 0:
		return fmt.Errorf("no trash entry found for workspace %q", workspaceID)
	case 1:
		// exactly one match — proceed
	default:
		// Multiple entries: use the most recent (last alphabetically, since
		// trash entries are named <id>_<YYYYMMDDTHHMMSSZ>).
		fmt.Printf("found %d trash entries for %s, restoring the most recent\n", len(matches), workspaceID)
		matches = []string{matches[len(matches)-1]}
	}

	trashEntry := matches[0]

	// Load trash metadata.
	metaPath := filepath.Join(trashEntry, ".nanika-trash-meta.json")
	metaData, err := os.ReadFile(metaPath)
	if err != nil {
		return fmt.Errorf("read trash metadata from %s: %w", trashEntry, err)
	}
	var meta git.TrashMeta
	if err := json.Unmarshal(metaData, &meta); err != nil {
		return fmt.Errorf("parse trash metadata: %w", err)
	}

	// Validate the original path is not already occupied.
	if _, err := os.Stat(meta.OriginalPath); err == nil {
		return fmt.Errorf("original path already exists: %s", meta.OriginalPath)
	}

	// Ensure the parent directory exists.
	if err := os.MkdirAll(filepath.Dir(meta.OriginalPath), 0755); err != nil {
		return fmt.Errorf("create parent dir: %w", err)
	}

	if dryRun {
		fmt.Printf("would restore %s → %s\n", trashEntry, meta.OriginalPath)
		fmt.Printf("  branch: %s\n", meta.Branch)
		fmt.Printf("  repo:   %s\n", meta.RepoRoot)
		return nil
	}

	// Move from trash back to original location.
	if err := os.Rename(trashEntry, meta.OriginalPath); err != nil {
		return fmt.Errorf("restore worktree: %w", err)
	}

	// Re-register the worktree with git.  After the rename the .git file
	// inside the worktree directory references the main repo's worktrees
	// metadata dir, but that entry was pruned during soft-delete.
	// `git worktree repair` recreates it.
	if err := git.RepairWorktree(meta.RepoRoot, meta.OriginalPath); err != nil {
		// Non-fatal: the files are back, but the git link may be broken.
		fmt.Printf("warning: could not re-register worktree with git: %v\n", err)
		fmt.Printf("run manually: git -C %s worktree repair %s\n", meta.RepoRoot, meta.OriginalPath)
	}

	fmt.Printf("restored worktree: %s\n", meta.OriginalPath)
	if meta.Branch != "" {
		fmt.Printf("  branch: %s\n", meta.Branch)
	}
	return nil
}
