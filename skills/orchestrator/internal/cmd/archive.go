package cmd

import (
	"fmt"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
)

func init() {
	archiveCmd := &cobra.Command{
		Use:   "archive",
		Short: "Archive dead-weight learnings (dry-run by default)",
		Long: `Identifies learnings that are no longer useful and marks them archived.

Four criteria are applied:
  1. Never injected, older than 90 days
  2. Chronic non-compliance: injected >= 5 times, compliance_rate < 0.10
  3. Low quality (< 0.2), never used, older than 60 days
  4. Single observation, no embedding, older than 30 days

Dry-run by default — pass --apply to write changes.`,
		RunE: runArchive,
	}
	archiveCmd.Flags().Bool("apply", false, "actually archive (default is dry-run)")
	archiveCmd.Flags().String("domain", "", "restrict to a specific domain (default: all domains)")

	rootCmd.AddCommand(archiveCmd)
}

func runArchive(cmd *cobra.Command, args []string) error {
	apply, _ := cmd.Flags().GetBool("apply")
	dom, _ := cmd.Flags().GetString("domain")

	db, err := learning.OpenDB("")
	if err != nil {
		return fmt.Errorf("open learning DB: %w", err)
	}
	defer db.Close()

	opts := learning.ArchiveOptions{
		DryRun: !apply,
		Domain: dom,
	}

	candidates, err := db.ArchiveDeadWeight(cmd.Context(), opts)
	if err != nil {
		return fmt.Errorf("archive: %w", err)
	}

	if len(candidates) == 0 {
		fmt.Println("no dead-weight learnings found")
		return nil
	}

	for _, c := range candidates {
		if opts.DryRun {
			fmt.Printf("would archive %s: %s\n", c.ID, c.Reason)
		} else {
			fmt.Printf("archived %s: %s\n", c.ID, c.Reason)
		}
	}

	if opts.DryRun {
		fmt.Printf("\ndry-run: %d learnings would be archived (use --apply to write)\n", len(candidates))
	} else {
		fmt.Printf("\narchived %d learnings\n", len(candidates))
	}
	return nil
}
