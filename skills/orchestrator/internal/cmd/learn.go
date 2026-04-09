package cmd

import (
	"context"
	"fmt"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
)

func init() {
	statsCmd := &cobra.Command{
		Use:   "stats",
		Short: "Show learning database statistics",
		RunE:  showStats,
	}

	pruneCmd := &cobra.Command{
		Use:   "prune",
		Short: "Prune old and low-quality learnings from the database",
		Long:  "Removes learnings based on age, quality score, and domain count caps.\nDry-run by default — use --apply to actually delete.",
		RunE:  runPrune,
	}
	pruneCmd.Flags().Bool("apply", false, "actually delete (default is dry-run)")
	pruneCmd.Flags().Int("max-age", 180, "max age in days for unused low-quality learnings")
	pruneCmd.Flags().Float64("min-score", 0.1, "delete learnings below this quality score")
	pruneCmd.Flags().Int("max-count", 500, "max learnings per domain")

	rootCmd.AddCommand(statsCmd)
	rootCmd.AddCommand(pruneCmd)
}

func showStats(cmd *cobra.Command, args []string) error {
	db, err := learning.OpenDB("")
	if err != nil {
		return fmt.Errorf("open learning DB: %w", err)
	}
	defer db.Close()

	total, withEmb, err := db.Stats()
	if err != nil {
		return err
	}

	totalC, injected, avgRate, err := db.ComplianceStats()
	if err != nil {
		return fmt.Errorf("compliance stats: %w", err)
	}
	_ = totalC // same as total from Stats()

	fmt.Printf("learnings: %d total, %d with embeddings\n", total, withEmb)

	if injected > 0 {
		fmt.Printf("compliance:  %d injected, avg rate %.0f%%\n", injected, avgRate*100)
	} else {
		fmt.Printf("compliance:  no injections recorded yet\n")
	}
	return nil
}

func runPrune(cmd *cobra.Command, args []string) error {
	apply, _ := cmd.Flags().GetBool("apply")
	maxAge, _ := cmd.Flags().GetInt("max-age")
	minScore, _ := cmd.Flags().GetFloat64("min-score")
	maxCount, _ := cmd.Flags().GetInt("max-count")

	db, err := learning.OpenDB("")
	if err != nil {
		return fmt.Errorf("open learning DB: %w", err)
	}
	defer db.Close()

	opts := learning.CleanupOptions{
		MaxAgeDays:   maxAge,
		MinScore:     minScore,
		MaxPerDomain: maxCount,
		DryRun:       !apply,
	}

	n, err := db.Cleanup(context.Background(), opts)
	if err != nil {
		return fmt.Errorf("cleanup: %w", err)
	}

	if opts.DryRun {
		fmt.Printf("dry-run: would remove %d learnings (use --apply to delete)\n", n)
	} else {
		fmt.Printf("removed %d learnings\n", n)
	}
	return nil
}
