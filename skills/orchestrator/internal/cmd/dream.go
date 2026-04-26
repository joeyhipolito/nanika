package cmd

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/config"
	"github.com/joeyhipolito/orchestrator-cli/internal/dream"
	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
)

func init() {
	dreamCmd := &cobra.Command{
		Use:   "dream",
		Short: "Mine Claude Code session transcripts for durable learnings",
		Long: `Dream walks ~/.claude/projects/ JSONL transcripts, chunks multi-turn
conversations into token-bounded windows, and extracts decisions, insights,
patterns, and gotchas into the learnings database using Haiku.

Three dedup layers prevent flooding the DB:
  1. File-level sha256: skip unchanged transcripts entirely.
  2. Chunk-level sha256: skip re-extraction for identical conversation windows.
  3. Cosine similarity at 0.85 inside learning.DB.Insert: final semantic gate.

Worker sessions (cwd inside ~/.alluka/worktrees/) are excluded because their
output is already captured live by CaptureWithFocus during phase execution.`,
	}

	runCmd := &cobra.Command{
		Use:   "run",
		Short: "Run one dream extraction cycle",
		Long: `Walk transcripts, chunk conversations, call Haiku, store learnings.
Dry-run by default when the global --dry-run flag is set.`,
		RunE: runDream,
	}
	runCmd.Flags().String("since", "", "only process transcripts modified after this time (RFC3339 or duration like 24h)")
	runCmd.Flags().String("session", "", "filter to transcripts whose path contains this string")
	runCmd.Flags().Bool("force", false, "reprocess already-processed transcripts (ignores file hash cache)")
	runCmd.Flags().Int("limit", 0, "max files to process this run (0 = use default of 20)")

	statusCmd := &cobra.Command{
		Use:   "status",
		Short: "Show dream processing statistics",
		RunE:  runDreamStatus,
	}

	resetCmd := &cobra.Command{
		Use:   "reset",
		Short: "Clear all dream processing state (processed_transcripts + processed_chunks)",
		Long:  "Clears processing state so the next run re-mines all transcripts.\nDoes not remove any learnings that were already stored.",
		RunE:  runDreamReset,
	}

	workerCmd := &cobra.Command{
		Use:   "worker",
		Short: "Consolidate persistent worker memories into the learnings database",
		Long: `Walk ~/.alluka/workers/*/memory.md, parse structured entries,
convert to learnings with heuristic quality scoring, and store via DB.Insert
with cosine dedup. No LLM calls — worker memories are already structured.

Worker memory files are read-only: this command never modifies memory.md.

Three dedup layers prevent duplicates:
  1. File-level sha256: skip unchanged memory files entirely.
  2. Entry-level content hash: skip already-processed entries.
  3. Cosine similarity at 0.85 inside learning.DB.Insert: semantic gate.`,
		RunE: runDreamWorker,
	}
	workerCmd.Flags().String("worker", "", "only process this worker's memory (substring match on name)")
	workerCmd.Flags().Bool("force", false, "reprocess already-processed memory files (ignores file hash cache)")

	dreamCmd.AddCommand(runCmd, statusCmd, resetCmd, workerCmd)
	rootCmd.AddCommand(dreamCmd)
}

func runDream(cmd *cobra.Command, args []string) error {
	sinceStr, _ := cmd.Flags().GetString("since")
	sessionFilter, _ := cmd.Flags().GetString("session")
	force, _ := cmd.Flags().GetBool("force")
	limit, _ := cmd.Flags().GetInt("limit")

	// Parse --since: accept RFC3339 timestamp or a duration like "24h".
	var since time.Time
	if sinceStr != "" {
		if t, err := time.Parse(time.RFC3339, sinceStr); err == nil {
			since = t
		} else if d, err := time.ParseDuration(sinceStr); err == nil {
			since = time.Now().Add(-d)
		} else {
			return fmt.Errorf("--since: cannot parse %q (want RFC3339 or duration like 24h)", sinceStr)
		}
	}

	dbPath, err := learningsDBPath()
	if err != nil {
		return err
	}

	store, err := dream.OpenSQLiteStore(dbPath)
	if err != nil {
		return fmt.Errorf("dream store: %w", err)
	}
	defer store.Close()

	ldb, err := learning.OpenDB("")
	if err != nil {
		return fmt.Errorf("learning DB: %w", err)
	}
	defer ldb.Close()

	embedder := learning.NewEmbedder(learning.LoadAPIKey())

	cfg := dream.DefaultConfig()
	cfg.DryRun = dryRun // global --dry-run flag from root.go
	cfg.Verbose = verbose // global --verbose flag from root.go
	cfg.Force = force
	cfg.Since = since
	cfg.SessionFilter = sessionFilter
	if limit > 0 {
		cfg.Limit = limit
	}

	runner := dream.NewRunner(store, ldb, embedder, cfg)

	ctx, cancel := context.WithTimeout(cmd.Context(), 10*time.Minute)
	defer cancel()

	report, err := runner.Run(ctx, domain, "")
	if err != nil {
		return fmt.Errorf("dream run: %w", err)
	}

	printDreamReport(report)
	return nil
}

func runDreamStatus(cmd *cobra.Command, args []string) error {
	dbPath, err := learningsDBPath()
	if err != nil {
		return err
	}

	store, err := dream.OpenSQLiteStore(dbPath)
	if err != nil {
		return fmt.Errorf("dream store: %w", err)
	}
	defer store.Close()

	files, chunks, err := store.Status()
	if err != nil {
		return fmt.Errorf("status: %w", err)
	}

	fmt.Printf("processed transcripts: %d\n", files)
	fmt.Printf("processed chunks:      %d\n", chunks)
	return nil
}

func runDreamReset(cmd *cobra.Command, args []string) error {
	dbPath, err := learningsDBPath()
	if err != nil {
		return err
	}

	store, err := dream.OpenSQLiteStore(dbPath)
	if err != nil {
		return fmt.Errorf("dream store: %w", err)
	}
	defer store.Close()

	if err := store.Reset(); err != nil {
		return fmt.Errorf("reset: %w", err)
	}

	fmt.Println("dream: processing state cleared (learnings are preserved)")
	return nil
}

// learningsDBPath returns the path to learnings.db in the config directory.
func learningsDBPath() (string, error) {
	base, err := config.Dir()
	if err != nil {
		return "", fmt.Errorf("config dir: %w", err)
	}
	return filepath.Join(base, "learnings.db"), nil
}

func runDreamWorker(cmd *cobra.Command, args []string) error {
	workerFilter, _ := cmd.Flags().GetString("worker")
	force, _ := cmd.Flags().GetBool("force")

	dbPath, err := learningsDBPath()
	if err != nil {
		return err
	}

	store, err := dream.OpenSQLiteStore(dbPath)
	if err != nil {
		return fmt.Errorf("dream store: %w", err)
	}
	defer store.Close()

	ldb, err := learning.OpenDB("")
	if err != nil {
		return fmt.Errorf("learning DB: %w", err)
	}
	defer ldb.Close()

	embedder := learning.NewEmbedder(learning.LoadAPIKey())

	cfg := dream.DefaultConfig()
	cfg.DryRun = dryRun
	cfg.Verbose = verbose
	cfg.Force = force
	cfg.SessionFilter = workerFilter

	runner := dream.NewWorkerRunner(store, ldb, embedder, cfg)

	ctx, cancel := context.WithTimeout(cmd.Context(), 5*time.Minute)
	defer cancel()

	report, err := runner.Run(ctx, domain)
	if err != nil {
		return fmt.Errorf("dream worker: %w", err)
	}

	printWorkerReport(report)
	return nil
}

// printWorkerReport emits a one-line summary plus any errors to stderr.
func printWorkerReport(r dream.WorkerReport) {
	fmt.Printf(
		"dream worker: discovered=%d skipped=%d processed=%d entries=%d eligible=%d stored=%d rejected=%d duration=%s\n",
		r.Discovered, r.SkippedFile, r.ProcessedFiles,
		r.EntriesParsed, r.EntriesParsed-r.EntriesSkipped,
		r.LearningsStored, r.LearningsRejected,
		r.Duration.Round(time.Millisecond),
	)
	if len(r.Errors) > 0 {
		fmt.Fprintf(os.Stderr, "dream worker: %d error(s):\n", len(r.Errors))
		for _, e := range r.Errors {
			fmt.Fprintf(os.Stderr, "  [%s] %s: %s\n", e.Phase, e.Path, e.Err)
		}
	}
}

// printDreamReport emits a one-line summary plus any errors to stderr.
func printDreamReport(r dream.Report) {
	fmt.Printf(
		"dream: discovered=%d skipped=%d processed=%d chunks=%d llm-calls=%d stored=%d rejected=%d duration=%s\n",
		r.Discovered, r.SkippedFile, r.ProcessedFiles,
		r.ChunksEmitted, r.LLMCalls,
		r.LearningsStored, r.LearningsRejected,
		r.Duration.Round(time.Millisecond),
	)
	if len(r.Errors) > 0 {
		fmt.Fprintf(os.Stderr, "dream: %d error(s):\n", len(r.Errors))
		for _, e := range r.Errors {
			fmt.Fprintf(os.Stderr, "  [%s] %s: %s\n", e.Phase, e.Path, e.Err)
		}
	}
}
