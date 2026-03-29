package cmd

import (
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/claims"
	"github.com/joeyhipolito/orchestrator-cli/internal/config"
	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/git"
)

var (
	cleanWorktrees bool
	cleanClaims    bool
)

func init() {
	cleanupCmd := &cobra.Command{
		Use:   "cleanup",
		Short: "Remove old workspaces (older than 7 days)",
		RunE:  runCleanup,
	}

	cleanupCmd.Flags().BoolVar(&cleanWorktrees, "worktrees", false, "remove orphaned worktrees from ~/.alluka/worktrees/")
	cleanupCmd.Flags().BoolVar(&cleanClaims, "claims", false, "purge file claims older than 7 days from the registry")

	rootCmd.AddCommand(cleanupCmd)
}

func runCleanup(cmd *cobra.Command, args []string) error {
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
			if err := git.RemoveWorktree(worktreePath); err != nil {
				// Graceful removal failed (repo may be gone); fall back to direct delete.
				os.RemoveAll(worktreePath)
			}
			fmt.Printf("removed worktree: %s\n", e.Name())
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
