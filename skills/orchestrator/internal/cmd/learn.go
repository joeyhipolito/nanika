package cmd

import (
	"context"
	"fmt"
	"os"
	"time"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
)

// Pricing for gemini-embedding-001 at the time of writing — see
// design.md (~/.alluka/artifacts/embedding-backfill/design.md). Unit-tested
// indirectly via the dry-run output asserts.
const (
	embeddingPricePerMTokens   = 0.15
	embeddingCharsPerTokenEst  = 4.0
	embeddingMaxContentChars   = 8000 // ~2k tokens, the per-input cap of gemini-embedding-001
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

	backfillCmd := &cobra.Command{
		Use:   "backfill-embeddings",
		Short: "Fill missing embeddings on existing learnings (dry-run by default)",
		Long: `Walks rows where embedding IS NULL and calls the Gemini embedding API to fill them.
Default is a dry-run that prints row count + cost estimate. Pass --apply to write.`,
		RunE: runBackfillEmbeddings,
	}
	backfillCmd.Flags().Bool("apply", false, "actually call the API and write embeddings (default is dry-run)")
	backfillCmd.Flags().Duration("since", 0, "only consider rows created within this window (e.g. 720h); 0 = all rows")
	backfillCmd.Flags().Int("limit", 0, "process at most N rows (0 = no cap)")
	backfillCmd.Flags().Int("batch-size", 100, "rows per batchEmbedContents call (capped at 100)")
	backfillCmd.Flags().Int("rpm", 60, "target requests per minute (0 = no throttle)")
	backfillCmd.Flags().Int("max-retries", 5, "per-batch retry ceiling on 429 / 5xx")
	backfillCmd.Flags().String("db", "", "")
	_ = backfillCmd.Flags().MarkHidden("db")
	backfillCmd.Flags().Bool("include-archived", false, "include archived rows (default skips them)")
	backfillCmd.Flags().Bool("quiet", false, "suppress per-batch progress lines")

	rootCmd.AddCommand(statsCmd)
	rootCmd.AddCommand(pruneCmd)
	rootCmd.AddCommand(backfillCmd)
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

func runBackfillEmbeddings(cmd *cobra.Command, args []string) error {
	apply, _ := cmd.Flags().GetBool("apply")
	since, _ := cmd.Flags().GetDuration("since")
	limit, _ := cmd.Flags().GetInt("limit")
	batchSize, _ := cmd.Flags().GetInt("batch-size")
	rpm, _ := cmd.Flags().GetInt("rpm")
	maxRetries, _ := cmd.Flags().GetInt("max-retries")
	dbPath, _ := cmd.Flags().GetString("db")
	includeArchived, _ := cmd.Flags().GetBool("include-archived")
	quiet, _ := cmd.Flags().GetBool("quiet")

	if batchSize <= 0 || batchSize > 100 {
		fmt.Fprintf(os.Stderr, "invalid --batch-size %d (must be 1..100)\n", batchSize)
		os.Exit(2)
	}
	if since < 0 {
		fmt.Fprintf(os.Stderr, "invalid --since %s (must be >= 0)\n", since)
		os.Exit(2)
	}

	ctx := cmd.Context()
	if ctx == nil {
		ctx = context.Background()
	}

	db, err := learning.OpenDB(dbPath)
	if err != nil {
		return fmt.Errorf("open learning DB: %w", err)
	}
	defer db.Close()

	stats, err := db.CountEmbeddingBackfill(ctx, since, includeArchived)
	if err != nil {
		return err
	}

	candidateRows := stats.Rows
	if limit > 0 && limit < candidateRows {
		candidateRows = limit
	}
	estTokens := int(float64(stats.TotalChars) / embeddingCharsPerTokenEst)
	if limit > 0 && stats.Rows > 0 && limit < stats.Rows {
		// scale tokens to the capped row count (assumes uniform content length)
		estTokens = estTokens * limit / stats.Rows
	}
	estUSD := float64(estTokens) * embeddingPricePerMTokens / 1_000_000.0
	batches := (candidateRows + batchSize - 1) / batchSize
	wallSecs := 0
	if rpm > 0 {
		wallSecs = (batches * 60) / rpm
	}

	if !apply {
		fmt.Printf("embed-backfill (dry-run):\n")
		fmt.Printf("  candidate rows: %d\n", candidateRows)
		fmt.Printf("  est. tokens:    %d (chars/4 heuristic)\n", estTokens)
		fmt.Printf("  est. cost:      $%.4f USD @ $%.2f per 1M tokens\n", estUSD, embeddingPricePerMTokens)
		fmt.Printf("  batches:        %d × %d (estimated wall time: ~%ds at %d rpm)\n",
			batches, batchSize, wallSecs, rpm)
		fmt.Printf("  run with --apply to write embeddings\n")
		return nil
	}

	apiKey := learning.LoadAPIKey()
	if apiKey == "" {
		fmt.Fprintln(os.Stderr, "embed-backfill: GEMINI_API_KEY not set (and no key in ~/.alluka/config or ~/.obsidian/config)")
		os.Exit(3)
	}
	embedder := learning.NewEmbedder(apiKey)
	if embedder == nil {
		return fmt.Errorf("failed to construct embedder despite non-empty key")
	}

	var logf func(string, ...any)
	if !quiet {
		logf = func(format string, args ...any) {
			fmt.Printf(format+"\n", args...)
		}
	}

	res, err := learning.BackfillEmbeddings(ctx, db, embedder.EmbedBatch, learning.BackfillOptions{
		MaxAge:          since,
		IncludeArchived: includeArchived,
		Limit:           limit,
		BatchSize:       batchSize,
		RPM:             rpm,
		MaxRetries:      maxRetries,
		MaxContentChars: embeddingMaxContentChars,
		Logf:            logf,
	})
	if err != nil {
		return fmt.Errorf("backfill: %w", err)
	}

	fmt.Printf("embed-backfill: processed %d / embedded %d / already %d / failed %d in %s\n",
		res.Processed, res.Embedded, res.AlreadyFilled, len(res.Failed), res.Elapsed.Round(time.Millisecond))

	if len(res.Failed) > 0 {
		for _, id := range res.Failed {
			fmt.Fprintf(os.Stderr, "embed-backfill: failed %s\n", id)
		}
		os.Exit(4)
	}
	return nil
}
